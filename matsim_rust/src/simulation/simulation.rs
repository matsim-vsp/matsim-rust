use crate::simulation::Identifiable;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{SimulationAgentLogic, SimulationAgentState};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::activity_engine::{ActivityEngine, ActivityEngineBuilder};
use crate::simulation::engines::leg_engine::LegEngine;
use crate::simulation::engines::timetable_transit_engine::TimetableTransitEngine;
use crate::simulation::engines::transit_engine::TransitEngine;
use crate::simulation::events::PersonStuckEventBuilder;
use crate::simulation::framework_events::MobsimEvent;
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker;
use crate::simulation::population::agent_source::DynAgentSource;
use crate::simulation::scenario::{MobsimInput, MobsimScenarioPartition};
use crate::simulation::time::{SimClock, SimTime, Tick};
use std::fmt::Debug;
use std::fmt::Formatter;
use tracing::info;

pub struct Simulation<C: SimCommunicator> {
    activity_engine: ActivityEngine,
    leg_engine: LegEngine<C>,
    comp_env: ThreadLocalComputationalEnvironment,
    start_tick: Tick,
    end_tick: Tick,
    clock: SimClock,
    pending_leg_agents: Vec<(Tick, SimulationAgent)>,
}

impl<C> Simulation<C>
where
    C: SimCommunicator,
{
    #[tracing::instrument(level = "info", skip(self), fields(rank = self.leg_engine.net_message_broker().rank()))]
    pub fn run(&mut self) -> Vec<SimulationAgent> {
        // use fixed start and end times
        let mut now = self.start_tick;
        let start_time = self.clock.tick_to_secs(self.start_tick);
        let end_time = self.clock.tick_to_secs(self.end_tick);
        info!(
            "Starting #{}. Network neighbors: {:?}, Start time {}, End time {}",
            self.leg_engine.net_message_broker().rank(),
            self.leg_engine.network().neighbors(),
            start_time,
            end_time,
        );

        let mut agents_changing_engine = vec![];

        while now <= self.end_tick {
            let now_time = self.clock.tick_to_time(now);
            let outward_now = self.clock.tick_to_secs(now);
            self.comp_env
                .mobsim_events_manager_borrow_mut()
                .process_event(MobsimEvent::before_sim_step(now_time));

            if outward_now.is_multiple_of(3600) {
                let _hour = outward_now / 3600;
                let _min = (outward_now % 3600) / 60;
                info!(
                    "#{} of Qsim at {_hour:02}:{_min:02}; Active Nodes: {}, Active Links: {}, Vehicles on Network Partition: {}",
                    self.leg_engine.net_message_broker().rank(),
                    self.leg_engine.network().active_nodes(),
                    self.leg_engine.network().active_links(),
                    self.leg_engine.network().veh_on_net()
                );
            }

            agents_changing_engine = self.do_sim_step(now, agents_changing_engine);

            self.comp_env
                .mobsim_events_manager_borrow_mut()
                .process_event(MobsimEvent::after_sim_step(now_time));

            now = now.next();
        }

        let agents = self
            .activity_engine
            .drain()
            .into_iter()
            .chain(self.leg_engine.drain())
            .chain(agents_changing_engine)
            .chain(self.pending_leg_agents.drain(..).map(|(_, agent)| agent))
            .collect::<Vec<_>>();

        // Note that agents who just ended a leg but haven't started the last activity yet are considered stuck.
        self.emit_stuck_events(self.clock.tick_to_time(self.end_tick), &agents);
        agents
    }

    fn emit_stuck_events(&mut self, time: SimTime, agents: &[SimulationAgent]) {
        let mut events = agents
            .iter()
            .filter_map(|agent| match agent.state() {
                SimulationAgentState::ACTIVITY if agent.next_leg().is_none() => None,
                SimulationAgentState::LEG => Some(
                    PersonStuckEventBuilder::default()
                        .time(time)
                        .person(agent.id().clone())
                        .link(agent.curr_link_id().cloned())
                        .leg_mode(Some(agent.curr_leg().mode.clone()))
                        .build()
                        .unwrap(),
                ),
                SimulationAgentState::STUCK => None,
                SimulationAgentState::ACTIVITY => Some(
                    PersonStuckEventBuilder::default()
                        .time(time)
                        .person(agent.id().clone())
                        .build()
                        .unwrap(),
                ),
            })
            .collect::<Vec<_>>();

        // Sort for deterministic results
        events.sort_unstable_by_key(|event| event.person.internal());
        let mut events_manager = self.comp_env.events_manager_borrow_mut();
        for event in &events {
            events_manager.process_event(event);
        }
    }

    /// Performs a sim step for the activity engine and the leg engine.
    /// Leg arrivals start their next activity in the same tick; resulting legs enter the leg engine
    /// on the next exchange while keeping their original event time.
    fn do_sim_step(&mut self, now: Tick, agents: Vec<SimulationAgent>) -> Vec<SimulationAgent> {
        let agents_act_to_leg = self.activity_engine.do_step(now, agents);
        for (event_time, agent) in self.pending_leg_agents.drain(..) {
            self.leg_engine
                .receive_agents_at(now, event_time, vec![agent]);
        }
        let agents_leg_to_act = self.leg_engine.do_step(now, agents_act_to_leg);
        self.pending_leg_agents = self
            .activity_engine
            .complete_legs_same_tick(now, agents_leg_to_act)
            .into_iter()
            .map(|agent| (now, agent))
            .collect();
        Vec::new()
    }

    pub(crate) fn is_local_route(
        agent: &SimulationAgent,
        message_broker: &NetMessageBroker<C>,
    ) -> bool {
        let leg = agent.curr_leg();
        let route = leg.route.as_ref().unwrap();
        let to = message_broker.rank_for_link(route.end_link());
        message_broker.rank() == to
    }
}

