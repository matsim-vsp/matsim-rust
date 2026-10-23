use crate::simulation::Identifiable;
use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, LinkEnterEvent, PersonArrivalEvent, PersonDepartureEvent,
    PersonEntersVehicleEvent, PersonLeavesVehicleEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent, TeleportationArrivalEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeEntity;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::backpacking::backpack::{Backpack, PersonExperience};
use crate::simulation::scoring::partial_plans::PartialPlan;
use nohash_hasher::{IntMap, IntSet};

pub(crate) struct BackpackingAttachment {
    backpacks: Vec<Backpack>,
}

pub struct BackpackingDataCollector {
    home_person_ids: Vec<Id<InternalPerson>>,
    person_id2backpack: IntMap<Id<InternalPerson>, Backpack>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
}

impl BackpackingDataCollector {
    pub fn new(home_person_ids: Vec<Id<InternalPerson>>) -> Self {
        Self {
            home_person_ids,
            person_id2backpack: Default::default(),
            vehicle_id2person_ids: Default::default(),
        }
    }

    #[hotpath::measure]
    pub(crate) fn reset_iteration(&mut self) {
        self.person_id2backpack.clear();
        for person in &self.home_person_ids {
            self.person_id2backpack
                .insert(person.clone(), Backpack::new(person.clone()));
        }
        self.vehicle_id2person_ids.clear();
    }

    /// Records the person as an occupant of the vehicle and forwards the event to its backpack.
    #[hotpath::measure]
    pub(crate) fn person_enters_vehicle(&mut self, event: &PersonEntersVehicleEvent) {
        self.vehicle_id2person_ids
            .entry(event.vehicle.clone())
            .or_default()
            .insert(event.person.clone());
        self.partial_plan(&event.person)
            .person_enters_vehicle(event);
    }

    /// Removes the person from the occupants of the vehicle and forwards the event to its backpack.
    #[hotpath::measure]
    pub(crate) fn person_leaves_vehicle(&mut self, event: &PersonLeavesVehicleEvent) {
        let remove_vehicle = self
            .vehicle_id2person_ids
            .get_mut(&event.vehicle)
            .map(|persons| {
                persons.remove(&event.person);
                persons.is_empty()
            })
            .unwrap_or(false);
        if remove_vehicle {
            self.vehicle_id2person_ids.remove(&event.vehicle);
        }
        self.partial_plan(&event.person)
            .person_leaves_vehicle(event);
    }

    pub(crate) fn person_departure(&mut self, event: &PersonDepartureEvent) {
        self.partial_plan(&event.person).person_departure(event);
    }

    pub(crate) fn person_arrival(&mut self, event: &PersonArrivalEvent) {
        self.partial_plan(&event.person).person_arrival(event);
    }

    pub(crate) fn activity_start(&mut self, event: &ActivityStartEvent) {
        self.partial_plan(&event.person).activity_start(event);
    }

    pub(crate) fn activity_end(&mut self, event: &ActivityEndEvent) {
        self.partial_plan(&event.person).activity_end(event);
    }

    pub(crate) fn teleportation_arrival(&mut self, event: &TeleportationArrivalEvent) {
        self.partial_plan(&event.person)
            .teleportation_arrival(event);
    }

    pub(crate) fn pt_teleportation_arrival(&mut self, event: &PtTeleportationArrivalEvent) {
        self.partial_plan(&event.person)
            .pt_teleportation_arrival(event);
    }

    pub(crate) fn person_stuck(&mut self, event: &PersonStuckEvent) {
        self.partial_plan(&event.person).person_stuck(event);
    }

    pub(crate) fn link_enter(&mut self, event: &LinkEnterEvent) {
        self.for_each_occupant(&event.vehicle, |plan| plan.link_enter(event));
    }

    pub(crate) fn vehicle_enters_traffic(&mut self, event: &VehicleEntersTrafficEvent) {
        self.for_each_occupant(&event.vehicle, |plan| plan.vehicle_enters_traffic(event));
    }

