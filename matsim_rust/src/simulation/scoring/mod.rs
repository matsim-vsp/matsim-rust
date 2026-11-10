use crate::simulation::config::{ActivityParameter, ModeParameter};
use crate::simulation::framework_events::{
    ControllerEvent, ControllerEventsManager, ControllerListenerRegisterFn, QSimId,
    WorkerListenerRegisterFunction,
};
use crate::simulation::io::xml::config::{IOConfig, scoring_parameter_sets};
use crate::simulation::scenario::ControllerScenario;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scoring::backpacking::backpacking_engine::{
    BackpackingEngine, BackpackingWorkerResult,
};
use ahash::HashMapExt;
use nohash_hasher::IntMap;
use rayon::iter::ParallelIterator;
use rayon::prelude::IntoParallelRefMutIterator;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use tracing::info;

pub mod backpacking;
mod charypar_nagel_scoring_function;
mod only_travel_time_dependent_scoring;
pub mod partial_plans;
mod plan_scorer;
mod score_assigner;

use crate::simulation::id::Id;
use crate::simulation::scoring::backpacking::backpack::PersonExperience;
pub use charypar_nagel_scoring_function::CharyparNagelScoringFunction;
pub use only_travel_time_dependent_scoring::OnlyTravelTimeDependentScoring;
pub use plan_scorer::PlanScorer;
pub(crate) use score_assigner::ScoreAssigner;

pub type WorkerListenerRegistrations = IntMap<QSimId, Vec<Box<WorkerListenerRegisterFunction>>>;

pub(crate) type PersonExperiences = IntMap<Id<InternalPerson>, PersonExperience>;

/// The scoring parameters of one subpopulation, like MATSim's `scoringParameters` parameter set. The set without
/// subpopulation is the default set. It applies to the persons whose subpopulation has no set of its own, like
/// MATSim's `getScoringParametersOrDefault`. Activity and mode parameters are not merged with those of the default
/// set.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct ScoringParameterSet {
    pub subpopulation: Option<String>,
    pub late_arrival: f64,              // utils/hour
    pub early_departure: f64,           // utils/hour
    pub performing: f64,                // utils/hour
    pub waiting: f64,                   // utils/hour
    pub marginal_utility_of_money: f64, // utils/money
    pub aborted_plan_score: f64,        // utils/hour
    /// The marginal utility of waiting for a transit vehicle, in utils/hour. Without a value, it
    /// is the marginal utility of traveling by `pt`, so that waiting costs as much as riding, like
    /// MATSim's `waitingPt`.
    pub waiting_pt: Option<f64>, // utils/hour
    pub utility_of_line_switch: f64,    // utils
    pub activity_params: Vec<ActivityParameter>,
    pub mode_params: Vec<ModeParameter>,
}

impl ScoringParameterSet {
    /// Returns "subpopulation <name>" or "the default subpopulation", for error messages.
    pub fn describe_subpopulation(&self) -> String {
        match &self.subpopulation {
            Some(subpopulation) => format!("subpopulation {subpopulation}"),
            None => "the default subpopulation".to_string(),
        }
    }
}

impl Default for ScoringParameterSet {
    fn default() -> Self {
        Self {
            subpopulation: None,
            late_arrival: -18.0,
            early_departure: -0.0,
            performing: 6.0,
            waiting: -0.0,
            marginal_utility_of_money: 1.0,
            aborted_plan_score: -18.0,
            waiting_pt: None,
            utility_of_line_switch: -1.0,
            activity_params: vec![
                ActivityParameter::default_for_activity_type("home"),
                ActivityParameter::default_for_activity_type("work"),
                ActivityParameter::default_for_activity_type("leisure"),
                ActivityParameter::default_for_activity_type("shop"),
                ActivityParameter::default_for_activity_type("errands"),
            ],
            mode_params: vec![
                ModeParameter::default_for_mode("car"),
                ModeParameter::default_for_mode("walk"),
                ModeParameter::default_for_mode("ride"),
                ModeParameter::default_for_mode("freight"),
                ModeParameter::default_for_mode("bike"),
            ],
        }
    }
}

