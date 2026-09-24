use crate::simulation::Identifiable;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::agents::{
    ActivityStartedEvent, AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic,
    WokeUpEvent,
};
use crate::simulation::config::Config;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::events::{ActivityEndEventBuilder, ActivityStartEventBuilder};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::time::{SimClock, SimTime, Tick};
use crate::simulation::time_queue::TimeQueue;
use tracing::instrument;

pub struct ActivityEngine {
    asleep_q: TimeQueue<AsleepSimulationAgent, InternalPerson>,
    awake_q: Vec<AwakeSimulationAgent>,
    comp_env: ThreadLocalComputationalEnvironment,
    clock: SimClock,
}

impl ActivityEngine {
    fn new(
        asleep_q: TimeQueue<AsleepSimulationAgent, InternalPerson>,
        awake_q: Vec<AwakeSimulationAgent>,
        comp_env: ThreadLocalComputationalEnvironment,
        clock: SimClock,
    ) -> Self {
        ActivityEngine {
            asleep_q,
            awake_q,
            comp_env,
            clock,
        }
    }

    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.awake_q
            .drain(..)
            .map(|a| a.agent)
            .chain(self.asleep_q.drain().into_iter().map(|a| a.agent))
            .collect()
    }

    #[instrument(level = "trace", skip(self, now, agents))]
    pub(crate) fn do_step(
        &mut self,
        now: impl Into<Tick>,
        agents: Vec<SimulationAgent>,
    ) -> Vec<SimulationAgent> {
        let now = now.into();
        let now_time = self.clock.tick_to_time(now);
        for mut agent in agents {
            agent.advance_plan(now_time);
            self.receive_agent(now, AsleepSimulationAgent::build(agent, now_time));
        }

        let mut end_after_wake_up = self.wake_up(now_time);
        self.notify_wakeup_all(now_time, &mut end_after_wake_up);

        let end_from_awake = self.end(now_time);
        self.notify_end_all(now_time, end_after_wake_up, end_from_awake)
    }

    #[instrument(
        level = "trace",
        skip(self, end_after_wake_up, end_from_awake),
        fields(now_ns = now.as_nanos())
    )]
    fn notify_end_all(
        &mut self,
        now: SimTime,
        end_after_wake_up: Vec<SimulationAgent>,
        end_from_awake: Vec<SimulationAgent>,
    ) -> Vec<SimulationAgent> {
        let mut res = Vec::with_capacity(end_after_wake_up.len() + end_from_awake.len());
        for mut agent in end_after_wake_up
            .into_iter()
            .chain(end_from_awake.into_iter())
        {
            self.comp_env.events_manager_borrow_mut().process_event(
                &ActivityEndEventBuilder::default()
                    .time(now)
                    .person(agent.id().clone())
                    .link(agent.curr_act().link_id.clone())
                    .act_type(agent.curr_act().act_type.clone())
                    .coordinate(agent.curr_act().coord.as_ref().unwrap().clone())
                    .build()
                    .unwrap(),
            );
            ActivityEngine::notify_act_end(&mut agent, now);
            res.push(agent);
        }
        res
    }

    #[instrument(
        level = "trace",
        skip(self, end_after_wake_up),
        fields(now_ns = now.as_nanos())
    )]
    fn notify_wakeup_all(&mut self, now: SimTime, end_after_wake_up: &mut [SimulationAgent]) {
        // Keep `now` as a regular span field for readability and add `now_ns` so the profiling
        // layer can reconstruct the exact `SimTime` without parsing `Debug` output.
        // inform agents about wakeup
        // those are the agents that are woken up and directly end their activity
        end_after_wake_up.iter_mut().for_each(|agent| {
            ActivityEngine::notify_wakeup(&mut self.comp_env, agent, now, now);
        });

        // inform all awake agents about wakeup
        for agent in &mut self.awake_q {
            let end_time = agent.end_time(now);
            ActivityEngine::notify_wakeup(&mut self.comp_env, &mut agent.agent, end_time, now);
        }
    }

    fn receive_agent(&mut self, now: Tick, mut agent: AsleepSimulationAgent) {
        // emmit act start event
        let now_time = self.clock.tick_to_time(now);
        let act = agent.agent.curr_act();
        self.comp_env.events_manager_borrow_mut().process_event(
            &ActivityStartEventBuilder::default()
                .time(now_time)
                .person(agent.agent.id().clone())
                .link(act.link_id.clone())
                .act_type(act.act_type.clone())
                .coordinate(act.coord.as_ref().unwrap().clone())
                .build()
                .unwrap(),
        );

        agent.agent.notify_event(
            &mut AgentEvent::ActivityStarted(ActivityStartedEvent {
                agent: &mut self.comp_env,
            }),
            now_time,
        );
        self.asleep_q.add(agent, now_time);
    }

    /// Pushes agents whose wakeup time is reached into the awake queue and returns agents whose end time is already reached.
    fn wake_up(&mut self, now: SimTime) -> Vec<SimulationAgent> {
        let mut end_agents = Vec::new();
        let wake_up = self.asleep_q.pop(now);

        // for fast turnaround, agents whose end time is already reached are directly returned and not put into the awake queue
        for agent in wake_up {
            let awake: AwakeSimulationAgent = agent.into();
            let end_time = awake.end_time(now);
            if end_time <= now {
                end_agents.push(awake.agent);
            } else {
                self.awake_q.push(awake);
            }
        }
        end_agents
    }

    fn end(&mut self, now: SimTime) -> Vec<SimulationAgent> {
        let mut agents = Vec::new();

        let mut i = 0;
        while i < self.awake_q.len() {
            let agent = &self.awake_q[i];
            if agent.end_time(now) <= now {
                let removed = self.awake_q.swap_remove(i);
                agents.push(removed.agent);
            } else {
                i += 1;
            }
        }
        agents
    }

    fn notify_wakeup(
        comp_env: &mut ThreadLocalComputationalEnvironment,
        agent: &mut SimulationAgent,
        end_time: SimTime,
        now: SimTime,
    ) {
        agent.notify_event(
            &mut AgentEvent::WokeUp(WokeUpEvent { comp_env, end_time }),
            now,
        );
    }

    fn notify_act_end(agent: &mut SimulationAgent, now: SimTime) {
        agent.notify_event(&mut AgentEvent::ActivityFinished(), now);
    }

    #[cfg(test)]
    fn awake_agents(&self) -> Vec<&SimulationAgent> {
        self.awake_q.iter().map(|a| &a.agent).collect()
    }
}

