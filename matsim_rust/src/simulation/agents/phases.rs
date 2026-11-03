//! Owned capabilities: the engine queue carries the current interface.
//!
//! The same allocation, including decorators and adaptive state, moves between roles.
//! Phase and role traits expose no plan types and have no default methods. A wrapper
//! therefore decides explicitly for every capability whether it forwards or changes it.

use super::SimulationAgentLogic;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::transit::{
    TransitDeparture, TransitLine, TransitRoute, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

pub type ActivityAgent = Box<dyn ActivityLogic>;
pub type LegAgent = Box<dyn LegLogic>;
pub type NetworkDriverAgent = Box<dyn NetworkDriverLogic>;
pub type TeleportationAgent = Box<dyn TeleportationLogic>;
pub type PassengerAgent = Box<dyn PassengerLogic>;
pub type StuckAgent = Box<dyn StuckLogic>;
pub type TransitDriverAgent = Box<dyn TransitDriverLogic>;

/// What an agent does after a phase. Only the agent decides this; the simulation carries it out.
#[derive(Debug)]
pub enum NextPhase {
    Activity(ActivityAgent),
    Leg(LegAgent),
    /// The agent gives up and takes no further part in the simulation.
    Stuck(StuckAgent),
}

#[cfg(test)]
impl NextPhase {
    pub fn expect_activity(self) -> ActivityAgent {
        match self {
            Self::Activity(agent) => agent,
            next => panic!("Expected an activity, but got {next:?}"),
        }
    }

    pub fn expect_leg(self) -> LegAgent {
        match self {
            Self::Leg(agent) => agent,
            next => panic!("Expected a leg, but got {next:?}"),
        }
    }
}

/// Observations and commands valid throughout an activity, including the final activity.
pub trait ActivityLogic: SimulationAgentLogic {
    fn act_type(&self) -> &Id<String>;
    fn link_id(&self) -> &Id<Link>;
    fn coord(&self) -> &Coordinate;
    fn has_next_leg(&self) -> bool;
    /// Whether the activity engine emits the start and the end of this activity.
    fn has_activity_events(&self) -> bool;
    /// `activity_start` is the time when the activity engine received the agent. An activity with
    /// only a maximum duration may derive its end from it.
    fn end_time(&self, activity_start: SimTime) -> SimTime;
    /// The next time the agent wants to be woken up, or `None` if it wants no wake-up. The engine
    /// asks when the activity starts and after each wake-up, and not in between. A time that is not
    /// after `now` is due in the same tick at the start of the activity and in the next tick after a
    /// wake-up. A wake-up that is due in the tick in which the activity ends comes before the end. A
    /// later one is not delivered.
    fn next_wakeup(&self, now: SimTime) -> Option<SimTime>;

    fn notify_started(&mut self, comp_env: &mut ThreadLocalComputationalEnvironment, now: SimTime);
    // The engine retains the departure time for the wakeup callback contract.
    // I think we should remove this field. This information comes from the agent to the engines and then goes back to the agent. So, it is redundant. paul, mar'26
    fn notify_wakeup(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        end_time: SimTime,
        now: SimTime,
    );
    fn notify_finished(&mut self, now: SimTime);
    fn finish_activity(self: Box<Self>, now: SimTime) -> NextPhase;
}

/// Observations and commands valid throughout a leg. The roles tell which engines can simulate it;
/// the leg engine decides which one does.
pub trait LegLogic: SimulationAgentLogic {
    fn mode(&self) -> &Id<String>;
    fn routing_mode(&self) -> Option<&Id<String>>;
    fn departure_link(&self) -> &Id<Link>;
    fn curr_link_id(&self) -> Option<&Id<Link>>;

    fn try_into_network(self: Box<Self>) -> Result<NetworkDriverAgent, LegAgent>;
    fn try_into_teleportation(self: Box<Self>) -> Result<TeleportationAgent, LegAgent>;
    fn try_into_passenger(self: Box<Self>) -> Result<PassengerAgent, LegAgent>;
    fn try_into_transit_driver(self: Box<Self>) -> Result<TransitDriverAgent, LegAgent>;

    fn finish_leg(self: Box<Self>, now: SimTime) -> NextPhase;
}

/// The driver part of MATSim's `DriverAgent`. Mode and position belong to the leg.
pub trait NetworkDriverLogic: LegLogic {
    fn planned_vehicle_id(&self) -> Option<&Id<InternalVehicle>>;
    fn peek_next_link_id(&self) -> Option<&Id<Link>>;
    fn is_wanting_to_arrive_on_current_link(&self) -> bool;
    fn notify_move_over_node(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        next_link: &Id<Link>,
        now: SimTime,
    );
}

pub trait TeleportationLogic: LegLogic {
    /// The arrival link. It also determines the partition that receives the agent.
    fn destination_link(&self) -> &Id<Link>;
    fn arrival_time(&self, departure: SimTime) -> SimTime;
    fn distance(&self) -> Option<f64>;
    /// `Some` for a PT route, whose arrival event carries the transit data.
    fn transit(&self) -> Option<TransitTeleportation<'_>>;
    fn notify_begin_teleportation(&mut self, now: SimTime);
}

pub trait PassengerLogic: LegLogic {
    /// The stop where the agent waits. Asked once, when the leg starts.
    fn access_stop(&self) -> Id<TransitStopFacility>;
    /// The stop where the agent plans to alight. Asked once, when the leg starts.
    fn egress_stop(&self) -> Id<TransitStopFacility>;
    /// Whether the agent boards a vehicle of `line` that serves `stops_to_come` after this stop.
    fn wants_to_board(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        line: &Id<TransitLine>,
        stops_to_come: &[Id<TransitStopFacility>],
        now: SimTime,
    ) -> bool;
    /// Whether the agent alights at `stop`. Before its planned stop, this ends the leg early.
    fn wants_to_alight(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        stop: &Id<TransitStopFacility>,
        now: SimTime,
    ) -> bool;
    /// The agent has left the vehicle at the stop where it alighted.
    fn notify_left_vehicle(&mut self, now: SimTime);
}

/// The final phase. A stuck agent receives no further calls and is materialized after mobsim.
pub trait StuckLogic: SimulationAgentLogic {
    /// The link where the agent got stuck, if it was on a leg.
    fn stuck_link(&self) -> Option<&Id<Link>>;
    /// The mode of the leg on which the agent got stuck, if it was on a leg.
    fn stuck_mode(&self) -> Option<&Id<String>>;
}

/// The leg of a transit driver is one departure. The mode and the links of the leg come from
/// [`LegLogic`]; at the last stop, the current link is the link where the leg ends.
pub trait TransitDriverLogic: LegLogic {
    fn vehicle(&self) -> &Id<InternalVehicle>;
    fn line(&self) -> &Id<TransitLine>;
    /// The route and the departure, for the event of the start of the departure.
    fn route(&self) -> &Id<TransitRoute>;
    fn departure(&self) -> &Id<TransitDeparture>;
    /// The stop at which the vehicle halts or which it approaches.
    fn stop(&self) -> &Id<TransitStopFacility>;
    fn upcoming_stops(&self) -> &[Id<TransitStopFacility>];
    /// The arrival at the current stop while the vehicle approaches it, the departure while it
    /// halts there.
    fn next_time(&self) -> SimTime;
    fn notify_stop_arrived(&mut self, now: SimTime);
    /// The vehicle left the current stop for the next one, which it reaches at `earliest_arrival`
    /// at the earliest. Never called at the last stop.
    fn notify_stop_departed(&mut self, earliest_arrival: SimTime, now: SimTime);
}

/// The data of a PT teleportation arrival event, without exposing the route type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitTeleportation<'a> {
    pub line: &'a Id<TransitLine>,
    pub route: &'a Id<TransitRoute>,
    pub boarding_time: Option<SimTime>,
    pub access_facility: &'a Id<TransitStopFacility>,
    pub egress_facility: &'a Id<TransitStopFacility>,
}
