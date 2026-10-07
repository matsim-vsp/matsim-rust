use crate::simulation::Identifiable;
use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, EventTrait, LinkEnterEvent, PersonArrivalEvent,
    PersonDepartureEvent, PersonEntersVehicleEvent, PersonLeavesVehicleEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent, TeleportationArrivalEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeEntity;
use crate::simulation::pt::runs::TransitVehicleRuns;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::backpacking::backpack::{Backpack, PersonExperience};
use nohash_hasher::{IntMap, IntSet};
use std::sync::Arc;

pub(crate) struct BackpackingAttachment {
    backpacks: Vec<Backpack>,
}

pub struct BackpackingDataCollector {
    home_person_ids: Vec<Id<InternalPerson>>,
    transit_runs: Arc<TransitVehicleRuns>,
    person_id2backpack: IntMap<Id<InternalPerson>, Backpack>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
}

impl BackpackingDataCollector {
    pub fn new(
        home_person_ids: Vec<Id<InternalPerson>>,
        transit_runs: Arc<TransitVehicleRuns>,
    ) -> Self {
        Self {
            home_person_ids,
            transit_runs,
            person_id2backpack: Default::default(),
            vehicle_id2person_ids: Default::default(),
        }
    }

    pub(crate) fn reset_iteration(&mut self) {
        self.person_id2backpack.clear();
        for person in &self.home_person_ids {
            self.person_id2backpack
                .insert(person.clone(), Backpack::new(person.clone()));
        }
        self.vehicle_id2person_ids.clear();
    }

    pub(crate) fn person_enters_vehicle(&mut self, event: &PersonEntersVehicleEvent) {
        if self.transit_runs.is_driver(&event.person) {
            return;
        }
        self.vehicle_id2person_ids
            .entry(event.vehicle.clone())
            .or_default()
            .insert(event.person.clone());
    }

    pub(crate) fn person_leaves_vehicle(&mut self, event: &PersonLeavesVehicleEvent) {
        if self.transit_runs.is_driver(&event.person) {
            return;
        }
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
    }

    /// Forwards simulation events to all backpacks affected by that event.
    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
        let affected_persons = if let Some(event) = event.as_any().downcast_ref::<LinkEnterEvent>()
        {
            self.vehicle_id2person_ids
                .get(&event.vehicle)
                .map(|persons| persons.iter().cloned().collect())
                .unwrap_or_default()
        } else if let Some(event) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<ActivityStartEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<ActivityEndEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<TeleportationArrivalEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<PtTeleportationArrivalEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<PersonEntersVehicleEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<PersonLeavesVehicleEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<PersonStuckEvent>() {
            vec![event.person.clone()]
        } else if let Some(event) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            self.vehicle_id2person_ids
                .get(&event.vehicle)
                .map(|persons| persons.iter().cloned().collect())
                .unwrap_or_default()
        } else if let Some(event) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            self.vehicle_id2person_ids
                .get(&event.vehicle)
                .map(|persons| persons.iter().cloned().collect())
                .unwrap_or_default()
        } else {
            return;
        };

        for person in affected_persons {
            if self.transit_runs.is_driver(&person) {
                continue;
            }
            self.person_id2backpack
                .get_mut(&person)
                .unwrap_or_else(|| {
                    panic!(
                        "No backpack is available for person {} while handling an event.",
                        person.external()
                    )
                })
                .handle_event(event);
        }
    }

    pub(crate) fn send(&mut self, entity: PartitionChangeEntity<'_>) -> BackpackingAttachment {
        let person_ids = Self::person_ids(entity);
        if let PartitionChangeEntity::Vehicle(vehicle) = entity {
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

        if let PartitionChangeEntity::Vehicle(vehicle) = entity {
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
        }
    }

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
        let mut departing = BackpackingDataCollector::new(
            vec![driver_id.clone(), passenger_id.clone()],
            Arc::new(TransitVehicleRuns::default()),
        );
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

        let mut arriving =
            BackpackingDataCollector::new(Vec::new(), Arc::new(TransitVehicleRuns::default()));
        arriving.receive(PartitionChangeEntity::Vehicle(&vehicle), attachment);
        assert_eq!(arriving.person_id2backpack.len(), 2);
        let expected: IntSet<_> = [driver_id, passenger_id].into_iter().collect();
        assert_eq!(
            arriving.vehicle_id2person_ids.get(vehicle.id()).unwrap(),
            &expected
        );
    }
}
