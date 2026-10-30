use crate::simulation::agents::ActivityAgent;
use crate::simulation::agents::EndTime;
use crate::simulation::config::Config;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::events::{ActivityEndEventBuilder, ActivityStartEventBuilder};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;
use tracing::instrument;

pub struct ActivityEngine {
    asleep_q: TimeQueue<AsleepSimulationAgent, InternalPerson>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl ActivityEngine {
    fn new(
        asleep_q: TimeQueue<AsleepSimulationAgent, InternalPerson>,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
    ) -> Self {
        ActivityEngine {
            asleep_q,
            comp_env,
            clock,
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<ActivityAgent> {
        self.asleep_q.drain().into_iter().map(|a| a.agent).collect()
    }

    #[hotpath::measure]
    #[instrument(level = "trace", skip(self, now, agents))]
    pub(crate) fn do_step(
        &mut self,
        now: impl Into<Tick>,
        agents: Vec<ActivityAgent>,
    ) -> Vec<ActivityAgent> {
        let now = now.into();
        let now_time = self.clock.tick_to_time(now);
        for agent in agents {
            self.receive_agent(now, agent);
        }

        let end_agents = self.wake_up(now_time);
        self.notify_end_all(now_time, end_agents)
    }

    #[instrument(
        level = "trace",
        skip(self, end_agents),
        fields(now_ns = now.as_nanos())
    )]
    fn notify_end_all(
        &mut self,
        now: SimTime,
        mut end_agents: Vec<ActivityAgent>,
    ) -> Vec<ActivityAgent> {
        for agent in &mut end_agents {
            let act = agent.as_ref();
            self.comp_env.events_manager_borrow_mut().process_event(
                &ActivityEndEventBuilder::default()
                    .time(now)
                    .person(agent.id().clone())
                    .link(act.link_id().clone())
                    .act_type(act.act_type().clone())
                    .coordinate(act.coord().clone())
                    .build()
                    .unwrap(),
            );
            ActivityEngine::notify_act_end(agent, now);
        }
        end_agents
    }

    fn receive_agent(&mut self, now: Tick, mut agent: ActivityAgent) {
        // emmit act start event
        let now_time = self.clock.tick_to_time(now);
        let act = agent.as_ref();
        self.comp_env.events_manager_borrow_mut().process_event(
            &ActivityStartEventBuilder::default()
                .time(now_time)
                .person(agent.id().clone())
                .link(act.link_id().clone())
                .act_type(act.act_type().clone())
                .coordinate(act.coord().clone())
                .build()
                .unwrap(),
        );

        agent.notify_started(&mut self.comp_env, now_time);
        self.asleep_q
            .add(AsleepSimulationAgent::build(agent, now_time), now_time);
    }

    /// Delivers the wake-ups that are due and returns the agents whose end time is reached. All
    /// other agents have been woken up and go back into the queue.
    #[instrument(level = "trace", skip(self), fields(now_ns = now.as_nanos()))]
    fn wake_up(&mut self, now: SimTime) -> Vec<ActivityAgent> {
        let mut end_agents = Vec::new();

        for mut agent in self.asleep_q.pop(now) {
            if agent.next_wakeup.is_some_and(|wakeup| wakeup <= now) {
                // "True" wakeup: inform about wakeup and assign next wakeup.
                let end_time = agent.activity_end().max(now);
                ActivityEngine::notify_wakeup(&mut self.comp_env, &mut agent.agent, end_time, now);
                agent.next_wakeup = agent.agent.next_wakeup(now);
            }

            if agent.activity_end() <= now {
                end_agents.push(agent.agent);
            } else {
                self.asleep_q.add(agent, now);
            }
        }
        end_agents
    }

    fn notify_wakeup(
        comp_env: &mut ThreadLocalComputationalEnvironment,
        agent: &mut ActivityAgent,
        end_time: SimTime,
        now: SimTime,
    ) {
        agent.notify_wakeup(comp_env, end_time, now);
    }

    fn notify_act_end(agent: &mut ActivityAgent, now: SimTime) {
        agent.notify_finished(now);
    }
}

pub struct ActivityEngineBuilder<'c> {
    agents: Vec<ActivityAgent>,
    config: &'c Config,
    comp_env: ThreadLocalComputationalEnvironment,
}

