use crate::simulation::Identifiable;
use crate::simulation::agents::agent_logic::{
    AdaptivePlanBasedSimulationLogic, PlanBasedSimulationLogic,
};
use crate::simulation::agents::{
    AgentEvent, EndTime, EnvironmentalEventObserver, SimulationAgentLogic, SimulationAgentState,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{InternalActivity, InternalLeg, InternalPerson};
use crate::simulation::time::SimTime;

#[derive(Debug)]
pub struct SimulationAgent {
    logic: Box<dyn SimulationAgentLogic>,
}

impl PartialEq for SimulationAgent {
    fn eq(&self, other: &Self) -> bool {
        self.id() == other.id()
    }
}

impl SimulationAgent {
    pub fn new_plan_based(person: InternalPerson) -> Self {
        Self {
            logic: Box::new(PlanBasedSimulationLogic::new(person)),
        }
    }

    pub fn new_adaptive_plan_based(person: InternalPerson) -> Self {
        Self {
            logic: Box::new(AdaptivePlanBasedSimulationLogic::new(person)),
        }
    }

    pub fn new(logic: Box<dyn SimulationAgentLogic>) -> Self {
        Self { logic }
    }

    pub fn into_person(self) -> Option<InternalPerson> {
        self.logic.into_person()
    }
}

impl EndTime for SimulationAgent {
    fn end_time(&self, now: SimTime) -> SimTime {
        self.logic.end_time(now)
    }
}

impl Identifiable<InternalPerson> for SimulationAgent {
    fn id(&self) -> &Id<InternalPerson> {
        self.logic.id()
    }
}

impl EnvironmentalEventObserver for SimulationAgent {
    fn notify_event(&mut self, event: &mut AgentEvent, now: SimTime) {
        self.logic.notify_event(event, now)
    }
}

impl SimulationAgentLogic for SimulationAgent {
    fn curr_act(&self) -> &InternalActivity {
        self.logic.curr_act()
    }
    fn next_act(&self) -> &InternalActivity {
        self.logic.next_act()
    }
    fn curr_leg(&self) -> &InternalLeg {
        self.logic.curr_leg()
    }
    fn next_leg(&self) -> Option<&InternalLeg> {
        self.logic.next_leg()
    }
    fn advance_plan(&mut self, now: SimTime) {
        self.logic.advance_plan(now);
    }
    fn state(&self) -> SimulationAgentState {
        self.logic.state()
    }
    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.logic.is_wanting_to_arrive_on_current_link()
    }
    fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.logic.curr_link_id()
    }
    fn peek_next_link_id(&self) -> Option<&Id<Link>> {
        self.logic.peek_next_link_id()
    }
    fn wakeup_time(&self, now: SimTime) -> SimTime {
        self.logic.wakeup_time(now)
    }

    fn into_person(self: Box<Self>) -> Option<InternalPerson> {
        self.logic.into_person()
    }
}
