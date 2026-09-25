use crate::simulation::framework_events::{
    ControllerEvent, ControllerEventsManager, ControllerListenerRegisterFn, QSimId,
    WorkerListenerRegisterFunction,
};
use crate::simulation::scenario::ControllerScenario;
use crate::simulation::scenario::population::Population;
use crate::simulation::scoring::backpacking::backpacking_scoring_engine::BackpackingScoringEngine;
use crate::simulation::{config, io};
use std::any::Any;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Sender};
use std::thread;
use tracing::info;

pub mod backpacking;
pub mod partial_plans;

pub trait Message: Any + Send {
    fn as_any(&self) -> &dyn Any;

    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Any + Send> Message for T {
    fn as_any(&self) -> &dyn Any {
        self
    }

    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

pub struct InternalScoringMessage {
    pub(crate) from_process: QSimId,
    #[allow(unused)]
    pub(crate) to_process: QSimId,
    pub(crate) message: Box<dyn Message>,
}

/// Trait for a scoring engine that can be initialized and finished by the controller.
pub trait ScoringEngine: Send + Sync {
    /// Attaches the senders to the internal structs managing message handling.
    fn attach_senders(&mut self, senders: Vec<Sender<InternalScoringMessage>>);

    /// Returns the register functions, given to the Partitions
    fn register_fn(&self) -> Box<WorkerListenerRegisterFunction>;

    /// Called from the Controller after the mobsim is finished. Shall finish remaining tasks,
    /// that can only be done after the iteration end.
    fn finish(&self) -> Population;

    /// Actual scoring.
    fn scoring(&self);
}

pub type WorkerListenerRegistrations = HashMap<QSimId, Vec<Box<WorkerListenerRegisterFunction>>>;

/// Creates the complete backpacking setup for the configured number of partitions.
///
/// The worker registrations collect experienced plans locally. The controller registration
/// synchronizes all collectors after mobsim, merges their results, and writes configured output.
pub fn create_for_n_partitions(
    scenario: &ControllerScenario,
) -> (
    WorkerListenerRegistrations,
    Box<ControllerListenerRegisterFn>,
) {
    let num_parts = scenario.core.config.partitioning().num_parts;
    let mut home_person_ids = vec![Vec::new(); num_parts as usize];

    for (person_id, person) in &scenario.population.persons {
        let activity = person
            .plan_element_at(0)
            .and_then(|element| element.as_activity())
            .unwrap_or_else(|| {
                panic!(
                    "Person {} does not have an initial activity for backpacking partition assignment.",
                    person_id.external()
                )
            });
        let partition = scenario.core.network.get_link(&activity.link_id).partition;
        assert!(
            partition < num_parts,
            "Person {} starts in partition {}, but only {} partitions exist.",
            person_id.external(),
            partition,
            num_parts
        );
        home_person_ids[partition as usize].push(person_id.clone());
    }
    for ids in &mut home_person_ids {
        ids.sort();
    }

    let mut senders = Vec::with_capacity(num_parts as usize);
    let mut receivers = Vec::with_capacity(num_parts as usize);
    for _ in 0..num_parts {
        let (sender, receiver) = mpsc::channel();
        senders.push(sender);
        receivers.push(Some(receiver));
    }

    let mut engines = Vec::with_capacity(num_parts as usize);
    let mut worker_registrations = WorkerListenerRegistrations::new();
    for rank in 0..num_parts {
        let engine = BackpackingScoringEngine::new(
            rank,
            std::mem::take(&mut home_person_ids[rank as usize]),
            receivers[rank as usize]
                .take()
                .expect("Each backpacking partition must have one receiver"),
            senders.clone(),
        );
        worker_registrations
            .entry(rank)
            .or_default()
            .push(engine.register_fn());
        engines.push(engine);
    }

    let engines = Arc::new(engines);
    let config = scenario.core.config.clone();
    let output_path = io::resolve_path(config.context(), &config.output().output_dir);
    let controller_registration = Box::new(move |events: &mut ControllerEventsManager| {
        events.on_event(move |event| match &event.payload {
            ControllerEvent::AfterMobsim(controller_event) => {
                let populations = thread::scope(|scope| {
                    let handles: Vec<_> = engines
                        .iter()
                        .map(|engine| scope.spawn(move || engine.finish()))
                        .collect();
                    handles
                        .into_iter()
                        .map(|handle| {
                            handle
                                .join()
                                .expect("Backpacking scoring engine failed while finishing")
                        })
                        .collect()
                });
                let population = merge_partition_populations(populations);

                if config
                    .controller()
                    .should_write_plans(event.meta.iteration, controller_event.last_iteration)
                {
                    write_experienced_population(
                        &population,
                        &config,
                        &output_path,
                        event.meta.iteration,
                        controller_event.last_iteration,
                    );
                }
            }
            ControllerEvent::Scoring(_) => {
                for engine in engines.iter() {
                    engine.scoring();
                }
            }
            _ => {}
        });
    });

    (worker_registrations, controller_registration)
}

fn merge_partition_populations(populations: Vec<Population>) -> Population {
    let mut persons: Vec<_> = populations
        .into_iter()
        .flat_map(|population| population.persons)
        .collect();
    persons.sort_by(|(left, _), (right, _)| left.cmp(right));

    let mut merged = Population::new();
    for (person_id, person) in persons {
        let previous = merged.persons.insert(person_id.clone(), person);
        assert!(
            previous.is_none(),
            "Person {} was returned by more than one backpacking partition.",
            person_id.external()
        );
    }
    merged
}

fn write_experienced_population(
    population: &Population,
    config: &config::Config,
    output_path: &Path,
    iteration: u32,
    is_last_iteration: bool,
) {
    let filename = config
        .controller()
        .compression_type
        .with_extension("output_experienced_plans");
    let iteration_path = output_path
        .join("ITERS")
        .join(format!("it.{iteration}"))
        .join(&filename);
    info!("Writing experienced plans to {}", iteration_path.display());
    population.to_file(&iteration_path);

    if is_last_iteration {
        let root_path = output_path.join(filename);
        info!("Writing experienced plans to {}", root_path.display());
        population.to_file(&root_path);
    }
}
