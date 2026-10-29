use crate::simulation::agents::{EndTime, TransitTeleportation};
use crate::simulation::agents::{LegAgent, TeleportationAgent, TeleportationLogic};
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
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::simulation::Simulation;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;

pub(crate) struct TeleportationAgentWrapper {
    pub(crate) agent: TeleportationAgent,
    pub(crate) end_time: SimTime,
}

impl TeleportationAgentWrapper {
    fn new(agent: TeleportationAgent, end_time: SimTime) -> Self {
        Self { agent, end_time }
    }

    fn into_agent(self) -> TeleportationAgent {
        self.agent
    }

    fn agent(&self) -> &dyn TeleportationLogic {
        self.agent.as_ref()
    }
}

impl EndTime for TeleportationAgentWrapper {
    fn end_time(&self, _: SimTime) -> SimTime {
        self.end_time
    }
}

impl From<TeleportationMessage> for TeleportationAgentWrapper {
    fn from(message: TeleportationMessage) -> Self {
        let time = message.end_time();
        Self::new(message.into_agent(), time)
    }
}

pub(crate) struct TeleportationEngine {
    queue: TimeQueue<TeleportationAgentWrapper, InternalPerson>,
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

    pub(crate) fn drain(&mut self) -> Vec<LegAgent> {
        self.queue
            .drain()
            .into_iter()
            .map(|agent| agent.into_agent() as LegAgent)
            .collect()
    }

