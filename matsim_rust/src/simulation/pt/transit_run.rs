use crate::simulation::pt::schedule::DepartureIndex;
use crate::simulation::vehicles::SimulationVehicle;

/// A departure of a transit vehicle which is on its way. It only carries the dynamic state; the
/// static data of its route lives in the shared [`TransitSimSchedule`](super::schedule::TransitSimSchedule).
#[derive(Debug)]
pub struct TransitRun {
    /// Index into [`TransitSimSchedule::departures`](super::schedule::TransitSimSchedule::departures).
    pub(crate) departure: DepartureIndex,
    /// Index of the stop at which the vehicle halts or which it approaches.
    pub(crate) stop: usize,
    pub(crate) vehicle: SimulationVehicle,
}
