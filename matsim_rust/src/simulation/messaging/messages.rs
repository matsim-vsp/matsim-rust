use crate::simulation::Identifiable;
use crate::simulation::agents::EndTime;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::engines::teleportation_engine::TeleportedAgent;
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeAttachments;
use crate::simulation::network::sim_network::StorageUpdate;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::time::{SimTime, Tick};
use crate::simulation::vehicles::SimulationVehicle;
use std::cmp::Ordering;

pub enum InternalSimMessage {
    Sync(InternalSyncMessage),
    Barrier,
}

#[derive(Debug)]
pub(crate) struct TeleportationMessage {
    agent: SimulationAgent,
    end_time: SimTime,
    attachments: PartitionChangeAttachments,
}

impl From<TeleportedAgent> for TeleportationMessage {
    fn from(teleported_agent: TeleportedAgent) -> Self {
        Self {
            agent: teleported_agent.agent,
            end_time: teleported_agent.end_time,
            attachments: PartitionChangeAttachments::default(),
        }
    }
}

impl TeleportationMessage {
    #[cfg(test)]
    pub(crate) fn new(agent: SimulationAgent, end_time: SimTime) -> Self {
        Self {
            agent,
            end_time,
            attachments: PartitionChangeAttachments::default(),
        }
    }

    pub(crate) fn agent(&self) -> &SimulationAgent {
        &self.agent
    }

    pub(crate) fn end_time(&self) -> SimTime {
        self.end_time
    }

    pub(crate) fn into_agent(self) -> SimulationAgent {
        self.agent
    }

    pub(crate) fn set_attachments(&mut self, attachments: PartitionChangeAttachments) {
        self.attachments = attachments;
    }

    pub(crate) fn take_attachments(&mut self) -> PartitionChangeAttachments {
        std::mem::replace(&mut self.attachments, PartitionChangeAttachments::default())
    }
}

pub struct VehicleMessage {
    vehicle: SimulationVehicle,
    attachments: PartitionChangeAttachments,
}

impl VehicleMessage {
    pub fn new(vehicle: SimulationVehicle) -> Self {
        Self::with_attachments(vehicle, PartitionChangeAttachments::default())
    }

    pub(crate) fn with_attachments(
        vehicle: SimulationVehicle,
        attachments: PartitionChangeAttachments,
    ) -> Self {
        Self {
            vehicle,
            attachments,
        }
    }

    pub fn vehicle(&self) -> &SimulationVehicle {
        &self.vehicle
    }

    pub(crate) fn into_parts(self) -> (SimulationVehicle, PartitionChangeAttachments) {
        (self.vehicle, self.attachments)
    }
}

impl std::fmt::Debug for VehicleMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VehicleMessage")
            .field("vehicle", &self.vehicle)
            .field("attachments", &self.attachments)
            .finish()
    }
}

impl EndTime for TeleportationMessage {
    fn end_time(&self, _now: SimTime) -> SimTime {
        self.end_time
    }
}

impl Identifiable<InternalPerson> for TeleportationMessage {
    fn id(&self) -> &Id<InternalPerson> {
        self.agent.id()
    }
}

#[derive(Debug)]
pub struct InternalSyncMessage {
    time: Tick,
    from_process: u32,
    to_process: u32,
    vehicles: Vec<VehicleMessage>,
    teleportations: Vec<TeleportationMessage>,
    storage_capacities: Vec<StorageUpdate>,
}

impl InternalSimMessage {
    pub fn sync_message(self) -> InternalSyncMessage {
        match self {
            InternalSimMessage::Sync(m) => m,
            _ => panic!("That message is no sync message."),
        }
    }

    pub fn from_sync_message(m: InternalSyncMessage) -> InternalSimMessage {
        InternalSimMessage::Sync(m)
    }

    pub fn barrier() -> InternalSimMessage {
        InternalSimMessage::Barrier
    }
}

impl InternalSyncMessage {
    pub fn new(time: Tick, from: u32, to: u32) -> Self {
        Self {
            time,
            from_process: from,
            to_process: to,
            vehicles: Vec::new(),
            teleportations: Vec::new(),
            storage_capacities: Vec::new(),
        }
    }

    pub fn add_veh(&mut self, vehicle: VehicleMessage) {
        self.vehicles.push(vehicle);
    }

    pub(crate) fn add_teleportation(&mut self, teleportation: TeleportationMessage) {
        self.teleportations.push(teleportation);
    }

    pub fn add_storage_cap(&mut self, storage_cap: StorageUpdate) {
        self.storage_capacities.push(storage_cap);
    }

    pub fn time(&self) -> Tick {
        self.time
    }

    pub fn from_process(&self) -> u32 {
        self.from_process
    }

    pub fn to_process(&self) -> u32 {
        self.to_process
    }

    #[cfg(test)]
    pub fn vehicles(&self) -> &[VehicleMessage] {
        &self.vehicles
    }