impl<'c> ActivityEngineBuilder<'c> {
    pub fn new(
        agents: Vec<ActivityAgent>,
        config: &'c Config,
        comp_env: ThreadLocalComputationalEnvironment,
    ) -> Self {
        ActivityEngineBuilder {
            agents,
            config,
            comp_env,
        }
    }

    pub fn build(self) -> ActivityEngine {
        let clock = SimClock::new(self.config.qsim().ticks_per_second);
        let now = clock.secs_to_tick(self.config.qsim().start_time as u64);
        let now_time = clock.tick_to_time(now);

        let mut asleep = TimeQueue::new();
        for agent in self.agents {
            let asleep_agent = AsleepSimulationAgent::build(agent, now_time);
            asleep.add(asleep_agent, now_time);
        }
        ActivityEngine::new(asleep, self.comp_env, clock)
    }
}

struct AsleepSimulationAgent {
    agent: ActivityAgent,
    next_wakeup: Option<SimTime>,
    // we need to keep track of the begin time, because this will be used to determine the end time of the agent.
    begin_time: SimTime,
}

impl AsleepSimulationAgent {
    fn build(agent: ActivityAgent, now: SimTime) -> Self {
        let next_wakeup = agent.next_wakeup(now);
        AsleepSimulationAgent {
            agent,
            next_wakeup,
            begin_time: now,
        }
    }

    fn activity_end(&self) -> SimTime {
        self.agent.end_time(self.begin_time)
    }
}