/// Reads the scoring parameter sets from a YAML file with a list of sets, or from the scoring module of a MATSim
/// config (`.xml`, `.xml.gz` or `.xml.zst`), see [`scoring_parameter_sets`].
pub fn read_scoring_parameter_sets(path: &Path) -> Vec<ScoringParameterSet> {
    info!("Loading scoring parameters from {:?}", path);
    if is_matsim_config(path) {
        return scoring_parameter_sets(&IOConfig::from_file(path)).unwrap_or_else(|e| {
            panic!(
                "Failed to read the scoring parameters from the MATSim config at {:?}: {}",
                path, e
            )
        });
    }
    let file = File::open(path).unwrap_or_else(|e| {
        panic!(
            "Failed to open scoring parameters file at {:?}. Original error was {}",
            path, e
        )
    });
    serde_yaml::from_reader(BufReader::new(file)).unwrap_or_else(|e| {
        panic!(
            "Failed to parse scoring parameters file at {:?}. Original error was: {}",
            path, e
        )
    })
}

fn is_matsim_config(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    [".xml", ".xml.gz", ".xml.zst"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

/// Writes the scoring parameter sets as a YAML list, which `read_scoring_parameter_sets` reads.
pub fn write_scoring_parameter_sets(sets: &[ScoringParameterSet], path: &Path) {
    let file = File::create(path).unwrap_or_else(|e| {
        panic!(
            "Failed to create scoring parameters file at {:?}. Original error was {}",
            path, e
        )
    });
    serde_yaml::to_writer(BufWriter::new(file), sets).unwrap_or_else(|e| {
        panic!(
            "Failed to write scoring parameters file at {:?}. Original error was {}",
            path, e
        )
    });
}

/// Rejects several sets for the same subpopulation, including several default sets, activity types or modes that
/// occur more than once within a set, and opening or closing times that are negative, not finite, or out of order.
pub fn validate_scoring_parameter_sets(sets: &[ScoringParameterSet]) -> Result<(), String> {
    let mut subpopulations = BTreeSet::new();
    for set in sets {
        if !subpopulations.insert(set.subpopulation.as_deref()) {
            return Err(format!(
                "There are several scoring parameter sets for {}.",
                set.describe_subpopulation()
            ));
        }
        let mut activity_types = BTreeSet::new();
        if let Some(params) = set
            .activity_params
            .iter()
            .find(|params| !activity_types.insert(params.activity_type.as_str()))
        {
            return Err(format!(
                "Activity type {} occurs several times in the scoring parameters for {}.",
                params.activity_type,
                set.describe_subpopulation()
            ));
        }
        for params in &set.activity_params {
            validate_opening_interval(params).map_err(|e| {
                format!(
                    "Activity type {} in the scoring parameters for {}: {e}",
                    params.activity_type,
                    set.describe_subpopulation()
                )
            })?;
        }
        let mut modes = BTreeSet::new();
        if let Some(params) = set
            .mode_params
            .iter()
            .find(|params| !modes.insert(params.mode.as_str()))
        {
            return Err(format!(
                "Mode {} occurs several times in the scoring parameters for {}.",
                params.mode,
                set.describe_subpopulation()
            ));
        }
    }
    Ok(())
}

/// MATSim's activity scoring assumes that an activity opens no later than it closes, without checking it.
fn validate_opening_interval(params: &ActivityParameter) -> Result<(), String> {
    for (name, time) in [
        ("opening time", params.opening_time_s),
        ("closing time", params.closing_time_s),
    ] {
        if let Some(time) = time
            && (!time.is_finite() || time < 0.0)
        {
            return Err(format!(
                "{name} must be finite and not negative, got {time}."
            ));
        }
    }
    if let (Some(opening), Some(closing)) = (params.opening_time_s, params.closing_time_s)
        && opening > closing
    {
        return Err(format!(
            "opening time {opening} s lies after closing time {closing} s."
        ));
    }
    Ok(())
}

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
        let partition = scenario.core.network.get_link(activity.link_id()).partition;
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
        let transit_schedule = scenario.transit_sim_schedule.clone();
        let network = scenario.core.network.clone();
        worker_registrations.entry(rank).or_default().push(Box::new(
            move |events, mobsim_events, _partition_events, migration_extensions| {
                let engine = Rc::new(RefCell::new(BackpackingEngine::new(
                    home_person_ids,
                    transit_schedule,
                    network,
                )));
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
/// Experienced plans receive the raw score; `assigner` assigns it to the selected plans.
pub(crate) fn score_population(
    experiences: &mut [PersonExperiences],
    population: &mut Population,
    plan_scorer: &dyn PlanScorer,
    iteration: u32,
    assigner: &ScoreAssigner,
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
        let selected_plan = population
            .persons
            .get_mut(&person_id)
            .unwrap()
            .selected_plan_mut();
        assigner.assign(iteration, selected_plan, score);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ScoringParameterSet, read_scoring_parameter_sets, validate_scoring_parameter_sets,
        write_scoring_parameter_sets,
    };
    use crate::simulation::config::{ActivityParameter, ActivityScoring, ModeParameter};

    #[test]
    fn scoring_parameter_sets_roundtrip_through_file() {
        let yaml = r#"
        - performing: 6.88
          activity_params: []
          mode_params: []
        - subpopulation: freight
          late_arrival: -12.0
          early_departure: -6.0
          performing: 4.0
          waiting: -3.0
          marginal_utility_of_money: 2.0
          aborted_plan_score: -24.0
          waiting_pt: -9.0
          utility_of_line_switch: -0.5
          activity_params:
            - activity_type: home
              typical_duration_s: 43200.0
          mode_params:
            - mode: car
              marginal_utility_of_traveling: -0.001
              marginal_utility_of_distance: -0.002
              monetary_distance_cost_rate: -0.003
              constant: 1.5
              daily_money_constant: -2.5
              daily_utility_constant: 3.5
        "#;

        let sets: Vec<ScoringParameterSet> =
            serde_yaml::from_str(yaml).expect("failed to parse scoring parameter sets");
        let expected = vec![
            ScoringParameterSet {
                performing: 6.88,
                activity_params: Vec::new(),
                mode_params: Vec::new(),
                ..ScoringParameterSet::default()
            },
            ScoringParameterSet {
                subpopulation: Some("freight".to_string()),
                late_arrival: -12.0,
                early_departure: -6.0,
                performing: 4.0,
                waiting: -3.0,
                marginal_utility_of_money: 2.0,
                aborted_plan_score: -24.0,
                waiting_pt: Some(-9.0),
                utility_of_line_switch: -0.5,
                activity_params: vec![ActivityParameter {
                    activity_type: "home".to_string(),
                    typical_duration_s: 43_200.0,
                    activity_scoring: ActivityScoring::ConsiderFullActivity,
                    opening_time_s: None,
                    closing_time_s: None,
                }],
                mode_params: vec![ModeParameter {
                    mode: "car".to_string(),
                    marginal_utility_of_traveling: -0.001,
                    marginal_utility_of_distance: -0.002,
                    monetary_distance_cost_rate: -0.003,
                    constant: 1.5,
                    daily_money_constant: -2.5,
                    daily_utility_constant: 3.5,
                }],
            },
        ];
        assert_eq!(sets, expected);

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("scoring-parameters.yml");
        write_scoring_parameter_sets(&sets, &path);
        assert_eq!(read_scoring_parameter_sets(&path), expected);
    }

    #[test]
    fn scoring_parameter_set_uses_defaults_for_missing_lists_and_agent_fields() {
        let sets: Vec<ScoringParameterSet> =
            serde_yaml::from_str("- {}").expect("failed to parse scoring parameter sets");

        assert_eq!(sets, vec![ScoringParameterSet::default()]);
    }

    #[test]
    fn scoring_validation_rejects_duplicate_subpopulations_and_modes() {
        let mut sets = vec![
            ScoringParameterSet::default(),
            ScoringParameterSet::default(),
        ];
        assert_eq!(
            validate_scoring_parameter_sets(&sets),
            Err("There are several scoring parameter sets for the default subpopulation.".into())
        );

        sets.pop();
        sets[0]
            .mode_params
            .push(ModeParameter::default_for_mode("car"));
        assert_eq!(
            validate_scoring_parameter_sets(&sets),
            Err(
                "Mode car occurs several times in the scoring parameters for the default subpopulation."
                    .into()
            )
        );

        sets[0].mode_params.pop();
        sets[0].activity_params[0].opening_time_s = Some(20.0 * 3_600.0);
        sets[0].activity_params[0].closing_time_s = Some(8.0 * 3_600.0);
        assert_eq!(
            validate_scoring_parameter_sets(&sets),
            Err(
                "Activity type home in the scoring parameters for the default subpopulation: opening time 72000 s lies after closing time 28800 s."
                    .into()
            )
        );
    }
}
