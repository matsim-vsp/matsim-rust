//! Borrowed capabilities of a plan agent. The owner stays boxed across all transitions.

use super::EndTime;
use crate::simulation::controller::ThreadLocalComputationalEnvironment;
use crate::simulation::id::Id;
use crate::simulation::scenario::network::Link;
use crate::simulation::scenario::population::{
    InternalActivity, InternalLeg, InternalPerson, InternalRoute,
};
use crate::simulation::time::SimTime;

pub enum AgentPhase<'a> {
    Activity(&'a dyn ActivityLogic),
    Leg(&'a dyn LegLogic),
}

impl<'a> AgentPhase<'a> {
    pub fn expect_activity(self, id: &Id<InternalPerson>) -> &'a dyn ActivityLogic {
        match self {
            Self::Activity(logic) => logic,
            Self::Leg(_) => panic!("Agent {id}: expected Activity phase, found Leg"),
        }
    }

    #[inline]
    pub fn expect_leg(self, id: &Id<InternalPerson>) -> &'a dyn LegLogic {
        match self {
            Self::Leg(logic) => logic,
            Self::Activity(_) => panic!("Agent {id}: expected Leg phase, found Activity"),
        }
    }
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

    #[inline]
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

/// Observations valid throughout an activity, including the final activity.
pub trait ActivityLogic: EndTime {
    fn curr_act(&self) -> &InternalActivity;
    fn next_act(&self) -> Option<&InternalActivity>;
    fn next_leg(&self) -> Option<&InternalLeg>;
    fn wakeup_time(&self, now: SimTime) -> SimTime;
}

pub trait ActivityCommands: ActivityLogic {
    fn on_started(&mut self, _comp_env: &mut ThreadLocalComputationalEnvironment, _now: SimTime) {}
    fn on_wakeup(
        &mut self,
        _comp_env: &mut ThreadLocalComputationalEnvironment,
        _end_time: SimTime,
        _now: SimTime,
    ) {
    }
    fn on_finished(&mut self, _now: SimTime) {}
    fn start_next_leg(&mut self, now: SimTime);
}

/// A mutable activity capability; changing phase consumes this borrow, not the agent.
pub struct ActivityMut<'a> {
    commands: &'a mut dyn ActivityCommands,
    agent_id: u64,
}

impl<'a> ActivityMut<'a> {
    #[inline]
    pub fn new(commands: &'a mut dyn ActivityCommands, agent_id: u64) -> Self {
        Self { commands, agent_id }
    }

    pub fn logic(&self) -> &dyn ActivityLogic {
        self.commands
    }

    pub fn on_started(&mut self, comp_env: &mut ThreadLocalComputationalEnvironment, now: SimTime) {
        self.commands.on_started(comp_env, now);
    }

    pub fn on_wakeup(
        &mut self,
        comp_env: &mut ThreadLocalComputationalEnvironment,
        end_time: SimTime,
        now: SimTime,
    ) {
        // The engine's departure time is retained here to preserve the wakeup callback contract.
        // I think we should remove this field. This information comes from the agent to the engines and then goes back to the agent. So, it is redundant. paul, mar'26
        self.commands.on_wakeup(comp_env, end_time, now);
    }

    pub fn on_finished(&mut self, now: SimTime) {
        self.commands.on_finished(now);
    }

    pub fn start_next_leg(self, now: SimTime) {
        self.commands.start_next_leg(now);
    }
}

pub trait LegLogic {
    fn curr_leg(&self) -> &InternalLeg;
    fn route(&self) -> &InternalRoute;
    fn curr_link_id(&self) -> Option<&Id<Link>>;

    fn network_driver(&self) -> Option<&dyn NetworkDriverLogic> {
        None
    }

    fn teleportation(&self) -> Option<&dyn TeleportationLogic> {
        None
    }
}

pub trait LegCommands: LegLogic {
    fn start_next_activity(&mut self, now: SimTime);

    fn network_driver_mut(&mut self) -> Option<&mut dyn NetworkDriverCommands> {
        None
    }

    fn teleportation_mut(&mut self) -> Option<&mut dyn TeleportationCommands> {
        None
    }

    /// Existing vehicle occupants receive link movements even when their own route is generic.
    fn vehicle_passenger_mut(&mut self) -> Option<&mut dyn VehiclePassengerCommands> {
        None
    }
}

/// A mutable leg borrow. A role borrow must end before the phase can change.
pub struct LegMut<'a> {
    commands: &'a mut dyn LegCommands,
    agent_id: u64,
}

impl<'a> LegMut<'a> {
    #[inline]
    pub fn new(commands: &'a mut dyn LegCommands, agent_id: u64) -> Self {
        Self { commands, agent_id }
    }

    #[inline]
    pub(super) fn agent_id(&self) -> u64 {
        self.agent_id
    }

    pub fn logic(&self) -> &dyn LegLogic {
        self.commands
    }

    pub fn network_driver(&mut self) -> Option<&mut dyn NetworkDriverCommands> {
        self.commands.network_driver_mut()
    }

    pub fn teleportation(&mut self) -> Option<&mut dyn TeleportationCommands> {
        self.commands.teleportation_mut()
    }

    #[inline]
    pub fn into_network_driver(self) -> Option<&'a mut dyn NetworkDriverCommands> {
        self.commands.network_driver_mut()
    }

    pub fn into_teleportation(self) -> Option<&'a mut dyn TeleportationCommands> {
        self.commands.teleportation_mut()
    }

    pub fn into_vehicle_passenger(self) -> Option<&'a mut dyn VehiclePassengerCommands> {
        self.commands.vehicle_passenger_mut()
    }

    pub fn start_next_activity(self, now: SimTime) {
        self.commands.start_next_activity(now);
    }
}

/// Network driving does not require a person or an activity plan.
pub trait NetworkDriverLogic {
    fn curr_link_id(&self) -> Option<&Id<Link>>;
    fn peek_next_link_id(&self) -> Option<&Id<Link>>;
    fn mode(&self) -> &Id<String>;

    fn is_wanting_to_arrive_on_current_link(&self) -> bool {
        self.peek_next_link_id().is_none()
    }
}

pub trait NetworkDriverCommands: NetworkDriverLogic {
    /// Commits the link previously returned by `peek_next_link_id`.
    fn leave_link(&mut self, now: SimTime);
}

/// Preserves route-position notifications for existing vehicle passengers. This adds no boarding,
/// transit routing or passenger engine; those need their own future roles.
pub trait VehiclePassengerCommands {
    fn leave_link(&mut self, now: SimTime);
}

pub trait TeleportationLogic: EndTime {
    fn mode(&self) -> &Id<String>;
    fn route(&self) -> &InternalRoute;

    fn destination_link(&self) -> &Id<Link> {
        self.route().end_link()
    }
}

pub trait TeleportationCommands: TeleportationLogic {
    fn begin_teleportation(&mut self, now: SimTime);
}
