use macros::deterministic_id_test;
use matsim_rust::simulation::config::{CommandLineArgs, Config, WriteEvents};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::framework_events::WorkerListenerRegisterFunction;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::scenario::Scenario;
use matsim_rust::simulation::scenario::network::Link;
use matsim_rust::simulation::scenario::population::{
    InternalGenericRoute, InternalPerson, InternalPlan, InternalPlanElement, InternalRoute,
};
use matsim_rust::simulation::scoring::PlanScorer;
use matsim_rust::simulation::time::SimTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

/// The events with the rank of the partition that emitted them.
type Events = Arc<Mutex<Vec<(u32, u64, String)>>>;

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
                        captured.lock().unwrap().push((
                            rank,
                            event.time().as_secs(),
                            event.type_().to_owned(),
                        ));
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
    let (events, aborted) = plan_events_by_rank(config_path, ranks, test_name, edit_plan);
    let events = events
        .into_iter()
        .map(|(_, time, kind)| (time, kind))
        .collect();
    (events, aborted)
}

/// Like `plan_events`, but every event comes with the rank of the partition that emitted it.
fn plan_events_by_rank(
    config_path: &str,
    ranks: u32,
    test_name: &str,
    edit_plan: impl Fn(&mut InternalPlan),
) -> (Vec<(u32, u64, String)>, Vec<bool>) {
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
    events.sort_by_key(|(_, time, _)| *time);
    let aborted = scorer.0.lock().unwrap().clone();
    (events, aborted)
}

fn drop_last_activity(plan: &mut InternalPlan) {
    plan.elements.pop();
}

/// Lets the last walk leg go back to `link1` without travel time, and moves the last activity
/// there. With two partitions, `link1` and `link3` are on different partitions.
fn walk_back_without_travel_time(plan: &mut InternalPlan) {
    let link1 = Id::<Link>::get_from_ext("link1");
    let [
        ..,
        InternalPlanElement::Leg(leg),
        InternalPlanElement::Activity(activity),
    ] = plan.elements.as_mut_slice()
    else {
        panic!("The plan ends with a leg and an activity");
    };
    let route = leg.route.as_ref().unwrap().as_generic();
    let route = InternalGenericRoute::new(
        route.start_link().clone(),
        link1.clone(),
        Some(Duration::ZERO),
        route.distance(),
        None,
    );
    leg.trav_time = Some(Duration::ZERO);
    leg.route = Some(InternalRoute::Generic(route));
    activity.link_id = Some(link1);
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

    // The agent gets stuck in the tick of its arrival.
    assert_eq!(
        &events[events.len() - 3..],
        owned(vec![
            (32537, "travelled"),
            (32537, "arrival"),
            (32537, "stuckAndAbort"),
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

    assert_eq!(events.last().unwrap(), &(32537, "stuckAndAbort".to_owned()));
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

    assert_eq!(events.last().unwrap(), &(32537, "actstart".to_owned()));
    assert_eq!(aborted, vec![false]);
}

#[deterministic_id_test(matsim_rust)]
fn a_leg_without_duration_after_a_leg_end_takes_one_tick_on_every_partitioning() {
    // The car leg ends at 32521. The walk leg departs in the same tick, after the exchange of
    // messages, so it ends in the next tick, also on the other partition.
    let expected = owned(vec![
        (32521, "arrival"),
        (32521, "actstart"),
        (32521, "actend"),
        (32521, "departure"),
        (32522, "travelled"),
        (32522, "arrival"),
        (32522, "actstart"),
    ]);
    let agent_events = |events: Vec<(u32, u64, String)>| -> Vec<(u32, u64, String)> {
        let kinds = expected.iter().map(|(_, kind)| kind).collect::<Vec<_>>();
        events
            .into_iter()
            .filter(|(_, _, kind)| kinds.contains(&kind))
            .collect()
    };
    let without_rank = |events: &[(u32, u64, String)]| -> Vec<(u64, String)> {
        events
            .iter()
            .map(|(_, time, kind)| (*time, kind.clone()))
            .collect()
    };

    let (one_part, aborted) = plan_events_by_rank(
        "./tests/resources/3-links/3-links-config-1.yml",
        1,
        "leg_without_duration_after_leg_end",
        walk_back_without_travel_time,
    );
    let one_part = agent_events(one_part);
    assert_eq!(&without_rank(&one_part)[one_part.len() - 7..], expected);
    assert_eq!(aborted, vec![false]);

    let (two_parts, aborted) = plan_events_by_rank(
        "./tests/resources/3-links/3-links-config-2.yml",
        2,
        "leg_without_duration_after_leg_end_two_parts",
        walk_back_without_travel_time,
    );
    let two_parts = agent_events(two_parts);
    let tail = &two_parts[two_parts.len() - 7..];
    assert_eq!(without_rank(tail), expected);
    assert_ne!(tail[3].0, tail[4].0, "The walk leg changes the partition");
    assert_eq!(aborted, vec![false]);
}