impl EndTime for AsleepSimulationAgent {
    fn end_time(&self, _now: SimTime) -> SimTime {
        match self.next_wakeup {
            Some(wakeup) => wakeup.min(self.activity_end()),
            None => self.activity_end(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::external_services::ExternalServiceType;
    use crate::external_services::routing::{
        InternalRoutingRequest, InternalRoutingRequestPayloadBuilder, InternalRoutingResponse,
    };
    use crate::simulation::Identifiable;
    use crate::simulation::agents::agent_logic::{
        AdaptivePlanBasedSimulationLogic, PlanBasedSimulationLogic,
    };
    use crate::simulation::agents::{
        ActivityAgent, ActivityLogic, NextPhase, SimulationAgentLogic,
    };
    use crate::simulation::config::Config;
    use crate::simulation::controller::{
        RequestSender, ThreadLocalComputationalEnvironment,
        ThreadLocalComputationalEnvironmentBuilder,
    };
    use crate::simulation::engines::activity_engine::{
        ActivityEngine, ActivityEngineBuilder, AsleepSimulationAgent,
    };
    use crate::simulation::id::Id;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
        InternalPlanElement, InternalRoute,
    };
    use crate::simulation::scenario::{Coordinate, ScenarioCore};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::collections::{HashMap, VecDeque};
    use std::sync::{Arc, Mutex};
    use std::thread::JoinHandle;
    use std::time::Duration;
    use tokio::sync::mpsc::Receiver;

    #[derive(Debug, PartialEq)]
    enum Call {
        Wakeup { end_time: SimTime, now: SimTime },
        Finished { now: SimTime },
    }

    type Calls = Arc<Mutex<Vec<Call>>>;

    fn wakeup(end_time: u64, now: u64) -> Call {
        Call::Wakeup {
            end_time: SimTime::from_secs(end_time),
            now: SimTime::from_secs(now),
        }
    }

    fn finished(now: u64) -> Call {
        Call::Finished {
            now: SimTime::from_secs(now),
        }
    }

    /// Requests the given wake-ups one after the other and records the calls it receives.
    struct ScriptedActivity {
        id: Id<InternalPerson>,
        act_type: Id<String>,
        link: Id<Link>,
        coord: Coordinate,
        end: SimTime,
        wakeups: VecDeque<SimTime>,
        calls: Calls,
    }

    impl Identifiable<InternalPerson> for ScriptedActivity {
        fn id(&self) -> &Id<InternalPerson> {
            &self.id
        }
    }

    impl SimulationAgentLogic for ScriptedActivity {
        fn into_person(self: Box<Self>) -> Option<InternalPerson> {
            None
        }
    }

    impl ActivityLogic for ScriptedActivity {
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
            false
        }
        fn end_time(&self, _activity_start: SimTime) -> SimTime {
            self.end
        }
        fn next_wakeup(&self, _now: SimTime) -> Option<SimTime> {
            self.wakeups.front().copied()
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
            end_time: SimTime,
            now: SimTime,
        ) {
            self.wakeups.pop_front();
            self.calls
                .lock()
                .unwrap()
                .push(Call::Wakeup { end_time, now });
        }
        fn notify_finished(&mut self, now: SimTime) {
            self.calls.lock().unwrap().push(Call::Finished { now });
        }
        fn finish_activity(self: Box<Self>, _now: SimTime) -> NextPhase {
            unimplemented!("The activity engine does not end the phase of an agent")
        }
    }

    fn scripted_agent(id: &str, wakeups: &[u64], end: u64, calls: &Calls) -> ActivityAgent {
        Box::new(ScriptedActivity {
            id: Id::create(id),
            act_type: Id::create("home"),
            link: Id::create("start"),
            coord: Coordinate::default(),
            end: SimTime::from_secs(end),
            wakeups: wakeups.iter().copied().map(SimTime::from_secs).collect(),
            calls: calls.clone(),
        })
    }

    fn step_without_end(engine: &mut ActivityEngine, ticks: std::ops::Range<u32>) {
        for tick in ticks {
            assert!(engine.do_step(tick, vec![]).is_empty());
        }
    }

    #[deterministic_id_test]
    fn test_activity_engine_build() {
        let mut engine =
            ActivityEngineBuilder::new(vec![], &Config::default(), Default::default()).build();

        assert_eq!(engine.asleep_q.len(), 0);
        engine.wake_up(SimTime::from_secs(0));
    }

    #[deterministic_id_test]
    fn test_activity_engine_wake_up_plan() {
        let plan = create_plan();

        let agent = PlanBasedSimulationLogic::new_agent(InternalPerson::new(Id::create("1"), plan));
        let agents = vec![agent];

        let mut engine = create_engine(agents, Default::default());

        {
            let agents = engine.wake_up(SimTime::from_secs(0));
            assert!(agents.is_empty());
        }
        {
            let agents = engine.wake_up(SimTime::from_secs(10));
            assert_eq!(agents.len(), 1);
        }
    }

    #[deterministic_id_test]
    fn test_activity_engine_wake_up_subsecond_due_time() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[], 10, &calls);
        let mut engine = create_engine(vec![], Default::default());
        let asleep_agent = AsleepSimulationAgent {
            agent,
            next_wakeup: Some(SimTime::from_nanos(350_000_000)),
            begin_time: SimTime::from_nanos(100_000_000),
        };
        let now = asleep_agent.begin_time;
        engine.asleep_q.add(asleep_agent, now);

        let early = engine.wake_up(SimTime::from_nanos(300_000_000));
        assert!(early.is_empty());
        assert!(calls.lock().unwrap().is_empty());

