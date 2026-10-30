use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config, WriteEvents};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::framework_events::WorkerListenerRegisterFunction;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::population::{
    InternalPerson, InternalPlan, InternalPlanElement,
};
use matsim_rust::simulation::scoring::PlanScorer;
use matsim_rust::simulation::time::SimTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Records for every experienced plan whether its last element is marked as aborted.
#[derive(Clone, Default)]
struct RecordingScorer(Arc<Mutex<Vec<bool>>>);

impl PlanScorer for RecordingScorer {
    fn score(
        &self,
        _person_id: &Id<InternalPerson>,
        _subpopulation: &str,
        experienced_plan: &InternalPlan,
    ) -> Result<f64, String> {
        let attributes = match experienced_plan.elements.last().unwrap() {
            InternalPlanElement::Activity(activity) => &activity.attributes,
            InternalPlanElement::Leg(leg) => &leg.attributes,
        };
        self.0
            .lock()
            .unwrap()
            .push(attributes.get::<bool>("aborted") == Some(true));
        Ok(0.0)
    }
}

type Events = Arc<Mutex<Vec<(u64, String)>>>;

fn register_event_capture(
    events: &Events,
    ranks: u32,
) -> HashMap<u32, Vec<Box<WorkerListenerRegisterFunction>>> {
    (0..ranks)
        .map(|rank| {
            let captured = events.clone();
            let register: Box<WorkerListenerRegisterFunction> =
                Box::new(move |manager, _, _, _| {
                    manager.on_any(move |event| {
                        captured
                            .lock()
                            .unwrap()
                            .push((event.time().as_secs(), event.type_().to_owned()));
                    });
                });
            (rank, vec![register])
        })
        .collect()
}

fn load_config(path: &str, test_name: &str, start_time: u32, end_time: u32) -> Config {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(path));
    config.qsim_mut().start_time = start_time;
    config.qsim_mut().end_time = end_time;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = PathBuf::from(format!("./test_output/phases/{test_name}"));
    config
}

/// Runs the plan of the 3-links scenario after `edit_plan` and returns all events by time, together
/// with whether the experienced plans were aborted.
fn plan_events(
    config_path: &str,
    ranks: u32,
    test_name: &str,
    edit_plan: impl Fn(&mut InternalPlan),
) -> (Vec<(u64, String)>, Vec<bool>) {
    let config = load_config(config_path, test_name, 32_400, 32_700);
    let events = Events::default();
    let scorer = RecordingScorer::default();
    let mut scenario = Scenario::load(config);
    for person in scenario.population.persons.values_mut() {
        edit_plan(&mut person.plans_mut()[0]);
    }
    ControllerBuilder::default_with_scenario(scenario)
        .scoring_function(Box::new(scorer.clone()))
        .worker_listener_register_fn(register_event_capture(&events, ranks))
        .build()
        .unwrap()
        .run();
    let mut events = Arc::try_unwrap(events).unwrap().into_inner().unwrap();
    events.sort_by_key(|(time, _)| *time);
    let aborted = scorer.0.lock().unwrap().clone();
    (events, aborted)
}

fn drop_last_activity(plan: &mut InternalPlan) {
    plan.elements.pop();
}

fn owned(events: Vec<(u64, &str)>) -> Vec<(u64, String)> {
    events
        .into_iter()
        .map(|(time, kind)| (time, kind.to_owned()))
        .collect()
}

#[deterministic_id_test(matsim_rust)]
fn an_agent_whose_plan_ends_with_a_leg_gets_stuck_once() {
    let (events, aborted) = plan_events(
        "./tests/resources/3-links/3-links-config-1.yml",
        1,
        "plan_ends_with_a_leg",
        drop_last_activity,
    );

    // A leg end costs a tick, so the agent gets stuck after its arrival.
    assert_eq!(
        &events[events.len() - 3..],
        owned(vec![
            (32538, "travelled"),
            (32538, "arrival"),
            (32539, "stuckAndAbort"),
        ])
    );
    assert_eq!(
        events.iter().filter(|(_, e)| e == "stuckAndAbort").count(),
        1
    );
    assert_eq!(aborted, vec![true]);
}

#[deterministic_id_test(matsim_rust)]
fn a_stuck_agent_on_another_partition_is_materialized_and_scored() {
    let (events, aborted) = plan_events(
        "./tests/resources/3-links/3-links-config-2.yml",
        2,
        "plan_ends_with_a_leg_two_parts",
        drop_last_activity,
    );

    assert_eq!(events.last().unwrap(), &(32539, "stuckAndAbort".to_owned()));
    assert_eq!(
        events.iter().filter(|(_, e)| e == "stuckAndAbort").count(),
        1
    );
    assert_eq!(aborted, vec![true]);
}

#[deterministic_id_test(matsim_rust)]
fn the_last_activity_never_ends() {
    let (events, aborted) = plan_events(
        "./tests/resources/3-links/3-links-config-1.yml",
        1,
        "last_activity_with_end_time",
        |plan| {
            let Some(InternalPlanElement::Activity(activity)) = plan.elements.last_mut() else {
                panic!("The plan ends with an activity");
            };
            activity.end_time = Some(SimTime::from_secs(32_600));
        },
    );

    assert_eq!(events.last().unwrap(), &(32539, "actstart".to_owned()));
    assert_eq!(aborted, vec![false]);
}
