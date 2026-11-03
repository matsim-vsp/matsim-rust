use crate::simulation::Identifiable;
use crate::simulation::agents::{
    LegAgent, NetworkDriverAgent, NetworkDriverLogic, PassengerAgent, TransitDriverAgent,
    TransitDriverLogic,
};
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
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
    // TODO: A passenger's `curr_link_id()` still returns its departure link after arrival,
    // because nothing moves its route cursor. Arrival events use the vehicle's link, but a Stuck
    // event for a passenger who arrives in the last tick would report the departure link. MATSim
    // sets the position on arrival (`notifyArrivalOnLinkByNonNetworkMode`). Do the same when
    // network vehicles take passengers; today `unpark_veh` creates vehicles without passengers.
    // Transit passengers do not ride these vehicles; `PassengerLogic::notify_left_vehicle` moves
    // their cursor.
    pub fn move_over_node(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        next_link: &Id<Link>,
        now: SimTime,
    ) {
        self.driver_mut()
            .notify_move_over_node(comp_env, next_link, now);
    }
}

impl Identifiable<InternalVehicle> for SimulationVehicle {
    fn id(&self) -> &Id<InternalVehicle> {
        self.id()
    }
}

/// A vehicle of schedule-based transit. Unlike [`SimulationVehicle`], it keeps the roles of its
/// driver and its passengers, because the transit engine asks them at every stop.
#[derive(Debug)]
pub struct TransitVehicle {
    id: Id<InternalVehicle>,
    driver: TransitDriverAgent,
    passengers: Vec<PassengerAgent>,
}

impl TransitVehicle {
    pub fn new(id: Id<InternalVehicle>, driver: TransitDriverAgent) -> Self {
        Self {
            id,
            driver,
            passengers: Vec::new(),
        }
    }

    pub fn id(&self) -> &Id<InternalVehicle> {
        &self.id
    }

    pub fn driver(&self) -> &dyn TransitDriverLogic {
        self.driver.as_ref()
    }

    pub fn driver_mut(&mut self) -> &mut dyn TransitDriverLogic {
        self.driver.as_mut()
    }

    pub fn passengers(&self) -> &[PassengerAgent] {
        &self.passengers
    }

    pub fn add_passenger(&mut self, passenger: PassengerAgent) {
        self.passengers.push(passenger);
    }

    /// Removes all passengers, e.g. at the end of the mobsim.
    pub fn take_passengers(&mut self) -> Vec<PassengerAgent> {
        std::mem::take(&mut self.passengers)
    }

    /// Removes the passengers for which `filter` is true, e.g. those who alight, and keeps the
    /// order of the others.
    pub fn extract_passengers(
        &mut self,
        mut filter: impl FnMut(&mut PassengerAgent) -> bool,
    ) -> Vec<PassengerAgent> {
        self.passengers
            .extract_if(.., |passenger| filter(passenger))
            .collect()
    }

    /// Drops the vehicle and returns its driver, e.g. after the last stop. Passengers still in the
    /// vehicle are dropped as well.
    pub fn into_driver(self) -> TransitDriverAgent {
        self.driver
    }
}
