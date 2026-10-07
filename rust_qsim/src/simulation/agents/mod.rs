pub mod agent;
pub mod agent_logic;

use crate::simulation::Identifiable;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::pt::driver::TransitDriver;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{InternalActivity, InternalLeg, InternalPerson};
use crate::simulation::time::SimTime;
use std::fmt::Debug;

pub trait EndTime {
    fn end_time(&self, now: SimTime) -> SimTime;
}

pub trait SimulationAgentLogic:
    EndTime + Identifiable<InternalPerson> + EnvironmentalEventObserver + Send
{
    fn curr_act(&self) -> &InternalActivity;
    fn next_act(&self) -> &InternalActivity;
    fn curr_leg(&self) -> &InternalLeg;
    fn next_leg(&self) -> Option<&InternalLeg>;
    fn advance_plan(&mut self, now: SimTime);
    fn state(&self) -> SimulationAgentState;

    // Having these functions here is not ideal. See https://github.com/matsim-vsp/parallel_qsim_rust/issues/203 for more details.
    fn is_wanting_to_arrive_on_current_link(&self) -> bool;
    fn curr_link_id(&self) -> Option<&Id<Link>>;
    fn peek_next_link_id(&self) -> Option<&Id<Link>>;
    fn wakeup_time(&self, now: SimTime) -> SimTime;

    fn into_person(self: Box<Self>) -> Option<InternalPerson>;

    /// The transit driver behind this agent, if it drives a scheduled vehicle.
    fn transit_driver(&self) -> Option<&TransitDriver> {
        None
    }

    fn transit_driver_mut(&mut self) -> Option<&mut TransitDriver> {
        None
    }
}

pub trait EnvironmentalEventObserver {
    fn notify_event(&mut self, event: &mut AgentEvent, now: SimTime);
}

#[non_exhaustive]
pub enum AgentEvent<'a> {
    // activity-related events
    ActivityStarted(ActivityStartedEvent<'a>),
    WokeUp(WokeUpEvent<'a>),
    ActivityFinished(),

    // teleportation-related events
    TeleportationStarted(),
    TeleportationFinished(),

    // leg related events
    NetworkLegStarted(),
    LeftLink(),
    NetworkLegFinished(),

    // transit-related events
    /// A passenger got off a transit vehicle at its egress stop.
    LeftTransitVehicle(),
}

pub struct ActivityStartedEvent<'a> {
    pub agent: &'a mut ThreadLocalComputationalEnvironment,
}

pub struct WokeUpEvent<'w> {
    pub comp_env: &'w mut ThreadLocalComputationalEnvironment,
    // I think we should remove this field. This information comes from the agent to the engines and then goes back to the agent. So, it is redundant. paul, mar'26
    pub end_time: SimTime,
}

impl Debug for dyn SimulationAgentLogic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Simulation Agent Logic for agent with id {}",
            self.id().external()
        )
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum SimulationAgentState {
    LEG,
    ACTIVITY,
    STUCK,
}
