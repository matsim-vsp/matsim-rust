use crate::simulation::framework_events::{
    ControllerEvent, ControllerEventsManager, ControllerListenerRegisterFn, QSimId,
    WorkerListenerRegisterFunction,
};
use crate::simulation::scenario::ControllerScenario;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scoring::backpacking::backpacking_engine::{
    BackpackingEngine, BackpackingWorkerResult,
};
use ahash::HashMapExt;
use nohash_hasher::IntMap;
use rayon::iter::ParallelIterator;
use rayon::prelude::IntoParallelRefMutIterator;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};

pub mod backpacking;
mod charypar_nagel_scoring_function;
mod only_travel_time_dependent_scoring;
pub mod partial_plans;
mod plan_scorer;

use crate::simulation::id::Id;
use crate::simulation::scoring::backpacking::backpack::PersonExperience;
pub use charypar_nagel_scoring_function::CharyparNagelScoringFunction;
pub use only_travel_time_dependent_scoring::OnlyTravelTimeDependentScoring;
pub use plan_scorer::PlanScorer;

pub type WorkerListenerRegistrations = IntMap<QSimId, Vec<Box<WorkerListenerRegisterFunction>>>;

pub(crate) type PersonExperiences = IntMap<Id<InternalPerson>, PersonExperience>;

struct ExperiencedPlansResult {
    iteration: u32,
    plans: Vec<PersonExperiences>,
}

#[derive(Clone, Default)]
pub(crate) struct ExperiencedPlansCollection {
    result: Arc<Mutex<Option<ExperiencedPlansResult>>>,
}

impl ExperiencedPlansCollection {
    fn store(&self, iteration: u32, plans: Vec<PersonExperiences>) {
        let mut result = self.result.lock().unwrap();
        assert!(
            result.is_none(),
            "Previous experienced-plan result was not consumed before iteration {iteration}."
        );
        *result = Some(ExperiencedPlansResult { iteration, plans });
    }

    pub(crate) fn take(&self, iteration: u32) -> Vec<PersonExperiences> {
        let result = self
            .result
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| panic!("No experienced-plan result for iteration {iteration}."));
        assert_eq!(
            result.iteration, iteration,
            "Experienced-plan result belongs to iteration {}, expected {iteration}.",
            result.iteration
        );
        result.plans
    }
}

/// Creates the complete backpacking setup for the configured number of partitions.
///
/// The worker registrations collect experienced plans locally. The controller registration
/// synchronizes all collectors after mobsim, and stores their deterministically merged result.
pub(crate) fn create_registrations(
    scenario: &ControllerScenario,
) -> (
    WorkerListenerRegistrations,
    Box<ControllerListenerRegisterFn>,
    ExperiencedPlansCollection,
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

    let experienced_plans = ExperiencedPlansCollection::default();
    let callback_experienced_plans = experienced_plans.clone();
    let controller_registration = Box::new(move |events: &mut ControllerEventsManager| {
        events.on_event(move |event| match &event.payload {
            ControllerEvent::AfterMobsim(_) => {
                let mut populations_by_rank: IntMap<
                    QSimId,
                    IntMap<Id<InternalPerson>, PersonExperience>,
                > = IntMap::default();
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
                    let previous =
                        populations_by_rank.insert(result.rank, result.experienced_plans);
                    assert!(
                        previous.is_none(),
                        "Received duplicate backpacking result from rank {} in iteration {}.",
                        result.rank,
                        event.meta.iteration
                    );
                }
                let plans = (0..num_parts)
                    .map(|rank| {
                        populations_by_rank.remove(&rank).unwrap_or_else(|| {
                            panic!(
                                "Missing backpacking result from rank {rank} in iteration {}.",
                                event.meta.iteration
                            )
                        })
                    })
                    .collect();
                callback_experienced_plans.store(event.meta.iteration, plans);
            }
            _ => {}
        });
    });

    (
        worker_registrations,
        controller_registration,
        experienced_plans,
    )
}

/// Performs multithreaded scoring of the population. Rayon pool is started in the controller.
pub(crate) fn score_population(
    experiences: &mut Vec<PersonExperiences>,
    population: &mut Population,
    plan_scorer: &dyn PlanScorer,
) {
    let scores: Vec<_> = experiences
        .par_iter_mut()
        .flat_map_iter(|experience| experience.iter_mut())
        .map(|(person_id, experience)| {
            let person = population.persons.get(person_id).unwrap();

            let score = plan_scorer
                .score(
                    person_id,
                    person.subpopulation().external(),
                    experience.plan(),
                )
                .unwrap_or_else(|error| panic!("{error}"));

            experience.plan_mut().score = Some(score);

            (person_id.clone(), score)
        })
        .collect();

    // setting the person scores in a separate loop to avoid mutable borrow issues
    for (person_id, score) in scores {
        population
            .persons
            .get_mut(&person_id)
            .unwrap()
            .selected_plan_mut()
            .score = Some(score);
    }
}
