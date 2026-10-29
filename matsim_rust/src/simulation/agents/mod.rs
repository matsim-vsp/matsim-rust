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
    /// Materializes the owned plan after mobsim. Agents that do not represent a person of the
    /// population, such as a transit driver, return `None` and are not materialized.
    fn into_person(self: Box<Self>) -> Option<InternalPerson>;
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