    pub(crate) fn vehicle_leaves_traffic(&mut self, event: &VehicleLeavesTrafficEvent) {
        self.for_each_occupant(&event.vehicle, |plan| plan.vehicle_leaves_traffic(event));
    }

    /// Passes an event of a vehicle to the backpacks of all its occupants. Events of vehicles
    /// without occupants on this partition are ignored.
    fn for_each_occupant(
        &mut self,
        vehicle: &Id<InternalVehicle>,
        mut handle: impl FnMut(&mut PartialPlan),
    ) {
        let Some(persons) = self.vehicle_id2person_ids.get(vehicle) else {
            return;
        };
        for person in persons {
            handle(Self::backpack(&mut self.person_id2backpack, person).partial_plan_mut());
        }
    }

    fn partial_plan(&mut self, person: &Id<InternalPerson>) -> &mut PartialPlan {
        Self::backpack(&mut self.person_id2backpack, person).partial_plan_mut()
    }

    fn backpack<'a>(
        backpacks: &'a mut IntMap<Id<InternalPerson>, Backpack>,
        person: &Id<InternalPerson>,
    ) -> &'a mut Backpack {
        backpacks.get_mut(person).unwrap_or_else(|| {
            panic!(
                "No backpack is available for person {} while handling an event.",
                person.external()
            )
        })
    }

    #[hotpath::measure]
    pub(crate) fn send(&mut self, entity: PartitionChangeEntity<'_>) -> BackpackingAttachment {
        let person_ids = Self::person_ids(entity);
        if let PartitionChangeEntity::Vehicle(vehicle)
        | PartitionChangeEntity::TransitVehicle(vehicle) = entity
        {
            self.vehicle_id2person_ids.remove(vehicle.id());
        }

        let backpacks = person_ids
            .into_iter()
            .map(|person_id| {
                self.person_id2backpack
                    .remove(&person_id)
                    .unwrap_or_else(|| {
                        panic!(
                            "No backpack is available for departing person {}.",
                            person_id.external()
                        )
                    })
            })
            .collect();
        BackpackingAttachment { backpacks }
    }

    #[hotpath::measure]
    pub(crate) fn receive(
        &mut self,
        entity: PartitionChangeEntity<'_>,
        attachment: BackpackingAttachment,
    ) {
        let expected_person_ids = Self::person_ids(entity);
        assert_eq!(
            attachment.backpacks.len(),
            expected_person_ids.len(),
            "Backpacking attachment contains the wrong number of backpacks."
        );

        for (expected_id, backpack) in expected_person_ids.iter().zip(attachment.backpacks) {
            assert_eq!(
                backpack.person_id(),
                expected_id,
                "Backpacking attachment contains a backpack for the wrong person."
            );
            let previous = self
                .person_id2backpack
                .insert(expected_id.clone(), backpack);
            assert!(
                previous.is_none(),
                "A backpack for arriving person {} is already present.",
                expected_id.external()
            );
        }

        if let PartitionChangeEntity::Vehicle(vehicle) | PartitionChangeEntity::TransitVehicle(vehicle) =
            entity
            // A transit vehicle may travel without passengers.
            && !expected_person_ids.is_empty()
        {
            let persons = expected_person_ids.into_iter().collect();
            let previous = self
                .vehicle_id2person_ids
                .insert(vehicle.id().clone(), persons);
            assert!(
                previous.is_none(),
                "A vehicle mapping for arriving vehicle {} is already present.",
                vehicle.id().external()
            );
        }
    }

    fn person_ids(entity: PartitionChangeEntity<'_>) -> Vec<Id<InternalPerson>> {
        match entity {
            PartitionChangeEntity::Vehicle(vehicle) => std::iter::once(vehicle.driver().id())
                .chain(vehicle.passengers().iter().map(Identifiable::id))
                .cloned()
                .collect(),
            PartitionChangeEntity::TeleportationAgent(agent) => vec![agent.id().clone()],
            // The driver of a transit vehicle is no agent and has no backpack.
            PartitionChangeEntity::TransitVehicle(vehicle) => vehicle
                .passengers()
                .iter()
                .map(Identifiable::id)
                .cloned()
                .collect(),
        }
    }

    #[hotpath::measure]
    pub(crate) fn finish(&mut self) -> IntMap<Id<InternalPerson>, PersonExperience> {
        self.person_id2backpack
            .drain()
            .map(|(person_id, backpack)| (person_id, backpack.finish()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::scenario::vehicles::InternalVehicle;
    use crate::simulation::vehicles::SimulationVehicle;
    use crate::test_utils::create_agent;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn migrates_driver_and_passenger_and_rebuilds_vehicle_mapping() {
        let driver = create_agent(1, vec!["destination"]);
        let passenger = create_agent(2, vec!["destination"]);
        let driver_id = driver.id().clone();
        let passenger_id = passenger.id().clone();
        let vehicle = SimulationVehicle::new(
            InternalVehicle::new(10, 0, 1.0, 1.0),
            Some(driver),
            vec![passenger],
        );
        let mut departing =
            BackpackingDataCollector::new(vec![driver_id.clone(), passenger_id.clone()]);
        departing.reset_iteration();
        departing.vehicle_id2person_ids.insert(
            vehicle.id().clone(),
            [driver_id.clone(), passenger_id.clone()]
                .into_iter()
                .collect(),
        );

        let attachment = departing.send(PartitionChangeEntity::Vehicle(&vehicle));
        assert!(departing.person_id2backpack.is_empty());
        assert!(departing.vehicle_id2person_ids.is_empty());

        let mut arriving = BackpackingDataCollector::new(Vec::new());
        arriving.receive(PartitionChangeEntity::Vehicle(&vehicle), attachment);
        assert_eq!(arriving.person_id2backpack.len(), 2);
        let expected: IntSet<_> = [driver_id, passenger_id].into_iter().collect();
        assert_eq!(
            arriving.vehicle_id2person_ids.get(vehicle.id()).unwrap(),
            &expected
        );
    }

    #[deterministic_id_test]
    fn migrates_only_passengers_of_transit_vehicle() {
        let passenger = create_agent(2, vec!["destination"]);
        let passenger_id = passenger.id().clone();
        let mut vehicle = SimulationVehicle::new_transit(InternalVehicle::new(11, 0, 1.0, 1.0));
        vehicle.add_passenger(passenger);
        let mut departing = BackpackingDataCollector::new(vec![passenger_id.clone()]);
        departing.reset_iteration();
        departing.vehicle_id2person_ids.insert(
            vehicle.id().clone(),
            [passenger_id.clone()].into_iter().collect(),
        );

        let attachment = departing.send(PartitionChangeEntity::TransitVehicle(&vehicle));
        assert!(departing.person_id2backpack.is_empty());
        assert!(departing.vehicle_id2person_ids.is_empty());

        let mut arriving = BackpackingDataCollector::new(Vec::new());
        arriving.receive(PartitionChangeEntity::TransitVehicle(&vehicle), attachment);
        assert_eq!(arriving.person_id2backpack.len(), 1);
        let expected: IntSet<_> = [passenger_id].into_iter().collect();
        assert_eq!(
            arriving.vehicle_id2person_ids.get(vehicle.id()).unwrap(),
            &expected
        );

        // An empty transit vehicle carries no backpacks and creates no mapping.
        let empty = SimulationVehicle::new_transit(InternalVehicle::new(12, 0, 1.0, 1.0));
        let attachment = arriving.send(PartitionChangeEntity::TransitVehicle(&empty));
        let mut other = BackpackingDataCollector::new(Vec::new());
        other.receive(PartitionChangeEntity::TransitVehicle(&empty), attachment);
        assert!(other.vehicle_id2person_ids.is_empty());
    }
}
