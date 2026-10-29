use crate::simulation::Identifiable;
use crate::simulation::agents::{LegAgent, NetworkDriverAgent, NetworkDriverLogic};
use crate::simulation::id::Id;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

#[derive(Debug)]
pub struct SimulationVehicle {
    pub(super) vehicle: InternalVehicle,
    pub(super) driver: Option<NetworkDriverAgent>,
    pub(super) passengers: Vec<LegAgent>,
}

impl SimulationVehicle {
    pub fn new(
        vehicle: InternalVehicle,
        driver: Option<NetworkDriverAgent>,
        passengers: Vec<LegAgent>,
    ) -> Self {
        Self {
            vehicle,
            driver,
            passengers,
        }
    }

    #[cfg(test)]
    pub fn from_parts(
        id: u64,
        veh_type: u64,
        max_v: f64,
        pce: f64,
        driver: NetworkDriverAgent,
    ) -> Self {
        Self::new(
            InternalVehicle::new(id, veh_type, max_v, pce),
            Some(driver),
            Vec::new(),
        )
    }

    pub fn driver_mut(&mut self) -> &mut dyn NetworkDriverLogic {
        self.driver
            .as_mut()
            .expect("SimulationVehicle has no driver.")
            .as_mut()
    }

    pub fn driver(&self) -> &dyn NetworkDriverLogic {
        self.driver
            .as_ref()
            .expect("SimulationVehicle has no driver.")
            .as_ref()
    }

    pub fn passengers(&self) -> &Vec<LegAgent> {
        &self.passengers
    }

    pub fn id(&self) -> &Id<InternalVehicle> {
        &self.vehicle.id
    }

    pub fn max_v(&self) -> f64 {
        self.vehicle.max_v
    }

    pub fn pce(&self) -> f64 {
        self.vehicle.pce
    }

    pub fn curr_link_id(&self) -> Option<&Id<Link>> {
        self.driver().curr_link_id()
    }

    pub fn peek_next_route_element(&self) -> Option<&Id<Link>> {
        self.driver().peek_next_link_id()
    }

    pub fn internal_vehicle(&self) -> &InternalVehicle {
        &self.vehicle
    }

    pub fn into_agents(mut self) -> Vec<LegAgent> {
        let mut agents = Vec::with_capacity(1 + self.passengers.len());
        let driver = self
            .driver
            .take()
            .expect("SimulationVehicle has no driver.");
        agents.push(driver as LegAgent);
        agents.extend(self.passengers);
        agents
    }
}

impl SimulationVehicle {
    /// Moves the driver over the node onto `next_link`. As in MATSim, passengers keep their own
    /// position until they leave the vehicle; their arrival link is the vehicle's link.
    // TODO(pt): A passenger's `curr_link_id()` still returns its departure link after arrival,
    // because nothing moves its route cursor. Arrival events use the vehicle's link, but a Stuck
    // event for a passenger who arrives in the last tick would report the departure link. MATSim
    // sets the position on arrival (`notifyArrivalOnLinkByNonNetworkMode`). Do the same when PT
    // passengers are introduced; today `unpark_veh` creates vehicles without passengers.
    pub fn move_over_node(&mut self, next_link: &Id<Link>, now: SimTime) {
        self.driver_mut().notify_move_over_node(next_link, now);
    }
}

impl Identifiable<InternalVehicle> for SimulationVehicle {
    fn id(&self) -> &Id<InternalVehicle> {
        self.id()
    }
}
