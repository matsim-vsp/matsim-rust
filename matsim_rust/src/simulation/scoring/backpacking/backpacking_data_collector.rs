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
use crate::simulation::scenario::transit::{TransitLine, TransitRoute, TransitStopFacility};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::backpacking::backpack::{Backpack, PersonExperience};
use crate::simulation::scoring::partial_plans::PartialPlan;
use nohash_hasher::{IntMap, IntSet};
use std::sync::Arc;

pub(crate) struct BackpackingAttachment {
    backpacks: Vec<Backpack>,
    /// The state of a transit vehicle, which moves with it.
    transit_vehicle_state: Option<TransitVehicleState>,
}

/// What the collector knows about a vehicle of schedule-based transit, like MATSim's
/// `EventsToLegs` keeps for transit vehicles. It comes from the events only.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TransitVehicleState {
    line: Id<TransitLine>,
    route: Id<TransitRoute>,
    driver: Id<InternalPerson>,
    /// The stop at which the vehicle arrived last.
    stop: Option<Id<TransitStopFacility>>,
}

pub struct BackpackingDataCollector {
    home_person_ids: Vec<Id<InternalPerson>>,
    person_id2backpack: IntMap<Id<InternalPerson>, Backpack>,
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
    transit_vehicle_states: IntMap<Id<InternalVehicle>, TransitVehicleState>,
    /// Drivers of transit vehicles on this partition. They have no backpacks, and their events
    /// are ignored.
    transit_drivers: IntSet<Id<InternalPerson>>,
    transit_schedule: Arc<TransitSimSchedule>,
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
            transit_vehicle_states: Default::default(),
            transit_drivers: Default::default(),
            transit_schedule,
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
        self.transit_vehicle_states.clear();
        self.transit_drivers.clear();
    }

    /// Records the person as an occupant of the vehicle and forwards the event to its backpack.
    #[hotpath::measure]
    pub(crate) fn person_enters_vehicle(&mut self, event: &PersonEntersVehicleEvent) {
        let Some(plan) = Self::plan_unless_driver(
            &mut self.person_id2backpack,
            &self.transit_drivers,
            &event.person,
        ) else {
            return;
        };
        match self.transit_vehicle_states.get(&event.vehicle) {
            Some(state) => plan.board_transit_vehicle(
                event,
                &state.line,
                &state.route,
                Self::current_stop(state, event),
            ),
            None => plan.person_enters_vehicle(event),
        }
        self.vehicle_id2person_ids
            .entry(event.vehicle.clone())
            .or_default()
            .insert(event.person.clone());
    }

    /// Removes the person from the occupants of the vehicle and forwards the event to its backpack.
    #[hotpath::measure]
    pub(crate) fn person_leaves_vehicle(&mut self, event: &PersonLeavesVehicleEvent) {
        let Some(plan) = Self::plan_unless_driver(
            &mut self.person_id2backpack,
            &self.transit_drivers,
            &event.person,
        ) else {
            // The driver leaves at the end of the departure.
            self.transit_vehicle_states.remove(&event.vehicle);
            return;
        };
        match self.transit_vehicle_states.get(&event.vehicle) {
            Some(state) => plan.alight_transit_vehicle(event, Self::current_stop(state, event)),
            None => plan.person_leaves_vehicle(event),
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

    pub(crate) fn person_departure(&mut self, event: &PersonDepartureEvent) {
        if let Some(plan) = Self::plan_unless_driver(
            &mut self.person_id2backpack,
            &self.transit_drivers,
            &event.person,
        ) {
            plan.person_departure(event);
        }
    }

    pub(crate) fn person_arrival(&mut self, event: &PersonArrivalEvent) {
        match Self::plan_unless_driver(
            &mut self.person_id2backpack,
            &self.transit_drivers,
            &event.person,
        ) {
            Some(plan) => plan.person_arrival(event),
            // The driver's departure ends with its arrival.
            None => {
                self.transit_drivers.remove(&event.person);
            }
        }
    }

    pub(crate) fn transit_driver_starts(&mut self, event: &TransitDriverStartsEvent) {
        self.transit_drivers.insert(event.driver.clone());
        self.transit_vehicle_states.insert(
            event.vehicle.clone(),
            TransitVehicleState {
                line: event.transit_line.clone(),
                route: event.transit_route.clone(),
                driver: event.driver.clone(),
                stop: None,
            },
        );
    }

    pub(crate) fn vehicle_arrives_at_facility(&mut self, event: &VehicleArrivesAtFacilityEvent) {
        if let Some(state) = self.transit_vehicle_states.get_mut(&event.vehicle) {
            state.stop = Some(event.facility.clone());
        }
    }

    fn current_stop<'a>(
        state: &'a TransitVehicleState,
        event: &impl std::fmt::Debug,
    ) -> &'a Id<TransitStopFacility> {
        state
            .stop
            .as_ref()
            .unwrap_or_else(|| panic!("Transit vehicle is at no stop for event {event:?}."))
    }

    /// The partial plan of `person`, or `None` for transit drivers. Drivers are only looked up for
    /// persons without a backpack, so that other persons need no additional lookup.
    fn plan_unless_driver<'a>(
        backpacks: &'a mut IntMap<Id<InternalPerson>, Backpack>,
        transit_drivers: &IntSet<Id<InternalPerson>>,
        person: &Id<InternalPerson>,
    ) -> Option<&'a mut PartialPlan> {
        match backpacks.get_mut(person) {
            Some(backpack) => Some(backpack.partial_plan_mut()),
            None if transit_drivers.contains(person) => None,
            None => panic!(
                "No backpack is available for person {} while handling an event.",
                person.external()
            ),
        }
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
                let state = self.transit_vehicle_states.remove(vehicle.id());
                if let Some(state) = &state {
                    self.transit_drivers.remove(&state.driver);
                }
                state
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
            transit_vehicle_state: transit_vehicle,
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
            (entity, attachment.transit_vehicle_state)
        {
            self.transit_drivers.insert(state.driver.clone());
            self.transit_vehicle_states
                .insert(vehicle.id().clone(), state);
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
            // The driver of a transit vehicle is no person of the population and has no backpack.
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
            .map(|(person_id, backpack)| (person_id, backpack.finish(&self.transit_schedule)))
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
        let mut vehicle =
            SimulationVehicle::new(InternalVehicle::new(11, 0, 1.0, 1.0), None, Vec::new());
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
        let empty = SimulationVehicle::new(InternalVehicle::new(12, 0, 1.0, 1.0), None, Vec::new());
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

        let mut vehicle = SimulationVehicle::new(
            garage.vehicles.get(&vehicle_id).unwrap().clone(),
            None,
            Vec::new(),
        );
        vehicle.add_passenger(agent);
        let attachment = departing.send(PartitionChangeEntity::TransitVehicle(&vehicle));
        assert!(departing.transit_vehicle_states.is_empty());

        assert!(departing.transit_drivers.is_empty());

        let driver = schedule.departures()[0].driver.clone();
        let mut arriving = BackpackingDataCollector::new(Vec::new(), schedule);
        arriving.receive(PartitionChangeEntity::TransitVehicle(&vehicle), attachment);
        assert_eq!(
            Some(&TransitVehicleState {
                line: Id::get_from_ext("Blue Line"),
                route: Id::get_from_ext("1to3"),
                driver: driver.clone(),
                stop: Some(Id::get_from_ext("1")),
            }),
            arriving.transit_vehicle_states.get(&vehicle_id)
        );
        assert!(arriving.transit_drivers.contains(&driver));

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

        // The driver has no backpack on this partition; its last events are ignored.
        arriving.person_leaves_vehicle(
            &PersonLeavesVehicleEventBuilder::default()
                .time(time(22140))
                .person(driver.clone())
                .vehicle(vehicle_id.clone())
                .build()
                .unwrap(),
        );
        arriving.person_arrival(
            &PersonArrivalEventBuilder::default()
                .time(time(22140))
                .person(driver.clone())
                .link(Id::create("33"))
                .leg_mode(Id::create("train"))
                .build()
                .unwrap(),
        );
        assert!(arriving.transit_vehicle_states.is_empty());
        assert!(arriving.transit_drivers.is_empty());

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