pub struct ActivityEngineBuilder<'c> {
    agents: Vec<SimulationAgent>,
    config: &'c Config,
    comp_env: ThreadLocalComputationalEnvironment,
}

impl<'c> ActivityEngineBuilder<'c> {
    pub fn new(
        agents: Vec<SimulationAgent>,
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
        let awake_q = Vec::new();
        ActivityEngine::new(asleep, awake_q, self.comp_env, clock)
    }
}

struct AwakeSimulationAgent {
    agent: SimulationAgent,
    begin_time: SimTime,
}

impl From<AsleepSimulationAgent> for AwakeSimulationAgent {
    fn from(value: AsleepSimulationAgent) -> Self {
        Self {
            agent: value.agent,
            begin_time: value.begin_time,
        }
    }
}

impl EndTime for AwakeSimulationAgent {
    fn end_time(&self, _now: SimTime) -> SimTime {
        // Using begin_time as reference because if only max_dur is set for activity, the agent assumes that the argument of end_time is the time when the activity started.
        self.agent.end_time(self.begin_time)
    }
}

struct AsleepSimulationAgent {
    agent: SimulationAgent,
    wakeup_time: SimTime,
    // we need to keep track of the begin time, because this will be used for the awake agent queue to determine the end time of the agent.
    begin_time: SimTime,
}

impl AsleepSimulationAgent {
    fn build(agent: SimulationAgent, now: SimTime) -> Self {
        let wakeup_time = agent.wakeup_time(now);
        AsleepSimulationAgent {
            agent,
            wakeup_time,
            begin_time: now,
        }
    }
}

impl EndTime for AsleepSimulationAgent {
    fn end_time(&self, _now: SimTime) -> SimTime {
        // end_time is used for the wake-up queue, so it should return the time when the agent is supposed to wake up.
        self.wakeup_time
    }
}

