use crate::simulation::Identifiable;
use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, LinkEnterEvent, PersonArrivalEvent, PersonDepartureEvent,
    PersonEntersVehicleEvent, PersonLeavesVehicleEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent, TeleportationArrivalEvent, TransitDriverStartsEvent,
    VehicleArrivesAtFacilityEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeEntity;
use crate::simulation::pt::schedule::TransitSimSchedule;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::backpacking::backpack::{Backpack, PersonExperience};
use crate::simulation::scoring::partial_plans::PartialPlan;
use nohash_hasher::{IntMap, IntSet};
use std::sync::Arc;

pub(crate) struct BackpackingAttachment {
    backpacks: Vec<Backpack>,
    /// The state of a transit vehicle, which moves with it.
    transit_vehicle: Option<TransitVehicleState>,
}

/// What the collector knows about a vehicle of schedule-based transit, like MATSim's
/// `EventsToLegs` keeps for transit vehicles.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TransitVehicleState {
    /// Index into [`TransitSimSchedule::routes`].
    route: usize,
    /// Index of the stop at which the vehicle arrived last.
    stop: Option<usize>,
}

pub struct BackpackingDataCollector {
    home_person_ids: Vec<Id<InternalPerson>>,
    person_id2backpack: IntMap<Id<InternalPerson>, Backpack>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
    /// Empty unless transit is simulated schedule based.
    transit_schedule: Arc<TransitSimSchedule>,
    transit_vehicles: IntMap<Id<InternalVehicle>, TransitVehicleState>,
}