    pub(crate) fn receive_agent<C: SimCommunicator>(
        &mut self,
        now: Tick,
        mut agent: TeleportationAgent,
        net_message_broker: &mut NetMessageBroker<C>,
    ) {
        let now_time = self.clock.tick_to_time(now);
        agent.notify_begin_teleportation(now_time);
        let end_time = agent.arrival_time(now_time);
        let teleportation = TeleportationAgentWrapper::new(agent, end_time);

        if Simulation::is_local_route(teleportation.agent(), net_message_broker) {
            self.enqueue(teleportation, now_time);
        } else {
            let to = net_message_broker.rank_for_link(teleportation.agent().destination_link());
            emit_partition_leave_events_for_agent(
                &mut self.comp_env,
                teleportation.agent(),
                to,
                now_time,
            );
            let context = PartitionChangeContext {
                time: now_time,
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
        teleported_agent: TeleportationAgentWrapper,
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

    fn enqueue(&mut self, teleportation: TeleportationAgentWrapper, now: SimTime) {
        // Using the internal id is stable since...
        // ... either proto ids were used (by definition stable)
        // ... or the agent was loaded via XML and sorted by external id before creating internal ids. paul, jul'26
        let stable_order = teleportation.agent().id().internal();
        self.queue.add_with_order(teleportation, now, stable_order);
    }

    #[hotpath::measure]
    pub fn do_step(&mut self, now: Tick) -> Vec<LegAgent> {
        let mut teleportation_agents = self.queue.pop(self.clock.tick_to_time(now));
        for teleporting_agent in &mut teleportation_agents {
            let agent = teleporting_agent.agent();

            match agent.transit() {
                Some(transit) => self.emit_travelled_with_pt(now, agent, transit),
                None => self.emit_travelled(now, agent),
            }
        }
        teleportation_agents
            .into_iter()
            .map(|agent| agent.into_agent() as LegAgent)
            .collect()
    }

    fn emit_travelled(&mut self, now: Tick, agent: &dyn TeleportationLogic) {
        let now_time = self.clock.tick_to_time(now);
        self.comp_env.events_manager_borrow_mut().process_event(
            &TeleportationArrivalEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .mode(agent.mode().clone())
                .distance(agent.distance().expect("Route distance needs to be set."))
                .build()
                .unwrap(),
        );
    }

    fn emit_travelled_with_pt(
        &mut self,
        now: Tick,
        agent: &dyn TeleportationLogic,
        transit: TransitTeleportation<'_>,
    ) {
        let now_time = self.clock.tick_to_time(now);
        let transit_line_id = Id::<String>::get_from_ext(transit.line);
        let transit_route_id = Id::<String>::get_from_ext(transit.route);
        let boarding_time = transit
            .boarding_time
            .expect("Boarding time needs to be set.");
        let access_facility = Id::<String>::get_from_ext(transit.access_facility);
        let egress_facility = Id::<String>::get_from_ext(transit.egress_facility);

        self.comp_env.events_manager_borrow_mut().process_event(
            &PtTeleportationArrivalEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .mode(agent.mode().clone())
                .distance(agent.distance().expect("Route distance needs to be set."))
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
    use super::{TeleportationAgentWrapper, TeleportationEngine};
    use crate::simulation::Identifiable;
    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::agents::{
        ActivityAgent, LegAgent, LegLogic, NetworkDriverAgent, SimulationAgentLogic,
        TeleportationAgent, TeleportationLogic, TransitTeleportation,
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
            TeleportationAgentWrapper::new(agent, due_time),
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
            TeleportationAgentWrapper::new(agent, sender_end_time),
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
                TeleportationAgentWrapper::new(
                    create_generic_route_agent(id),
                    SimTime::from_secs(4),
                ),
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
            TeleportationAgentWrapper::new(create_generic_route_agent(id), SimTime::from_secs(4)),
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
                TeleportationAgentWrapper::new(agent, SimTime::from_secs(10)),
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
        let mut agent: TeleportationAgent = Box::new(logic);
        agent.notify_begin_teleportation(SimTime::from_secs(1));
        assert_eq!(started.load(Ordering::SeqCst), 1);

        let clock = SimClock::new(1);
        let mut engine = TeleportationEngine::new(Default::default(), clock);
        engine.receive_remote_agent(
            Tick::new(2),
            TeleportationAgentWrapper::new(agent, SimTime::from_secs(4)),
            1,
            2,
        );

        assert_eq!(started.load(Ordering::SeqCst), 1);
        assert!(engine.do_step(Tick::new(3)).is_empty());
        assert_eq!(engine.do_step(Tick::new(4)).len(), 1);
        assert_eq!(started.load(Ordering::SeqCst), 1);
    }

    struct CountingTeleportationLogic {
        delegate: TeleportationAgent,
        started: Arc<AtomicUsize>,
    }

    impl Identifiable<InternalPerson> for CountingTeleportationLogic {
        fn id(&self) -> &Id<InternalPerson> {
            self.delegate.id()
        }
    }

    impl SimulationAgentLogic for CountingTeleportationLogic {
        fn into_person(self: Box<Self>) -> Option<InternalPerson> {
            self.delegate.into_person()
        }
    }

    impl LegLogic for CountingTeleportationLogic {
        fn mode(&self) -> &Id<String> {
            self.delegate.mode()
        }
        fn routing_mode(&self) -> Option<&Id<String>> {
            self.delegate.routing_mode()
        }
        fn departure_link(&self) -> &Id<Link> {
            self.delegate.departure_link()
        }
        fn curr_link_id(&self) -> Option<&Id<Link>> {
            self.delegate.curr_link_id()
        }
        fn try_into_network(self: Box<Self>) -> Result<NetworkDriverAgent, LegAgent> {
            Err(self)
        }
        fn try_into_teleportation(self: Box<Self>) -> Result<TeleportationAgent, LegAgent> {
            Ok(self)
        }
        fn start_next_activity(self: Box<Self>, now: SimTime) -> ActivityAgent {
            self.delegate.start_next_activity(now)
        }
    }

    impl TeleportationLogic for CountingTeleportationLogic {
        fn destination_link(&self) -> &Id<Link> {
            self.delegate.destination_link()
        }
        fn arrival_time(&self, departure: SimTime) -> SimTime {
            self.delegate.arrival_time(departure)
        }
        fn distance(&self) -> Option<f64> {
            self.delegate.distance()
        }
        fn transit(&self) -> Option<TransitTeleportation<'_>> {
            self.delegate.transit()
        }
        fn notify_begin_teleportation(&mut self, now: SimTime) {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.delegate.notify_begin_teleportation(now);
        }
    }

    fn create_generic_route_agent(id: u64) -> TeleportationAgent {
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
        let agent = SimulationAgent::new_plan_based(person).start_next_leg(SimTime::default());
        agent.try_into_teleportation().unwrap()
    }
}