    #[cfg(test)]
    pub fn vehicles_mut(&mut self) -> &mut Vec<VehicleMessage> {
        &mut self.vehicles
    }

    #[cfg(test)]
    pub(crate) fn teleportations(&self) -> &[TeleportationMessage] {
        &self.teleportations
    }

    pub fn storage_capacities(&self) -> &Vec<StorageUpdate> {
        &self.storage_capacities
    }

    pub fn take_storage_capacities(&mut self) -> Vec<StorageUpdate> {
        std::mem::take(&mut self.storage_capacities)
    }

    pub fn take_vehicles(&mut self) -> Vec<VehicleMessage> {
        std::mem::take(&mut self.vehicles)
    }

    pub(crate) fn take_teleportations(&mut self) -> Vec<TeleportationMessage> {
        std::mem::take(&mut self.teleportations)
    }
}

impl PartialEq for InternalSyncMessage {
    fn eq(&self, other: &Self) -> bool {
        self.time == other.time
    }
}

// Implementation for ordering, so that vehicle messages can be put into a message queue sorted by time
impl PartialOrd for InternalSyncMessage {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for InternalSyncMessage {}

impl Ord for InternalSyncMessage {
    fn cmp(&self, other: &Self) -> Ordering {
        other.time.cmp(&self.time)
    }
}

#[cfg(test)]
mod tests {
    use super::{InternalSyncMessage, TeleportationMessage, VehicleMessage};
    use crate::simulation::Identifiable;
    use crate::simulation::messaging::partition_change::{
        PartitionChangeContext, PartitionChangeEntity, PartitionChangeExtensionsManager,
    };
    use crate::simulation::time::{SimTime, Tick};
    use crate::simulation::vehicles::SimulationVehicle;
    use crate::test_utils::create_agent;
    use macros::deterministic_id_test;
    use std::cell::RefCell;
    use std::rc::Rc;

    #[deterministic_id_test]
    fn sync_message_preserves_scheduled_teleportation() {
        let agent = create_agent(7, vec!["destination"]);
        let end_time = SimTime::from_nanos(12_345_678_900);
        let mut message = InternalSyncMessage::new(Tick::new(2), 1, 3);
        let context = PartitionChangeContext {
            time: SimTime::from_secs(2),
            from: 1,
            to: 3,
        };
        let received = Rc::new(RefCell::new(None));
        let arrival_received = received.clone();
        let mut manager = PartitionChangeExtensionsManager::new();
        manager.register::<String, _, _>(
            |_, _| Some("teleportation attachment".to_owned()),
            move |_, attachment, _| *arrival_received.borrow_mut() = attachment,
        );

        let mut teleportation = TeleportationMessage::new(agent, end_time);
        let attachments = manager.send(
            PartitionChangeEntity::TeleportationAgent(teleportation.agent()),
            &context,
        );
        teleportation.set_attachments(attachments);
        message.add_teleportation(teleportation);

        assert_eq!(message.teleportations().len(), 1);
        assert_eq!(message.teleportations()[0].id().external(), "7");
        assert_eq!(message.teleportations()[0].end_time(), end_time);

        let mut teleportation = message.take_teleportations().pop().unwrap();
        assert_eq!(teleportation.id().external(), "7");
        assert_eq!(teleportation.end_time(), end_time);
        assert!(message.teleportations().is_empty());
        let attachments = teleportation.take_attachments();
        manager.receive(
            PartitionChangeEntity::TeleportationAgent(teleportation.agent()),
            attachments,
            &context,
        );
        assert_eq!(
            received.borrow().as_deref(),
            Some("teleportation attachment")
        );
    }

    #[deterministic_id_test]
    fn sync_message_preserves_vehicle_attachments() {
        let agent = create_agent(8, vec!["destination"]);
        let vehicle = SimulationVehicle::from_parts(8, 0, 1.0, 1.0, agent);
        let context = PartitionChangeContext {
            time: SimTime::from_secs(2),
            from: 1,
            to: 3,
        };
        let received = Rc::new(RefCell::new(None));
        let arrival_received = received.clone();
        let mut manager = PartitionChangeExtensionsManager::new();
        manager.register::<String, _, _>(
            |_, _| Some("vehicle attachment".to_owned()),
            move |_, attachment, _| *arrival_received.borrow_mut() = attachment,
        );
        let attachments = manager.send(PartitionChangeEntity::Vehicle(&vehicle), &context);
        let mut message = InternalSyncMessage::new(Tick::new(2), 1, 3);
        message.add_veh(VehicleMessage::with_attachments(vehicle, attachments));

        let vehicle_message = message.take_vehicles().pop().unwrap();
        let (vehicle, attachments) = vehicle_message.into_parts();
        manager.receive(
            PartitionChangeEntity::Vehicle(&vehicle),
            attachments,
            &context,
        );

        assert_eq!(vehicle.id().external(), "8");
        assert_eq!(received.borrow().as_deref(), Some("vehicle attachment"));
    }
}
