pub mod agent;
pub mod agent_logic;
mod phases;

pub use phases::*;

use crate::simulation::Identifiable;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::time::SimTime;
use std::fmt::Debug;

pub trait EndTime {
    fn end_time(&self, now: SimTime) -> SimTime;
}

pub trait SimulationAgentLogic: Identifiable<InternalPerson> + Send {
    fn phase(&self) -> AgentPhase<'_>;
    fn phase_mut(&mut self) -> AgentPhaseMut<'_>;

    /// Materializes the owned plan after mobsim; every simulation agent has a person.
    fn into_person(self: Box<Self>) -> InternalPerson;
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
