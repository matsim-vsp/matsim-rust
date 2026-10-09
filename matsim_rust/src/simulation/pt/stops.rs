//! Passengers waiting at transit stops. MATSim's `TransitStopAgentTracker`.
//!
//! A passenger waits on the partition that owns its access stop's link, which is also where every
//! vehicle serving that stop handles it, so the waiting lists never cross partitions.

use crate::simulation::Identifiable;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::id::Id;
use crate::simulation::scenario::transit::{TransitLine, TransitStopFacility};
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;

#[derive(Debug)]
pub(crate) struct WaitingPassenger {
    pub(crate) agent: SimulationAgent,
    since: SimTime,
    line: Id<TransitLine>,
    pub(crate) egress: Id<TransitStopFacility>,
}

impl WaitingPassenger {
    pub(crate) fn new(
        agent: SimulationAgent,
        since: SimTime,
        line: Id<TransitLine>,
        egress: Id<TransitStopFacility>,
    ) -> Self {
        Self {
            agent,
            since,
            line,
            egress,
        }
    }

    /// MATSim's default boarding acceptance `checkLineAndStop`: the vehicle serves the planned
    /// line and still stops at the planned egress stop.
    pub(crate) fn accepts(
        &self,
        line: &Id<TransitLine>,
        mut stops_to_come: impl Iterator<Item = Id<TransitStopFacility>>,
    ) -> bool {
        &self.line == line && stops_to_come.any(|stop| stop == self.egress)
    }
}

#[derive(Debug, Default)]
pub(crate) struct TransitStops {
    waiting: IntMap<Id<TransitStopFacility>, Vec<WaitingPassenger>>,
    alighted: Vec<SimulationAgent>,
}

impl TransitStops {
    /// Queues a passenger behind everyone who arrived earlier. Passengers arriving in the same
    /// instant use descending person ID, matching MATSim's same-time boarding order independently
    /// of how engines hand them over across partitions.
    pub(crate) fn add(&mut self, stop: Id<TransitStopFacility>, passenger: WaitingPassenger) {
        let queue = self.waiting.entry(stop).or_default();
        let key = (passenger.since, passenger.agent.id().internal());
        let position = queue.partition_point(|p| {
            p.since < key.0 || (p.since == key.0 && p.agent.id().internal() >= key.1)
        });
        queue.insert(position, passenger);
    }

    pub(crate) fn waiting_at(&self, stop: &Id<TransitStopFacility>) -> &[WaitingPassenger] {
        self.waiting
            .get(stop)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// Removes the passengers at the given positions of a stop's queue, keeping their order.
    pub(crate) fn take(
        &mut self,
        stop: &Id<TransitStopFacility>,
        positions: &[usize],
    ) -> Vec<WaitingPassenger> {
        if positions.is_empty() {
            return Vec::new();
        }
        let queue = self.waiting.get_mut(stop).expect("No passengers at stop");
        let mut taken: Vec<_> = positions
            .iter()
            .rev()
            .map(|&position| queue.remove(position))
            .collect();
        taken.reverse();
        taken
    }

    /// Passengers who left a vehicle during this step and now start their next activity.
    pub(crate) fn push_alighted(&mut self, agent: SimulationAgent) {
        self.alighted.push(agent);
    }

    pub(crate) fn take_alighted(&mut self) -> Vec<SimulationAgent> {
        std::mem::take(&mut self.alighted)
    }

    /// Passengers still waiting when Mobsim ends, together with the ones who just alighted. They
    /// leave as ordinary agents: a passenger whose vehicle never came is left in the last
    /// activity state, and the simulation's stuck-event pass turns that into the
    /// `stuckandabort` event MATSim writes for a passenger who never boarded.
    pub(crate) fn drain(&mut self) -> Vec<SimulationAgent> {
        self.waiting
            .drain()
            .flat_map(|(_, queue)| queue.into_iter().map(|p| p.agent))
            .chain(self.alighted.drain(..))
            .collect()
    }
}
