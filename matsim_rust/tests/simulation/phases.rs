use macros::deterministic_id_test;
use matsim_rust::external_services::routing::{InternalRoutingRequest, InternalRoutingResponse};
use matsim_rust::external_services::{
    AdapterHandleBuilder, AsyncExecutor, ExternalServiceType, RequestAdapter, RequestAdapterFactory,
};
use matsim_rust::simulation::Identifiable;
use matsim_rust::simulation::agents::{
    ActivityLogic, LegAgent, LegLogic, NetworkDriverAgent, NextPhase, SimulationAgentLogic,
    StuckLogic, TeleportationAgent, TeleportationLogic, TransitTeleportation,
};
use matsim_rust::simulation::config::{CommandLineArgs, Config, RoutingMode, WriteEvents};
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::controller::{RequestSender, ThreadLocalComputationalEnvironment};
use matsim_rust::simulation::framework_events::WorkerListenerRegisterFunction;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::population::agent_source::{
    AgentSet, AgentSource, PreplanningHorizonAgentSource,
};
use matsim_rust::simulation::scenario::network::Link;
use matsim_rust::simulation::scenario::population::{
    InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan, InternalPlanElement,
    InternalRoute, PREPLANNING_HORIZON,
};
use matsim_rust::simulation::scenario::{
    Coordinate, MobsimScenarioPartition, PopulationShard, Scenario,
};
use matsim_rust::simulation::scoring::PlanScorer;
use matsim_rust::simulation::time::SimTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
enum Phase {
    /// An activity that ends at the given second.
    Activity(u64),
    /// A teleported leg with the given travel time in seconds.
    Walk(u64),
    Final,
    Stuck,
}

/// A planless agent that goes through a fixed sequence of phases. The simulation must carry out
/// whatever an agent chooses, not only the alternation of a plan.
struct PhaseChain {
    person: InternalPerson,
    phases: Vec<Phase>,
    current: usize,
    act_type: Id<String>,
    mode: Id<String>,
    link: Id<Link>,
    coord: Coordinate,
}

impl PhaseChain {
    fn next_phase(mut self: Box<Self>) -> NextPhase {
        self.current += 1;
        match self.phases[self.current] {
            Phase::Activity(_) | Phase::Final => NextPhase::Activity(self),
            Phase::Walk(_) => NextPhase::Leg(self),
            Phase::Stuck => NextPhase::Stuck(self),
        }
    }
}

impl Identifiable<InternalPerson> for PhaseChain {
    fn id(&self) -> &Id<InternalPerson> {
        self.person.id()
    }
}

impl SimulationAgentLogic for PhaseChain {
    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        Some(self.person)
    }
}

impl ActivityLogic for PhaseChain {
    fn act_type(&self) -> &Id<String> {
        &self.act_type
    }
    fn link_id(&self) -> &Id<Link> {
        &self.link
    }
    fn coord(&self) -> &Coordinate {
        &self.coord
    }
    fn has_next_leg(&self) -> bool {
        !matches!(self.phases[self.current], Phase::Final)
    }
    fn end_time(&self, _activity_start: SimTime) -> SimTime {
        match self.phases[self.current] {
            Phase::Activity(end) => SimTime::from_secs(end),
            _ => SimTime::from_nanos(u64::MAX),
        }
    }
    fn wakeup_time(&self, now: SimTime) -> SimTime {
        self.end_time(now)
    }
    fn notify_started(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _now: SimTime,
    ) {
    }
    fn notify_wakeup(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _end_time: SimTime,
        _now: SimTime,
    ) {
    }
    fn notify_finished(&mut self, _now: SimTime) {}
    fn finish_activity(self: Box<Self>, _now: SimTime) -> NextPhase {
        self.next_phase()
    }
}