        let ready = engine.wake_up(SimTime::from_nanos(400_000_000));
        assert!(ready.is_empty());
        assert_eq!(engine.asleep_q.len(), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            [Call::Wakeup {
                end_time: SimTime::from_secs(10),
                now: SimTime::from_nanos(400_000_000),
            }]
        );
    }

    #[deterministic_id_test]
    fn an_agent_without_wakeup_is_not_called_before_its_activity_ends() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[], 10, &calls);
        let mut engine = create_engine(vec![agent], Default::default());

        step_without_end(&mut engine, 0..10);
        assert!(calls.lock().unwrap().is_empty());

        assert_eq!(engine.do_step(10, vec![]).len(), 1);
        assert_eq!(*calls.lock().unwrap(), [finished(10)]);
    }

    #[deterministic_id_test]
    fn an_agent_gets_one_call_per_requested_wakeup() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[3, 7], 10, &calls);
        let mut engine = create_engine(vec![agent], Default::default());

        step_without_end(&mut engine, 0..10);
        assert_eq!(*calls.lock().unwrap(), [wakeup(10, 3), wakeup(10, 7)]);

        assert_eq!(engine.do_step(10, vec![]).len(), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            [wakeup(10, 3), wakeup(10, 7), finished(10)]
        );
    }

    #[deterministic_id_test]
    fn a_wakeup_in_the_tick_of_the_end_comes_before_the_end() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[10], 10, &calls);
        let mut engine = create_engine(vec![agent], Default::default());

        step_without_end(&mut engine, 0..10);
        assert_eq!(engine.do_step(10, vec![]).len(), 1);
        assert_eq!(*calls.lock().unwrap(), [wakeup(10, 10), finished(10)]);
    }

    #[deterministic_id_test]
    fn a_wakeup_after_the_end_is_not_delivered() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[11], 10, &calls);
        let mut engine = create_engine(vec![agent], Default::default());

        step_without_end(&mut engine, 0..10);
        assert_eq!(engine.do_step(10, vec![]).len(), 1);
        assert_eq!(*calls.lock().unwrap(), [finished(10)]);
    }

    #[deterministic_id_test]
    fn an_overdue_wakeup_is_delivered_when_the_activity_starts() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[5], 30, &calls);
        let mut engine = create_engine(vec![], Default::default());

        assert!(engine.do_step(20, vec![agent]).is_empty());
        assert_eq!(*calls.lock().unwrap(), [wakeup(30, 20)]);
    }

    #[deterministic_id_test]
    fn an_activity_that_starts_after_its_end_time_leaves_in_the_same_tick() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[5], 10, &calls);
        let mut engine = create_engine(vec![], Default::default());

        assert_eq!(engine.do_step(20, vec![agent]).len(), 1);
        // The agent leaves now, so it must not plan with the end time that has passed.
        assert_eq!(*calls.lock().unwrap(), [wakeup(20, 20), finished(20)]);
    }

    #[deterministic_id_test]
    fn a_time_that_has_passed_is_due_in_the_next_tick_after_a_wakeup() {
        let calls = Calls::default();
        let agent = scripted_agent("1", &[3, 3, 4], 10, &calls);
        let mut engine = create_engine(vec![agent], Default::default());

        step_without_end(&mut engine, 0..4);
        assert_eq!(*calls.lock().unwrap(), [wakeup(10, 3)]);

        step_without_end(&mut engine, 4..10);
        assert_eq!(
            *calls.lock().unwrap(),
            [wakeup(10, 3), wakeup(10, 4), wakeup(10, 5)]
        );
    }

    #[deterministic_id_test]
    fn all_wakeups_of_a_tick_come_before_the_first_end() {
        let calls = Calls::default();
        // The first agent is due first. Its end must wait for the wake-up of the second agent,
        // because an agent may block for a response at the end of its activity.
        let agents = vec![
            scripted_agent("1", &[], 5, &calls),
            scripted_agent("2", &[5], 9, &calls),
        ];
        let mut engine = create_engine(agents, Default::default());

        step_without_end(&mut engine, 0..5);
        assert_eq!(engine.do_step(5, vec![]).len(), 1);
        assert_eq!(*calls.lock().unwrap(), [wakeup(9, 5), finished(5)]);
    }

    #[deterministic_id_test]
    fn an_agent_goes_behind_agents_with_the_same_end_when_it_is_woken_up() {
        let calls = Calls::default();
        let agents = vec![
            scripted_agent("1", &[3], 5, &calls),
            scripted_agent("2", &[], 5, &calls),
            scripted_agent("3", &[4], 5, &calls),
            scripted_agent("4", &[], 5, &calls),
        ];
        let mut engine = create_engine(agents, Default::default());

        step_without_end(&mut engine, 0..5);
        let ended = engine.do_step(5, vec![]);
        let ids: Vec<_> = ended.iter().map(|a| a.id().external()).collect();
        assert_eq!(ids, ["2", "4", "1", "3"]);
    }

    #[deterministic_id_test]
    fn drain_returns_agents_before_and_after_their_wakeup() {
        let calls = Calls::default();
        let agents = vec![
            scripted_agent("1", &[3], 10, &calls),
            scripted_agent("2", &[7], 10, &calls),
        ];
        let mut engine = create_engine(agents, Default::default());

        step_without_end(&mut engine, 0..5);
        let drained = engine.drain();
        let mut ids: Vec<_> = drained.iter().map(|a| a.id().external()).collect();
        ids.sort();
        assert_eq!(ids, ["1", "2"]);
        assert_eq!(engine.asleep_q.len(), 0);
    }

    #[deterministic_id_test]
    fn test_activity_engine_end() {
        let plan = create_plan();

        let agent = PlanBasedSimulationLogic::new_agent(InternalPerson::new(Id::create("1"), plan));
        let agents = vec![agent];

        let mut engine = create_engine(agents, Default::default());

        {
            let agents = engine.do_step(0, vec![]);
            assert!(agents.is_empty());
            assert_eq!(engine.asleep_q.len(), 1);
        }
        {
            let agents = engine.do_step(10, vec![]);
            assert_eq!(agents.len(), 1);
            assert_eq!(engine.asleep_q.len(), 0);
        }
    }

    #[deterministic_id_test]
    fn test_activity_engine_with_preplanning_horizon() {
        // The new mode id needs to be created before the test, so that it gets the correct internal id.
        Id::<String>::create("new_mode");

        let (env, recv) = routing_env(ScenarioCore::default());

        let plan = create_plan();

        let handle = run_test_thread(
            recv,
            ("start", Coordinate::default()),
            ("end", Coordinate::default()),
        );

        let agents = test_adaptive(plan, env);
        let agent = agents.first().unwrap();

        assert_eq!(agent.act_type(), &Id::get_from_ext("home"));
        assert_eq!(agent.link_id(), &Id::get_from_ext("start"));
        assert!(agent.has_next_leg());

        handle.join().unwrap();
        // The agent has finished its first activity, so the replaced trip follows element 0.
        let leg = agents
            .into_iter()
            .next()
            .unwrap()
            .finish_activity(SimTime::from_secs(10))
            .expect_leg();
        assert_eq!(leg.mode(), &Id::get_from_ext("new_mode"));
        assert!(leg.mode().external().eq("new_mode"));

        // The replaced trip applies to this iteration only.
        let person = leg.into_person().unwrap();
        let elements = &person.selected_plan().unwrap().elements;
        let next_act = elements[2].as_activity().unwrap();
        assert_eq!(next_act.act_type, Id::get_from_ext("work"));
        assert_eq!(next_act.link_id, Some(Id::get_from_ext("end")));
        assert_eq!(elements[1].as_leg().unwrap().mode, Id::get_from_ext("mode"));
    }

    fn routing_env(
        scenario_core: ScenarioCore,
    ) -> (
        ThreadLocalComputationalEnvironment,
        Receiver<InternalRoutingRequest>,
    ) {
        let mut map: HashMap<ExternalServiceType, RequestSender> = HashMap::new();
        let (send, recv) = tokio::sync::mpsc::channel::<InternalRoutingRequest>(11);
        map.insert(
            ExternalServiceType::Routing("mode".to_string()),
            Arc::new(send).into(),
        );

        let env = ThreadLocalComputationalEnvironmentBuilder::default()
            .scenario_core(scenario_core)
            .services(map.into())
            .mobsim_events_manager(Default::default())
            .partition_events_manager(Default::default())
            .build()
            .unwrap();
        (env, recv)
    }

    /// Answers one routing request after checking that it starts and ends at the given links and
    /// coordinates. A second request fails the test.
    fn run_test_thread(
        mut recv: Receiver<InternalRoutingRequest>,
        from: (&'static str, Coordinate),
        to: (&'static str, Coordinate),
    ) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let request = recv.blocking_recv();
            assert!(request.is_some());

            let payload = InternalRoutingRequestPayloadBuilder::default()
                .person_id("1".to_string())
                .from_link(from.0.to_string())
                .from(from.1)
                .to(to.1)
                .to_link(to.0.to_string())
                .mode("mode".to_string())
                .departure_time(SimTime::from_secs(10))
                .now(SimTime::from_secs(5))
                .build()
                .unwrap();
            assert!(
                request
                    .as_ref()
                    .unwrap()
                    .payload
                    .equals_ignoring_uuid(&payload)
            );
            request
                .unwrap()
                .response_tx
                .send(InternalRoutingResponse {
                    elements: vec![InternalPlanElement::Leg(new_leg())],
                    request_id: payload.uuid,
                })
                .unwrap();

            // The channel closes when the engine is dropped.
            assert!(recv.blocking_recv().is_none());
        })
    }

    fn new_leg() -> InternalLeg {
        InternalLeg {
            mode: Id::create("new_mode"),
            routing_mode: Some(Id::create("new_mode")),
            dep_time: Some(SimTime::default()),
            trav_time: Some(Duration::from_secs(2)),
            route: Some(InternalRoute::Generic(InternalGenericRoute::new(
                Id::create("start"),
                Id::create("end"),
                None,
                None,
                None,
            ))),
            attributes: Default::default(),
        }
    }

    fn test_adaptive(
        plan: InternalPlan,
        comp_env: ThreadLocalComputationalEnvironment,
    ) -> Vec<ActivityAgent> {
        let agent =
            AdaptivePlanBasedSimulationLogic::new_agent(InternalPerson::new(Id::create("1"), plan));
        let agents = vec![agent];
        let mut engine = create_engine(agents, comp_env);
        {
            let agents = engine.do_step(0, vec![]);
            assert!(agents.is_empty());
            assert_eq!(engine.asleep_q.len(), 1);
        }
        {
            // agent is not released, but was woken up
            let agents = engine.do_step(5, vec![]);
            assert!(agents.is_empty());
            assert_eq!(engine.asleep_q.len(), 1);
        }
        {
            let agents = engine.do_step(10, vec![]);
            assert_eq!(agents.len(), 1);
            assert_eq!(engine.asleep_q.len(), 0);
            agents
        }
    }

    fn create_engine(
        agents: Vec<ActivityAgent>,
        comp_env: ThreadLocalComputationalEnvironment,
    ) -> ActivityEngine {
        ActivityEngineBuilder::new(agents, &Config::default(), comp_env).build()
    }

    fn create_plan() -> InternalPlan {
        let mut plan = InternalPlan::default();
        let mut activity = InternalActivity::new(
            Some(Coordinate::default()),
            "home",
            Id::create("start"),
            None,
            None,
            Some(Duration::from_secs(10)),
        );
        activity.attributes.add(
            crate::simulation::scenario::population::PREPLANNING_HORIZON,
            5,
        );
        plan.add_act(activity);
        plan.add_leg(InternalLeg::new(
            InternalRoute::Generic(InternalGenericRoute::new(
                Id::create("start"),
                Id::create("end"),
                None,
                None,
                None,
            )),
            "mode",
            "mode",
            Duration::from_secs(1),
            Some(SimTime::from_secs(2)),
        ));
        plan.add_act(InternalActivity::new(
            Some(Coordinate::default()),
            "work",
            Id::create("end"),
            None,
            None,
            Some(Duration::from_secs(10)),
        ));
        plan
    }
}