#[cfg(test)]
mod tests {
    use crate::external_services::ExternalServiceType;
    use crate::external_services::routing::{
        InternalRoutingRequest, InternalRoutingRequestPayloadBuilder, InternalRoutingResponse,
    };
    use crate::simulation::Identifiable;
    use crate::simulation::agents::SimulationAgentLogic;
    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::config::Config;
    use crate::simulation::controller::{
        RequestSender, ThreadLocalComputationalEnvironment,
        ThreadLocalComputationalEnvironmentBuilder,
    };
    use crate::simulation::engines::activity_engine::{
        ActivityEngine, ActivityEngineBuilder, AsleepSimulationAgent,
    };
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
        InternalPlanElement, InternalRoute,
    };
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::thread::JoinHandle;
    use std::time::Duration;
    use tokio::sync::mpsc::Receiver;

    #[deterministic_id_test]
    fn test_activity_engine_build() {
        let mut engine =
            ActivityEngineBuilder::new(vec![], &Config::default(), Default::default()).build();

        assert_eq!(engine.awake_q.len(), 0);
        assert_eq!(engine.asleep_q.len(), 0);
        engine.end(SimTime::from_secs(0));
    }

    #[deterministic_id_test]
    fn test_activity_engine_wake_up_plan() {
        let plan = create_plan();

        let agent = SimulationAgent::new_plan_based(InternalPerson::new(Id::create("1"), plan));
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
        let plan = create_plan();
        let agent = SimulationAgent::new_plan_based(InternalPerson::new(Id::create("1"), plan));
        let mut engine = create_engine(vec![], Default::default());
        let asleep_agent = AsleepSimulationAgent {
            agent,
            wakeup_time: SimTime::from_nanos(350_000_000),
            begin_time: SimTime::from_nanos(100_000_000),
        };
        let now = asleep_agent.begin_time;
        engine.asleep_q.add(asleep_agent, now);

        let early = engine.wake_up(SimTime::from_nanos(300_000_000));
        assert!(early.is_empty());
        assert_eq!(engine.awake_q.len(), 0);

        let ready = engine.wake_up(SimTime::from_nanos(400_000_000));
        assert!(ready.is_empty());
        assert_eq!(engine.awake_q.len(), 1);
    }

    #[deterministic_id_test]
    fn test_activity_engine_end() {
        let plan = create_plan();

        let agent = SimulationAgent::new_plan_based(InternalPerson::new(Id::create("1"), plan));
        let agents = vec![agent];

        let mut engine = create_engine(agents, Default::default());

        {
            let agents = engine.do_step(0, vec![]);
            assert!(agents.is_empty());
            assert_eq!(engine.awake_agents().len(), 0);
        }
        {
            let agents = engine.do_step(10, vec![]);
            assert_eq!(agents.len(), 1);
            assert_eq!(engine.awake_agents().len(), 0);
        }
    }

    #[deterministic_id_test]
    fn test_activity_engine_with_preplanning_horizon() {
        // The new mode id needs to be created before the test, so that it gets the correct internal id.
        Id::<String>::create("new_mode");

        let mut map: HashMap<ExternalServiceType, RequestSender> = HashMap::new();
        let (send, recv) = tokio::sync::mpsc::channel::<InternalRoutingRequest>(11);
        map.insert(
            ExternalServiceType::Routing("mode".to_string()),
            Arc::new(send).into(),
        );

        let env = ThreadLocalComputationalEnvironmentBuilder::default()
            .services(map.into())
            .mobsim_events_manager(Default::default())
            .partition_events_manager(Default::default())
            .build()
            .unwrap();

        let plan = create_plan();

        let handle = run_test_thread(recv);

        let agents = test_adaptive(plan, env);
        let agent = agents.first().unwrap();

        assert_eq!(agent.curr_act().act_type, Id::get_from_ext("home"));
        assert_eq!(agent.curr_act().link_id, Id::get_from_ext("start"));
        assert_eq!(agent.next_act().act_type, Id::get_from_ext("work"));
        assert_eq!(agent.next_act().link_id, Id::get_from_ext("end"));

        let leg = agent.next_leg().unwrap();

        assert_eq!(leg.mode, Id::get_from_ext("new_mode"));
        assert!(&leg.mode.external().eq("new_mode"));
        assert_eq!(leg, &new_leg());

        handle.join().unwrap();
    }

    fn run_test_thread(mut recv: Receiver<InternalRoutingRequest>) -> JoinHandle<()> {
        std::thread::spawn(move || {
            let request = recv.blocking_recv();
            assert!(request.is_some());

            let payload = InternalRoutingRequestPayloadBuilder::default()
                .person_id("1".to_string())
                .from_link("start".to_string())
                .from(Coordinate::default())
                .to(Coordinate::default())
                .to_link("end".to_string())
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
    ) -> Vec<SimulationAgent> {
        let agent =
            SimulationAgent::new_adaptive_plan_based(InternalPerson::new(Id::create("1"), plan));
        let agents = vec![agent];
        let mut engine = create_engine(agents, comp_env);
        {
            let agents = engine.do_step(0, vec![]);
            assert!(agents.is_empty());
            assert_eq!(engine.awake_agents().len(), 0);
        }
        {
            // agent is not released, but awake
            let agents = engine.do_step(5, vec![]);
            assert!(agents.is_empty());
            assert_eq!(engine.awake_agents().len(), 1);
            assert_eq!(engine.awake_agents()[0].id(), &Id::create("1"));
        }
        {
            let agents = engine.do_step(10, vec![]);
            assert_eq!(agents.len(), 1);
            assert_eq!(engine.awake_agents().len(), 0);
            agents
        }
    }

    fn create_engine(
        agents: Vec<SimulationAgent>,
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