impl LegLogic for PhaseChain {
    fn mode(&self) -> &Id<String> {
        &self.mode
    }
    fn routing_mode(&self) -> Option<&Id<String>> {
        Some(&self.mode)
    }
    fn departure_link(&self) -> &Id<Link> {
        &self.link
    }
    fn curr_link_id(&self) -> Option<&Id<Link>> {
        Some(&self.link)
    }
    fn try_into_network(self: Box<Self>) -> Result<NetworkDriverAgent, LegAgent> {
        Err(self)
    }
    fn try_into_teleportation(self: Box<Self>) -> Result<TeleportationAgent, LegAgent> {
        Ok(self)
    }
    fn finish_leg(self: Box<Self>, _now: SimTime) -> NextPhase {
        self.next_phase()
    }
}

impl TeleportationLogic for PhaseChain {
    fn destination_link(&self) -> &Id<Link> {
        &self.link
    }
    fn arrival_time(&self, departure: SimTime) -> SimTime {
        let Phase::Walk(travel_time) = self.phases[self.current] else {
            panic!("Only a walk leg is teleported");
        };
        departure.saturating_add(Duration::from_secs(travel_time))
    }
    fn distance(&self) -> Option<f64> {
        Some(0.0)
    }
    fn transit(&self) -> Option<TransitTeleportation<'_>> {
        None
    }
    fn notify_begin_teleportation(&mut self, _now: SimTime) {}
}

impl StuckLogic for PhaseChain {
    fn stuck_link(&self) -> Option<&Id<Link>> {
        None
    }
    fn stuck_mode(&self) -> Option<&Id<String>> {
        None
    }
}

struct PhaseChainSource {
    phases: Vec<Phase>,
}

impl AgentSource for PhaseChainSource {
    fn create_agents(
        &self,
        population: PopulationShard,
        _partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        population
            .population
            .persons
            .into_iter()
            .map(|(id, person)| {
                let first = person.selected_plan().unwrap().elements[0]
                    .as_activity()
                    .unwrap();
                let chain = PhaseChain {
                    act_type: first.act_type.clone(),
                    link: first.link_id().clone(),
                    coord: first.coord().clone(),
                    mode: Id::create("walk"),
                    phases: self.phases.clone(),
                    current: 0,
                    person,
                };
                (id, Box::new(chain) as _)
            })
            .collect()
    }
}

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

/// Records the number of legs of every experienced plan.
#[derive(Clone, Default)]
struct LegCountingScorer(Arc<Mutex<Vec<usize>>>);

impl PlanScorer for LegCountingScorer {
    fn score(
        &self,
        _person_id: &Id<InternalPerson>,
        _subpopulation: &str,
        experienced_plan: &InternalPlan,
    ) -> Result<f64, String> {
        self.0.lock().unwrap().push(experienced_plan.legs().len());
        Ok(0.0)
    }
}

/// Creates adaptive agents and records the selected plan they start each iteration with.
#[derive(Clone, Default)]
struct PlanRecordingSource(Arc<Mutex<Vec<Vec<InternalPlanElement>>>>);

impl AgentSource for PlanRecordingSource {
    fn create_agents(
        &self,
        population: PopulationShard,
        partition: &MobsimScenarioPartition,
    ) -> AgentSet {
        self.0.lock().unwrap().extend(
            population
                .population
                .persons
                .values()
                .map(|person| person.selected_plan().unwrap().elements.clone()),
        );
        PreplanningHorizonAgentSource.create_agents(population, partition)
    }
}

/// Answers every routing request with the same leg.
struct OneLegRouter(InternalLeg);

impl RequestAdapterFactory<InternalRoutingRequest> for OneLegRouter {
    async fn build(self) -> impl RequestAdapter<InternalRoutingRequest> {
        self
    }
}