impl BackpackingDataCollector {
    pub fn new(
        home_person_ids: Vec<Id<InternalPerson>>,
        transit_schedule: Arc<TransitSimSchedule>,
    ) -> Self {
        Self {
            home_person_ids,
            person_id2backpack: Default::default(),
            vehicle_id2person_ids: Default::default(),
            transit_schedule,
            transit_vehicles: Default::default(),
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
        self.transit_vehicles.clear();
    }

    /// Records the person as an occupant of the vehicle and forwards the event to its backpack.
    #[hotpath::measure]
    pub(crate) fn person_enters_vehicle(&mut self, event: &PersonEntersVehicleEvent) {
        if self.transit_schedule.is_driver(&event.person) {
            return;
        }
        self.vehicle_id2person_ids
            .entry(event.vehicle.clone())
            .or_default()
            .insert(event.person.clone());
        match self.transit_vehicles.get(&event.vehicle).copied() {
            Some(state) => {
                let route = self.transit_schedule.route(state.route);
                let stop = &route.stops[Self::current_stop(state, event)];
                Self::backpack(&mut self.person_id2backpack, &event.person)
                    .partial_plan_mut()
                    .board_transit_vehicle(event, route, stop);
            }
            None => self
                .partial_plan(&event.person)
                .person_enters_vehicle(event),
        }
    }

    /// Removes the person from the occupants of the vehicle and forwards the event to its backpack.
    #[hotpath::measure]
    pub(crate) fn person_leaves_vehicle(&mut self, event: &PersonLeavesVehicleEvent) {
        if self.transit_schedule.is_driver(&event.person) {
            // The driver leaves at the end of the departure.
            self.transit_vehicles.remove(&event.vehicle);
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
        match self.transit_vehicles.get(&event.vehicle).copied() {
            Some(state) => {
                let route = self.transit_schedule.route(state.route);
                let stop = &route.stops[Self::current_stop(state, event)];
                Self::backpack(&mut self.person_id2backpack, &event.person)
                    .partial_plan_mut()
                    .alight_transit_vehicle(event, stop);
            }
            None => self
                .partial_plan(&event.person)
                .person_leaves_vehicle(event),
        }
    }

    pub(crate) fn person_departure(&mut self, event: &PersonDepartureEvent) {
        if self.transit_schedule.is_driver(&event.person) {
            return;
        }
        self.partial_plan(&event.person).person_departure(event);
    }

    pub(crate) fn person_arrival(&mut self, event: &PersonArrivalEvent) {
        if self.transit_schedule.is_driver(&event.person) {
            return;
        }
        self.partial_plan(&event.person).person_arrival(event);
    }

    pub(crate) fn transit_driver_starts(&mut self, event: &TransitDriverStartsEvent) {
        let route = self
            .transit_schedule
            .find_route_index(
                event.transit_line.external(),
                event.transit_route.external(),
            )
            .unwrap_or_else(|| {
                panic!(
                    "Transit route {} of line {} is not in the schedule.",
                    event.transit_route, event.transit_line
                )
            });
        self.transit_vehicles.insert(
            event.vehicle.clone(),
            TransitVehicleState { route, stop: None },
        );
    }

    pub(crate) fn vehicle_arrives_at_facility(&mut self, event: &VehicleArrivesAtFacilityEvent) {
        let Some(state) = self.transit_vehicles.get_mut(&event.vehicle) else {
            return;
        };
        let stops = &self.transit_schedule.route(state.route).stops;
        // Routes may serve a stop twice, so search after the previous stop.
        let from = state.stop.map_or(0, |stop| stop + 1);
        let offset = stops[from..]
            .iter()
            .position(|stop| stop.facility == event.facility)
            .unwrap_or_else(|| {
                panic!(
                    "Transit vehicle {} arrives at stop {}, which its route does not serve next.",
                    event.vehicle, event.facility
                )
            });
        state.stop = Some(from + offset);
    }

    fn current_stop(state: TransitVehicleState, event: &impl std::fmt::Debug) -> usize {
        state
            .stop
            .unwrap_or_else(|| panic!("Transit vehicle is at no stop for event {event:?}."))
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
        let transit_vehicle = match entity {
            PartitionChangeEntity::TransitVehicle(vehicle) => {
                self.transit_vehicles.remove(vehicle.id())
            }
            _ => None,
        };

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
        BackpackingAttachment {
            backpacks,
            transit_vehicle,
        }
    }

    #[hotpath::measure]
    pub(crate) fn receive(
        &mut self,
        entity: PartitionChangeEntity<'_>,
        attachment: BackpackingAttachment,
    ) {
        let expected_person_ids = Self::person_ids(entity);
        if let (PartitionChangeEntity::TransitVehicle(vehicle), Some(state)) =
            (entity, attachment.transit_vehicle)
        {
            self.transit_vehicles.insert(vehicle.id().clone(), state);
        }
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
        let mut departing = BackpackingDataCollector::new(
            vec![driver_id.clone(), passenger_id.clone()],
            Default::default(),
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

        let mut arriving = BackpackingDataCollector::new(Vec::new(), Default::default());
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
        let mut departing =
            BackpackingDataCollector::new(vec![passenger_id.clone()], Default::default());
        departing.reset_iteration();
        departing.vehicle_id2person_ids.insert(
            vehicle.id().clone(),
            [passenger_id.clone()].into_iter().collect(),
        );

        let attachment = departing.send(PartitionChangeEntity::TransitVehicle(&vehicle));
        assert!(departing.person_id2backpack.is_empty());
        assert!(departing.vehicle_id2person_ids.is_empty());

        let mut arriving = BackpackingDataCollector::new(Vec::new(), Default::default());
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
        let mut other = BackpackingDataCollector::new(Vec::new(), Default::default());
        other.receive(PartitionChangeEntity::TransitVehicle(&empty), attachment);
        assert!(other.vehicle_id2person_ids.is_empty());
    }

    /// A passenger boards on one partition and alights on another. The state of the transit
    /// vehicle moves with it, so that the second partition knows the route and the stop.
    #[deterministic_id_test]
    fn transit_vehicle_state_moves_with_vehicle() {
        use crate::simulation::events::{
            ActivityEndEventBuilder, PersonArrivalEventBuilder, PersonDepartureEventBuilder,
            PersonEntersVehicleEventBuilder, PersonLeavesVehicleEventBuilder,
            TransitDriverStartsEventBuilder, VehicleArrivesAtFacilityEventBuilder,
        };
        use crate::simulation::scenario::Coordinate;
        use crate::simulation::scenario::network::Network;
        use crate::simulation::scenario::population::{
            InternalPlanElement, InternalRoute, Population,
        };
        use crate::simulation::scenario::transit::TransitSchedule;
        use crate::simulation::scenario::vehicles::Garage;
        use crate::simulation::time::SimTime;
        use crate::test_utils::create_pt_agent;
        use std::path::{Path, PathBuf};

        let network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        let schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let schedule = Arc::new(TransitSimSchedule::build(
            &schedule,
            &network,
            &garage,
            &Population::default(),
        ));
        let agent = create_pt_agent(1, "Blue Line", "1to3", "1", "3", "11", "33");
        let person = agent.id().clone();
        let vehicle_id = Id::<InternalVehicle>::get_from_ext("tr_1");
        let time = SimTime::from_secs;
        let arrives = |facility: &str| {
            VehicleArrivesAtFacilityEventBuilder::default()
                .time(time(21600))
                .vehicle(vehicle_id.clone())
                .facility(Id::get_from_ext(facility))
                .build()
                .unwrap()
        };

        let mut departing = BackpackingDataCollector::new(vec![person.clone()], schedule.clone());
        departing.reset_iteration();
        departing.activity_end(
            &ActivityEndEventBuilder::default()
                .time(time(21000))
                .person(person.clone())
                .link(Id::create("11"))
                .coordinate(Coordinate::default())
                .act_type(Id::create("home"))
                .build()
                .unwrap(),
        );
        departing.person_departure(
            &PersonDepartureEventBuilder::default()
                .time(time(21000))
                .person(person.clone())
                .link(Id::create("11"))
                .leg_mode(Id::create("pt"))
                .routing_mode(Some(Id::create("pt")))
                .build()
                .unwrap(),
        );
        departing.transit_driver_starts(
            &TransitDriverStartsEventBuilder::default()
                .time(time(21600))
                .driver(schedule.departures()[0].driver.clone())
                .vehicle(vehicle_id.clone())
                .transit_line(Id::get_from_ext("Blue Line"))
                .transit_route(Id::get_from_ext("1to3"))
                .departure(Id::get_from_ext("01"))
                .build()
                .unwrap(),
        );
        departing.vehicle_arrives_at_facility(&arrives("1"));
        departing.person_enters_vehicle(
            &PersonEntersVehicleEventBuilder::default()
                .time(time(21600))
                .person(person.clone())
                .vehicle(vehicle_id.clone())
                .build()
                .unwrap(),
        );

        let mut vehicle =
            SimulationVehicle::new_transit(garage.vehicles.get(&vehicle_id).unwrap().clone());
        vehicle.add_passenger(agent);
        let attachment = departing.send(PartitionChangeEntity::TransitVehicle(&vehicle));
        assert!(departing.transit_vehicles.is_empty());

        let mut arriving = BackpackingDataCollector::new(Vec::new(), schedule);
        arriving.receive(PartitionChangeEntity::TransitVehicle(&vehicle), attachment);
        assert_eq!(
            Some(&TransitVehicleState {
                route: 0,
                stop: Some(0)
            }),
            arriving.transit_vehicles.get(&vehicle_id)
        );

        arriving.vehicle_arrives_at_facility(&arrives("2a"));
        arriving.vehicle_arrives_at_facility(&arrives("3"));
        arriving.person_leaves_vehicle(
            &PersonLeavesVehicleEventBuilder::default()
                .time(time(22140))
                .person(person.clone())
                .vehicle(vehicle_id.clone())
                .build()
                .unwrap(),
        );
        arriving.person_arrival(
            &PersonArrivalEventBuilder::default()
                .time(time(22140))
                .person(person.clone())
                .link(Id::create("33"))
                .leg_mode(Id::create("pt"))
                .build()
                .unwrap(),
        );

        let experiences = arriving.finish();
        let InternalPlanElement::Leg(leg) = &experiences[&person].plan().elements[1] else {
            panic!("Expected a leg");
        };
        let Some(InternalRoute::Pt(route)) = &leg.route else {
            panic!("Expected a transit route");
        };
        assert_eq!("1", route.description.access_facility_id);
        assert_eq!("3", route.description.egress_facility_id);
        assert_eq!(Some(6100.0), route.generic_delegate().distance());
    }
}
