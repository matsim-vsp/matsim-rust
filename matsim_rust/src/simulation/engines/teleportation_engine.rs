use crate::simulation::Identifiable;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{
    AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic,
};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::engines::{
    emit_partition_enter_events_for_agent, emit_partition_leave_events_for_agent,
};
use crate::simulation::events::{
    PtTeleportationArrivalEventBuilder, TeleportationArrivalEventBuilder,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::messages::TeleportationMessage;
use crate::simulation::messaging::partition_change::{
    PartitionChangeContext, PartitionChangeEntity,
};
use crate::simulation::messaging::sim_communication::SimCommunicator;
use crate::simulation::messaging::sim_communication::message_broker::NetMessageBroker;
use crate::simulation::scenario::population::{InternalPerson, InternalRoute};
use crate::simulation::simulation::Simulation;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;

pub(crate) struct TeleportedAgent {
    pub(crate) agent: SimulationAgent,
    pub(crate) end_time: SimTime,
}

impl TeleportedAgent {
    fn new(agent: SimulationAgent, end_time: SimTime) -> Self {
        Self { agent, end_time }
    }

    fn into_agent(self) -> SimulationAgent {
        self.agent
    }

    fn agent(&self) -> &SimulationAgent {
        &self.agent
    }
}

impl EndTime for TeleportedAgent {
    fn end_time(&self, _: SimTime) -> SimTime {
        self.end_time
    }
}

impl From<TeleportationMessage> for TeleportedAgent {
    fn from(message: TeleportationMessage) -> Self {
        let time = message.end_time();
        Self::new(message.into_agent(), time)
    }
}

pub(crate) struct TeleportationEngine {
    queue: TimeQueue<TeleportedAgent, InternalPerson>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl TeleportationEngine {
    pub fn new(comp_env: ThreadLocalComputationalEnvironment, clock: SimClock) -> Self {
        TeleportationEngine {
            queue: TimeQueue::new(),
            comp_env,
            clock,
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.queue
            .drain()
            .into_iter()
            .map(TeleportedAgent::into_agent)
            .collect()
    }

    pub(crate) fn receive_agent_at<C: SimCommunicator>(
        &mut self,
        now: Tick,
        event_time: SimTime,
        mut agent: SimulationAgent,
        net_message_broker: &mut NetMessageBroker<C>,
    ) {
        agent.notify_event(&mut AgentEvent::TeleportationStarted(), event_time);
        let end_time = agent.end_time(event_time);
        let teleportation = TeleportedAgent::new(agent, end_time);

        if Simulation::is_local_route(teleportation.agent(), net_message_broker) {
            self.enqueue(teleportation, event_time);
        } else {
            let to = net_message_broker.rank_for_link(
                teleportation
                    .agent()
                    .curr_link_id()
                    .expect("Remote teleported vehicles must have a destination link"),
            );
            emit_partition_leave_events_for_agent(
                &mut self.comp_env,
                teleportation.agent(),
                to,
                event_time,
            );
            let context = PartitionChangeContext {
                time: event_time,
                from: net_message_broker.rank(),
                to,
            };
            let attachments = self
                .comp_env
                .partition_migration_extensions_manager_borrow_mut()
                .send(
                    PartitionChangeEntity::TeleportationAgent(teleportation.agent()),
                    &context,
                );
            let mut message = TeleportationMessage::from(teleportation);
            message.set_attachments(attachments);
            net_message_broker.add_teleportation(message, now);
        }
    }

    pub(crate) fn receive_remote_agent(
        &mut self,
        now: Tick,
        teleported_agent: TeleportedAgent,
        from: u32,
        to: u32,
    ) {
        let due_tick = self
            .clock
            .time_to_tick(teleported_agent.end_time(self.clock.tick_to_time(now)));
        assert!(
            now <= due_tick,
            "Remote teleportation for agent {} from partition {} to partition {} arrived at tick {} after its queue-processing deadline: end time {}, due tick {}. This might happen\
            if teleportation messages are received one time step later than expected. To mitigate this problem, you might enable the global sync.",
            teleported_agent.agent().id().external(),
            from,
            to,
            now.value(),
            teleported_agent.end_time(self.clock.tick_to_time(now)),
            due_tick.value(),
        );

        let now_time = self.clock.tick_to_time(now);
        emit_partition_enter_events_for_agent(
            &mut self.comp_env,
            teleported_agent.agent(),
            from,
            now_time,
        );
        self.enqueue(teleported_agent, now_time);
    }

    fn enqueue(&mut self, teleportation: TeleportedAgent, now: SimTime) {
        // Using the internal id is stable since...
        // ... either proto ids were used (by definition stable)
        // ... or the agent was loaded via XML and sorted by external id before creating internal ids. paul, jul'26
        let stable_order = teleportation.agent().id().internal();
        self.queue.add_with_order(teleportation, now, stable_order);
    }

    #[hotpath::measure]
    pub fn do_step(&mut self, now: Tick) -> Vec<SimulationAgent> {
        let mut teleportation_agents = self.queue.pop(self.clock.tick_to_time(now));
        for teleporting_agent in &mut teleportation_agents {
            let agent = teleporting_agent.agent();

            match agent.curr_leg().route.as_ref().unwrap() {
                InternalRoute::Generic(_) => self.emit_travelled(now, agent),
                InternalRoute::Network(_) => self.emit_travelled(now, agent),
                InternalRoute::Pt(_) => self.emit_travelled_with_pt(now, agent),
            }
        }
        teleportation_agents
            .into_iter()
            .map(TeleportedAgent::into_agent)
            .collect()
    }

    fn emit_travelled(&mut self, now: Tick, agent: &SimulationAgent) {
        let now_time = self.clock.tick_to_time(now);
        let leg = agent.curr_leg();
        let route = leg.route.as_ref().unwrap();
        self.comp_env.events_manager_borrow_mut().process_event(
            &TeleportationArrivalEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .mode(leg.mode.clone())
                .distance(
                    route
                        .as_generic()
                        .distance()
                        .expect("Route distance needs to be set."),
                )
                .build()
                .unwrap(),
        );
    }

    fn emit_travelled_with_pt(&mut self, now: Tick, agent: &SimulationAgent) {
        let now_time = self.clock.tick_to_time(now);
        let leg = agent.curr_leg();
        let route = leg.route.as_ref().unwrap();
        let transit_line_id =
            Id::<String>::get_from_ext(route.as_pt().unwrap().description.transit_line_id.as_str());
        let transit_route_id = Id::<String>::get_from_ext(
            route.as_pt().unwrap().description.transit_route_id.as_str(),
        );
        let boarding_time = route
            .as_pt()
            .unwrap()
            .description
            .boarding_time
            .expect("Boarding time needs to be set.");
        let access_facility = Id::<String>::get_from_ext(
            route
                .as_pt()
                .unwrap()
                .description
                .access_facility_id
                .as_str(),
        );
        let egress_facility = Id::<String>::get_from_ext(
            route
                .as_pt()
                .unwrap()
                .description
                .egress_facility_id
                .as_str(),
        );

        self.comp_env.events_manager_borrow_mut().process_event(
            &PtTeleportationArrivalEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .mode(leg.mode.clone())
                .distance(
                    route
                        .as_generic()
                        .distance()
                        .expect("Route distance needs to be set."),
                )
                .line(transit_line_id)
                .route(transit_route_id)
                .boarding_time(boarding_time)
                .access_facility(access_facility)
                .egress_facility(egress_facility)
                .build()
                .unwrap(),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{TeleportationEngine, TeleportedAgent};
    use crate::simulation::Identifiable;
    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::agents::{
        AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic, SimulationAgentState,
    };
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
        InternalRoute,
    };
    use crate::simulation::time::{SimClock, SimTime, Tick};
    use macros::deterministic_id_test;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[deterministic_id_test]
    fn do_step_releases_subsecond_due_vehicle() {
        let clock = SimClock::new(10);
        let mut engine = TeleportationEngine::new(Default::default(), clock);
        let agent = create_generic_route_agent(1);
        let due_time = SimTime::from_nanos(350_000_000);

        engine.queue.add(
            TeleportedAgent::new(agent, due_time),
            SimTime::from_nanos(0),
        );

        let early = engine.do_step(Tick::new(3));
        assert!(early.is_empty());

        let ready = engine.do_step(Tick::new(4));
        assert_eq!(ready.len(), 1);
    }

    #[deterministic_id_test]
    fn remote_agent_received_before_due_uses_sender_end_time() {
        let clock = SimClock::new(1);
        let mut engine = TeleportationEngine::new(Default::default(), clock);
        let agent = create_generic_route_agent(1);
        let sender_end_time = SimTime::from_secs(4);

        engine.receive_remote_agent(
            Tick::new(3),
            TeleportedAgent::new(agent, sender_end_time),
            1,
            2,
        );

        assert!(engine.do_step(Tick::new(3)).is_empty());
        let ready = engine.do_step(Tick::new(4));
        assert_eq!(ready.len(), 1);
        assert_eq!(ready[0].id().external(), "1");
    }

    #[deterministic_id_test]
    fn remote_agent_received_after_due_tick_panics() {
        let id = 1;
        let receive_tick = 5;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let clock = SimClock::new(1);
            let mut engine = TeleportationEngine::new(Default::default(), clock);
            engine.receive_remote_agent(
                Tick::new(receive_tick),
                TeleportedAgent::new(create_generic_route_agent(id), SimTime::from_secs(4)),
                1,
                2,
            );
        }));

        assert!(result.is_err(), "receive tick {receive_tick} must panic");
    }

    #[deterministic_id_test]
    fn remote_agent_received_at_due_tick_does_not_panic() {
        let id = 1;
        let receive_tick = 4;
        let clock = SimClock::new(1);
        let mut engine = TeleportationEngine::new(Default::default(), clock);
        engine.receive_remote_agent(
            Tick::new(receive_tick),
            TeleportedAgent::new(create_generic_route_agent(id), SimTime::from_secs(4)),
            1,
            2,
        );
    }

    #[deterministic_id_test]
    fn simultaneous_teleportations_use_person_id_order() {
        let clock = SimClock::new(1);
        let mut engine = TeleportationEngine::new(Default::default(), clock);
        let agents = [
            create_generic_route_agent(1),
            create_generic_route_agent(2),
            create_generic_route_agent(3),
        ];
        let [agent_1, agent_2, agent_3] = agents;

        for agent in [agent_3, agent_1, agent_2] {
            engine.enqueue(
                TeleportedAgent::new(agent, SimTime::from_secs(10)),
                SimTime::default(),
            );
        }

        let ids: Vec<_> = engine
            .do_step(Tick::new(10))
            .into_iter()
            .map(|agent| agent.id().external().to_owned())
            .collect();
        assert_eq!(ids, vec!["1", "2", "3"]);
    }

    #[deterministic_id_test]
    fn remote_receive_does_not_restart_teleportation() {
        let started = Arc::new(AtomicUsize::new(0));
        let logic = CountingTeleportationLogic {
            delegate: create_generic_route_agent(1),
            started: Arc::clone(&started),
        };
        let mut agent = SimulationAgent::new(Box::new(logic));
        agent.notify_event(
            &mut AgentEvent::TeleportationStarted(),
            SimTime::from_secs(1),
        );
        assert_eq!(started.load(Ordering::SeqCst), 1);

        let clock = SimClock::new(1);
        let mut engine = TeleportationEngine::new(Default::default(), clock);
        engine.receive_remote_agent(
            Tick::new(2),
            TeleportedAgent::new(agent, SimTime::from_secs(4)),
            1,
            2,
        );

        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(engine.do_step(Tick::new(3)).is_empty());
        assert_eq!(engine.do_step(Tick::new(4)).len(), 1);
        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    struct CountingTeleportationLogic {
        delegate: SimulationAgent,
        started: Arc<AtomicUsize>,
    }

    impl EndTime for CountingTeleportationLogic {
        fn end_time(&self, now: SimTime) -> SimTime {
            self.delegate.end_time(now)
        }
    }

    impl Identifiable<InternalPerson> for CountingTeleportationLogic {
        fn id(&self) -> &Id<InternalPerson> {
            self.delegate.id()
        }
    }

    impl EnvironmentalEventObserver for CountingTeleportationLogic {
        fn notify_event(&mut self, event: &mut AgentEvent, now: SimTime) {
            if matches!(event, AgentEvent::TeleportationStarted()) {
                self.started.fetch_add(1, Ordering::SeqCst);
            }
            self.delegate.notify_event(event, now);
        }
    }

    impl SimulationAgentLogic for CountingTeleportationLogic {
        fn curr_act(&self) -> &InternalActivity {
            self.delegate.curr_act()
        }

        fn next_act(&self) -> &InternalActivity {
            self.delegate.next_act()
        }

        fn curr_leg(&self) -> &InternalLeg {
            self.delegate.curr_leg()
        }

        fn next_leg(&self) -> Option<&InternalLeg> {
            self.delegate.next_leg()
        }

        fn advance_plan(&mut self, now: SimTime) {
            self.delegate.advance_plan(now);
        }

        fn state(&self) -> SimulationAgentState {
            self.delegate.state()
        }

        fn is_wanting_to_arrive_on_current_link(&self) -> bool {
            self.delegate.is_wanting_to_arrive_on_current_link()
        }

        fn curr_link_id(&self) -> Option<&Id<Link>> {
            self.delegate.curr_link_id()
        }

        fn peek_next_link_id(&self) -> Option<&Id<Link>> {
            self.delegate.peek_next_link_id()
        }

        fn wakeup_time(&self, now: SimTime) -> SimTime {
            self.delegate.wakeup_time(now)
        }

        fn into_person(self: Box<Self>) -> Option<InternalPerson> {
            self.delegate.into_person()
        }
    }

    fn create_generic_route_agent(id: u64) -> SimulationAgent {
        let route = InternalRoute::Generic(InternalGenericRoute::new(
            Id::create("start"),
            Id::create("end"),
            None,
            Some(123.0),
            None,
        ));
        let leg = InternalLeg::new(
            route,
            "walk",
            "walk",
            std::time::Duration::default(),
            Some(SimTime::from_secs(1)),
        );
        let act = InternalActivity::new(
            Some(Coordinate::default()),
            "home",
            Id::create("start"),
            None,
            None,
            None,
        );
        let mut plan = InternalPlan::default();
        plan.add_act(act);
        plan.add_leg(leg);
        let person = InternalPerson::new(Id::create(id.to_string().as_str()), plan);
        let mut agent = SimulationAgent::new_plan_based(person);
        agent.advance_plan(SimTime::default());
        agent
    }
}