impl RequestAdapter<InternalRoutingRequest> for OneLegRouter {
    fn on_request(&mut self, req: InternalRoutingRequest) {
        let leg = InternalPlanElement::Leg(self.0.clone());
        req.response_tx
            .send(InternalRoutingResponse::new(vec![leg], req.payload.uuid))
            .unwrap();
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

fn agent_events(phases: Vec<Phase>, test_name: &str) -> Vec<(u64, String)> {
    let config = load_config(
        "./tests/resources/3-links/3-links-config-1.yml",
        test_name,
        0,
        100,
    );
    let events = Events::default();
    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .agent_source(PhaseChainSource { phases })
        .worker_listener_register_fn(register_event_capture(&events, 1))
        .build()
        .unwrap()
        .run();
    Arc::try_unwrap(events).unwrap().into_inner().unwrap()
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
fn an_agent_may_follow_an_activity_with_an_activity_and_a_leg_with_a_leg() {
    let events = agent_events(
        vec![
            Phase::Activity(10),
            Phase::Activity(12),
            Phase::Walk(5),
            Phase::Walk(0),
            Phase::Final,
        ],
        "chained_phases",
    );

    assert_eq!(
        events,
        owned(vec![
            (10, "actend"),
            // The activity engine has already run in tick 10, so the next activity starts in 11.
            (11, "actstart"),
            (12, "actend"),
            (12, "departure"),
            (17, "travelled"),
            (17, "arrival"),
            // Every leg end costs a tick, whatever follows.
            (18, "departure"),
            (18, "travelled"),
            (18, "arrival"),
            (19, "actstart"),
        ])
    );
}

#[deterministic_id_test(matsim_rust)]
fn an_agent_can_choose_to_get_stuck() {
    let events = agent_events(
        vec![Phase::Activity(10), Phase::Stuck],
        "stuck_after_activity",
    );

    assert_eq!(events, owned(vec![(10, "actend"), (10, "stuckAndAbort")]));
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

#[deterministic_id_test(matsim_rust)]
fn plan_changes_do_not_outlive_the_iteration() {
    let mut config = load_config(
        "./tests/resources/3-links/3-links-config-1.yml",
        "plan_changes",
        0,
        32_700,
    );
    config.routing_mut().mode = RoutingMode::AdHoc;
    config.controller_mut().last_iteration = 1;

    let barrier = Arc::new(Barrier::new((config.partitioning().num_parts + 1) as usize));
    let executor = AsyncExecutor::from_config(&config, barrier.clone());

    let mut scenario = Scenario::load(config);
    for person in scenario.population.persons.values_mut() {
        person.plans_mut()[0].acts_mut()[0]
            .attributes
            .insert(PREPLANNING_HORIZON, 600);
    }

    // The planned trip has three legs. The router replaces it with a single one.
    let route = InternalGenericRoute::new(
        Id::get_from_ext("link1"),
        Id::get_from_ext("link3"),
        Some(Duration::from_secs(60)),
        Some(1200.0),
        None,
    );
    let leg = InternalLeg::new(
        InternalRoute::Generic(route),
        "walk",
        "walk",
        Duration::from_secs(60),
        None,
    );
    let (handle, send, shutdown) = executor.spawn_thread("routing_adapter", OneLegRouter(leg));
    let mut services: HashMap<ExternalServiceType, RequestSender> = HashMap::new();
    services.insert(
        ExternalServiceType::Routing("car".to_string()),
        Arc::new(send).into(),
    );

    let plans = PlanRecordingSource::default();
    let scorer = LegCountingScorer::default();
    ControllerBuilder::default_with_scenario(scenario)
        .agent_source(plans.clone())
        .scoring_function(Box::new(scorer.clone()))
        .external_services(services.into())
        .adapter_handles(vec![
            AdapterHandleBuilder::default()
                .handle(handle)
                .shutdown_sender(shutdown)
                .build()
                .unwrap(),
        ])
        .global_barrier(barrier)
        .build()
        .unwrap()
        .run();

    // The agent took the replaced trip in both iterations ...
    assert_eq!(*scorer.0.lock().unwrap(), vec![1, 1]);
    // ... but it started both from the unchanged plan.
    let plans = plans.0.lock().unwrap();
    assert_eq!(plans.len(), 2);
    assert_eq!(plans[0].len(), 7);
    assert_eq!(plans[0], plans[1]);
}
