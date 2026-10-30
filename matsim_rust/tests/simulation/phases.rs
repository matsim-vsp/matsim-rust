use macros::deterministic_id_test;
use matsim_rust::simulation::Identifiable;
use matsim_rust::simulation::agents::{
    ActivityLogic, LegAgent, LegLogic, NetworkDriverAgent, NextPhase, SimulationAgentLogic,
    TeleportationAgent, TeleportationLogic, TransitTeleportation,
};
use matsim_rust::simulation::config::{CommandLineArgs, Config, WriteEvents};
use matsim_rust::simulation::controller::ThreadLocalComputationalEnvironment;
use matsim_rust::simulation::controller::controller::ControllerBuilder;
use matsim_rust::simulation::framework_events::WorkerListenerRegisterFunction;
use matsim_rust::simulation::id::Id;
use matsim_rust::simulation::population::agent_source::{AgentSet, AgentSource};
use matsim_rust::simulation::scenario::network::Link;
use matsim_rust::simulation::scenario::population::InternalPerson;
use matsim_rust::simulation::scenario::{
    Coordinate, MobsimScenarioPartition, PopulationShard, Scenario,
};
use matsim_rust::simulation::time::SimTime;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
enum Phase {
    /// An activity that ends at the given second.
    Activity(u64),
    /// A teleported leg with the given travel time in seconds.
    Walk(u64),
    Final,
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

fn agent_events(phases: Vec<Phase>, test_name: &str) -> Vec<(u64, String)> {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/3-links/3-links-config-1.yml",
    ));
    config.qsim_mut().start_time = 0;
    config.qsim_mut().end_time = 100;
    config.output_mut().write_events = WriteEvents::None;
    config.output_mut().output_dir = PathBuf::from(format!("./test_output/phases/{test_name}"));
    let events = Arc::new(Mutex::new(Vec::new()));
    let captured = events.clone();
    let register: Box<WorkerListenerRegisterFunction> = Box::new(move |manager, _, _, _| {
        manager.on_any(move |event| {
            captured
                .lock()
                .unwrap()
                .push((event.time().as_secs(), event.type_().to_owned()));
        });
    });
    let scenario = Scenario::load(config);
    ControllerBuilder::default_with_scenario(scenario)
        .agent_source(PhaseChainSource { phases })
        .worker_listener_register_fn(HashMap::from([(0, vec![register])]))
        .build()
        .unwrap()
        .run();
    Arc::try_unwrap(events).unwrap().into_inner().unwrap()
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
