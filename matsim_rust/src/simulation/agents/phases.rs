//! Borrowed capabilities of a plan agent. The owner stays boxed across all transitions.
//!
//! Phase and role traits expose no plan types and have no default methods. A wrapper therefore
//! decides explicitly for every capability whether it forwards or changes it.

use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::time::SimTime;

pub enum AgentPhase<'a> {
    Activity(&'a dyn ActivityLogic),
    Leg(&'a dyn LegLogic),
}

pub enum AgentPhaseMut<'a> {
    Activity(ActivityMut<'a>),
    Leg(LegMut<'a>),
}

impl<'a> AgentPhaseMut<'a> {
    pub fn expect_activity(self) -> ActivityMut<'a> {
        match self {
            Self::Activity(logic) => logic,
            Self::Leg(logic) => panic!(
                "Agent {}: expected Activity phase, found Leg",
                Id::<InternalPerson>::get(logic.agent_id)
            ),
        }
    }

    pub fn expect_leg(self) -> LegMut<'a> {
        match self {
            Self::Leg(logic) => logic,
            Self::Activity(logic) => panic!(
                "Agent {}: expected Leg phase, found Activity",
                Id::<InternalPerson>::get(logic.agent_id)
            ),
        }
    }
}

/// Observations and commands valid throughout an activity, including the final activity.
pub trait ActivityLogic {
    fn act_type(&self) -> &Id<String>;
    fn link_id(&self) -> &Id<Link>;
    fn coord(&self) -> &Coordinate;
    fn has_next_leg(&self) -> bool;
    /// `activity_start` is the time when the activity engine received the agent. An activity with
    /// only a maximum duration may derive its end from it.
    fn end_time(&self, activity_start: SimTime) -> SimTime;
    fn wakeup_time(&self, now: SimTime) -> SimTime;

    fn on_started(&mut self, comp_env: &mut ThreadLocalComputationalEnvironment, now: SimTime);
    fn on_wakeup(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        end_time: SimTime,
        now: SimTime,
    );
    fn on_finished(&mut self, now: SimTime);
    fn start_next_leg(&mut self, now: SimTime);
}

/// A mutable activity capability; changing phase consumes this borrow, not the agent.
pub struct ActivityMut<'a> {
    logic: &'a mut dyn ActivityLogic,
    agent_id: u64,
}

impl<'a> ActivityMut<'a> {
    pub fn new(logic: &'a mut dyn ActivityLogic, agent_id: u64) -> Self {
        Self { logic, agent_id }
    }

    pub fn on_started(&mut self, comp_env: &mut ThreadLocalComputationalEnvironment, now: SimTime) {
        self.logic.on_started(comp_env, now);
    }

    pub fn on_wakeup(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        end_time: SimTime,
        now: SimTime,
    ) {
        // The engine's departure time is retained here to preserve the wakeup callback contract.
        // I think we should remove this field. This information comes from the agent to the engines and then goes back to the agent. So, it is redundant. paul, mar'26
        self.logic.on_wakeup(comp_env, end_time, now);
    }

    pub fn on_finished(&mut self, now: SimTime) {
        self.logic.on_finished(now);
    }

    pub fn start_next_leg(self, now: SimTime) {
        self.logic.start_next_leg(now);
    }
}

/// Observations and commands valid throughout a leg. The roles tell which engines can simulate it;
/// the leg engine decides which one does.
pub trait LegLogic {
    fn mode(&self) -> &Id<String>;
    fn routing_mode(&self) -> Option<&Id<String>>;
    fn departure_link(&self) -> &Id<Link>;
    /// The person's position. For a network driver, this is the link of its vehicle.
    fn curr_link_id(&self) -> Option<&Id<Link>>;

    fn network_driver(&self) -> Option<&dyn NetworkDriverLogic>;
    fn network_driver_mut(&mut self) -> Option<&mut dyn NetworkDriverLogic>;
    fn teleportation(&self) -> Option<&dyn TeleportationLogic>;
    fn teleportation_mut(&mut self) -> Option<&mut dyn TeleportationLogic>;

    fn start_next_activity(&mut self, now: SimTime);
}

/// A mutable leg borrow. A role borrow must end before the phase can change.
pub struct LegMut<'a> {
    logic: &'a mut dyn LegLogic,
    agent_id: u64,
}

impl<'a> LegMut<'a> {
    pub fn new(logic: &'a mut dyn LegLogic, agent_id: u64) -> Self {
        Self { logic, agent_id }
    }

    pub(super) fn agent_id(&self) -> u64 {
        self.agent_id
    }

    pub fn into_network_driver(self) -> Option<&'a mut dyn NetworkDriverLogic> {
        self.logic.network_driver_mut()
    }

    pub fn into_teleportation(self) -> Option<&'a mut dyn TeleportationLogic> {
        self.logic.teleportation_mut()
    }

    pub fn start_next_activity(self, now: SimTime) {
        self.logic.start_next_activity(now);
    }
}

/// The driver part of MATSim's `DriverAgent`. Mode and position belong to the leg.
pub trait NetworkDriverLogic {
    fn planned_vehicle_id(&self) -> Option<&Id<InternalVehicle>>;
    /// MATSim's `chooseNextLinkId`. `None` means that the route ends on the current link.
    fn peek_next_link_id(&self) -> Option<&Id<Link>>;
    fn is_wanting_to_arrive_on_current_link(&self) -> bool;
    /// MATSim's `notifyMoveOverNode`. The network calls it when the vehicle leaves its link for
    /// `next_link`, which the network obtained from `peek_next_link_id` and checked before.
    fn move_over_node(&mut self, next_link: &Id<Link>, now: SimTime);
}

pub trait TeleportationLogic {
    /// The arrival link. It also determines the partition that receives the agent.
    fn destination_link(&self) -> &Id<Link>;
    fn arrival_time(&self, departure: SimTime) -> SimTime;
    fn distance(&self) -> Option<f64>;
    /// `Some` for a PT route, whose arrival event carries the transit data.
    fn transit(&self) -> Option<TransitTeleportation<'_>>;
    fn begin_teleportation(&mut self, now: SimTime);
}

/// The data of a PT teleportation arrival event, without exposing the route type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitTeleportation<'a> {
    pub line: &'a str,
    pub route: &'a str,
    pub boarding_time: Option<SimTime>,
    pub access_facility: &'a str,
    pub egress_facility: &'a str,
}
