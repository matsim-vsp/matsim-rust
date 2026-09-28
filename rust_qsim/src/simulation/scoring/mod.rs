use crate::simulation::framework_events::{
    ControllerEvent, ControllerEventsManager, ControllerListenerRegisterFn, QSimId,
    WorkerListenerRegisterFunction,
};
use crate::simulation::scenario::ControllerScenario;
use crate::simulation::scenario::population::Population;
use crate::simulation::scoring::backpacking::backpacking_engine::{
    BackpackingEngine, BackpackingWorkerResult,
};
use crate::simulation::{config, io};
use nohash_hasher::IntMap;
use std::cell::RefCell;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::mpsc;
use tracing::info;

pub mod backpacking;
pub mod partial_plans;

pub type WorkerListenerRegistrations = HashMap<QSimId, Vec<Box<WorkerListenerRegisterFunction>>>;

/// Creates the complete backpacking setup for the configured number of partitions.
///
/// The worker registrations collect experienced plans locally. The controller registration
/// synchronizes all collectors after mobsim, merges their results, and writes configured output.
pub(crate) fn crate_registrations(
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

    let (result_sender, result_receiver) = mpsc::channel::<BackpackingWorkerResult>();

    let mut worker_registrations = WorkerListenerRegistrations::new();
    for rank in 0..num_parts {
        let home_person_ids = std::mem::take(&mut home_person_ids[rank as usize]);
        let result_sender = result_sender.clone();
        worker_registrations.entry(rank).or_default().push(Box::new(
            move |events, mobsim_events, _partition_events, migration_extensions| {
                let engine = Rc::new(RefCell::new(BackpackingEngine::new(home_person_ids)));
                BackpackingEngine::register(
                    engine,
                    events,
                    mobsim_events,
                    migration_extensions,
                    result_sender,
                    rank,
                );
            },
        ));
    }

    let config = scenario.core.config.clone();
    let output_path = io::resolve_path(config.context(), &config.output().output_dir);
    let controller_registration = Box::new(move |events: &mut ControllerEventsManager| {
        events.on_event(move |event| match &event.payload {
            ControllerEvent::AfterMobsim(controller_event) => {
                let mut populations_by_rank: IntMap<QSimId, Population> = IntMap::default();
                for _ in 0..num_parts {
                    let result = result_receiver.recv().unwrap_or_else(|error| {
                        panic!(
                            "Failed to receive backpacking result for iteration {}: {error}",
                            event.meta.iteration
                        )
                    });
                    assert_eq!(
                        result.iteration, event.meta.iteration,
                        "Received backpacking result for iteration {}, expected {}.",
                        result.iteration, event.meta.iteration
                    );
                    assert!(
                        result.rank < num_parts,
                        "Received backpacking result from invalid rank {}.",
                        result.rank
                    );
                    let previous = populations_by_rank.insert(result.rank, result.population);
                    assert!(
                        previous.is_none(),
                        "Received duplicate backpacking result from rank {} in iteration {}.",
                        result.rank,
                        event.meta.iteration
                    );
                }
                let populations = (0..num_parts)
                    .map(|rank| {
                        populations_by_rank.remove(&rank).unwrap_or_else(|| {
                            panic!(
                                "Missing backpacking result from rank {rank} in iteration {}.",
                                event.meta.iteration
                            )
                        })
                    })
                    .collect();
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