impl<C: SimCommunicator + 'static> Debug for Simulation<C> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Simulation with Rank #{}",
            self.leg_engine.net_message_broker().rank()
        )
    }
}

pub struct SimulationBuilder<C: SimCommunicator> {
    input: MobsimInput,
    net_message_broker: NetMessageBroker<C>,
    comp_env: ThreadLocalComputationalEnvironment,
    agent_source: DynAgentSource,
}

impl<C: SimCommunicator> SimulationBuilder<C> {
    pub fn new(
        input: MobsimInput,
        net_message_broker: NetMessageBroker<C>,
        comp_env: ThreadLocalComputationalEnvironment,
        agent_source: DynAgentSource,
    ) -> Self {
        SimulationBuilder {
            input,
            net_message_broker,
            comp_env,
            agent_source,
        }
    }

    pub fn build(self) -> Simulation<C> {
        let clock = SimClock::new(self.input.partition.scenario.config.qsim().ticks_per_second);

        let agents = self
            .agent_source
            .create_agents(self.input.population, &self.input.partition);

        let MobsimScenarioPartition {
            scenario,
            network_partition,
            ..
        } = self.input.partition;

        let activity_engine = ActivityEngineBuilder::new(
            agents.into_values().collect(),
            &scenario.config,
            self.comp_env.clone(),
        )
        .build();

        let start_tick = clock.secs_to_tick(scenario.config.qsim().start_time as u64);
        let transit_engine = scenario.config.transit().simulate_vehicles.then(|| {
            TransitEngine::new(
                &scenario,
                network_partition.partition(),
                self.comp_env.clone(),
                clock,
                clock.tick_to_time(start_tick),
            )
        });
        let timetable_transit_engine = scenario.config.transit().simulate_vehicles.then(|| {
            TimetableTransitEngine::new(
                &scenario,
                network_partition.partition(),
                self.comp_env.clone(),
                clock,
                clock.tick_to_time(start_tick),
            )
        });

        let leg_engine = LegEngine::new(
            network_partition,
            scenario.garage.clone(),
            self.net_message_broker,
            scenario.config.qsim(),
            self.comp_env.clone(),
            transit_engine,
            timetable_transit_engine,
        );

        Simulation {
            activity_engine,
            leg_engine,
            comp_env: self.comp_env,
            start_tick: clock.secs_to_tick(scenario.config.qsim().start_time as u64),
            end_tick: clock.secs_to_tick(scenario.config.qsim().end_time as u64),
            clock,
            pending_leg_agents: Vec::new(),
        }
    }
}
