use crate::simulation::events::{
    ActivityEndEvent, ActivityStartEvent, LinkEnterEvent, PersonArrivalEvent, PersonDepartureEvent,
    PersonEntersVehicleEvent, PersonLeavesVehicleEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent, TeleportationArrivalEvent, TransitDriverStartsEvent,
    VehicleArrivesAtFacilityEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::messaging::partition_change::PartitionChangeEntity;
use crate::simulation::pt::schedule::TransitSimSchedule;
use crate::simulation::replanning::routing::utils::calc_transit_distance;
use crate::simulation::scenario::network::Network;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{
    RouteIndex, TransitLine, TransitRoute, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::InternalVehicle;
use crate::simulation::scoring::backpacking::backpack::{Backpack, PersonExperience};
use crate::simulation::scoring::partial_plans::{PartialPlan, TransitBoarding};
use nohash_hasher::{IntMap, IntSet};
use std::sync::Arc;

pub(crate) struct BackpackingAttachment {
    backpacks: Vec<Backpack>,
}

/// The general structure is as follows:
/// Before mobsim: create a backpack for each person starting on this partition (using `home_person_ids`).
///
/// During mobsim:
/// - Map person and vehicle events into partial plans.
/// - Keep track of which persons are in which vehicles.
/// - Send backpacks with departing persons to the next partition, and receive backpacks of arriving persons from the previous partition.
pub struct BackpackingDataCollector {
    home_person_ids: Vec<Id<InternalPerson>>,
    person_id2backpack: IntMap<Id<InternalPerson>, Backpack>,
    /// Keeps track of persons being in vehicles.
    vehicle_id2person_ids: IntMap<Id<InternalVehicle>, IntSet<Id<InternalPerson>>>,
    transit_vehicles: IntMap<Id<InternalVehicle>, ActiveVehicle>,
    /// The drivers of the active transit vehicles, until their legs end. Drivers have no person
    /// and thus no backpack. This field is there to avoid expensive calls to the schedule.
    known_transit_drivers: IntSet<Id<InternalPerson>>,
    transit_schedule: Arc<TransitSimSchedule>,
    network: Arc<Network>,
}

/// A transit vehicle that is active on this partition, with the line and route of its departure.
struct ActiveVehicle {
    line: Id<TransitLine>,
    route: Id<TransitRoute>,
    route_index: RouteIndex,
    /// The stop at which the vehicle arrived last on this partition. `None` before its first
    /// arrival here.
    stop: Option<Id<TransitStopFacility>>,
}

impl ActiveVehicle {
    /// The stop at which the vehicle halts. Passengers only board and alight at stops.
    fn stop(&self, vehicle_id: &Id<InternalVehicle>) -> &Id<TransitStopFacility> {
        self.stop.as_ref().unwrap_or_else(|| {
            panic!("Transit vehicle {vehicle_id} has not arrived at a stop on this partition yet.")
        })
    }
}

impl BackpackingDataCollector {
    pub fn new(
        home_person_ids: Vec<Id<InternalPerson>>,
        transit_schedule: Arc<TransitSimSchedule>,
        network: Arc<Network>,
    ) -> Self {
        Self {
            home_person_ids,
            person_id2backpack: Default::default(),
            vehicle_id2person_ids: Default::default(),
            transit_vehicles: Default::default(),
            known_transit_drivers: Default::default(),
            transit_schedule,
            network,
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
        self.known_transit_drivers.clear();
    }

    /// Records the person as an occupant of the vehicle and forwards the event to its backpack.
    /// A passenger who boards a transit vehicle starts a ride on it instead.
    #[hotpath::measure]
    pub(crate) fn person_enters_vehicle(&mut self, event: &PersonEntersVehicleEvent) {
        let Some(plan) = Self::partial_plan_of(
            &mut self.person_id2backpack,
            &self.known_transit_drivers,
            &self.transit_schedule,
            &event.person,
        ) else {
            return;
        };
        if let Some(vehicle) = self.transit_vehicles.get(&event.vehicle) {
            plan.transit_boarding(TransitBoarding {
                line: vehicle.line.clone(),
                route: vehicle.route.clone(),
                route_index: vehicle.route_index,
                access: vehicle.stop(&event.vehicle).clone(),
                time: event.time,
            });
            return;
        }
        self.vehicle_id2person_ids
            .entry(event.vehicle.clone())
            .or_default()
            .insert(event.person.clone());
        plan.person_enters_vehicle(event);
    }

    /// Removes the person from the occupants of the vehicle and forwards the event to its backpack.
    /// A passenger who leaves a transit vehicle ends its ride at the vehicle's stop. A driver who
    /// leaves its vehicle ends its departure.
    #[hotpath::measure]
    pub(crate) fn person_leaves_vehicle(&mut self, event: &PersonLeavesVehicleEvent) {
        let Some(plan) = Self::partial_plan_of(
            &mut self.person_id2backpack,
            &self.known_transit_drivers,
            &self.transit_schedule,
            &event.person,
        ) else {
            // A driver leaves its vehicle at the last stop.
            Self::remove_transit_vehicle(&mut self.transit_vehicles, &event.vehicle);
            return;
        };
        if let Some(vehicle) = self.transit_vehicles.get(&event.vehicle) {
            let egress = vehicle.stop(&event.vehicle);
            let schedule = self.transit_schedule.schedule();
            let stop_link = |stop: &Id<TransitStopFacility>| {
                schedule
                    .stop_link(stop)
                    .unwrap_or_else(|| panic!("Transit stop facility {stop} has no link."))
            };
            plan.transit_alighting(egress.clone(), |boarding| {
                calc_transit_distance(
                    &schedule.route(boarding.route_index).network_route,
                    stop_link(&boarding.access),
                    stop_link(egress),
                    &self.network,
                )
            });
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
        plan.person_leaves_vehicle(event);
    }

    pub(crate) fn person_departure(&mut self, event: &PersonDepartureEvent) {
        if let Some(plan) = self.partial_plan(&event.person) {
            plan.person_departure(event);
        }
    }

    pub(crate) fn person_arrival(&mut self, event: &PersonArrivalEvent) {
        match Self::partial_plan_of(
            &mut self.person_id2backpack,
            &self.known_transit_drivers,
            &self.transit_schedule,
            &event.person,
        ) {
            Some(plan) => plan.person_arrival(event, &self.network),
            // The leg of a driver ends after its departure.
            None => {
                self.known_transit_drivers.remove(&event.person);
            }
        }
    }

    pub(crate) fn activity_start(&mut self, event: &ActivityStartEvent) {
        if let Some(plan) = self.partial_plan(&event.person) {
            plan.activity_start(event);
        }
    }

    pub(crate) fn activity_end(&mut self, event: &ActivityEndEvent) {
        if let Some(plan) = self.partial_plan(&event.person) {
            plan.activity_end(event);
        }
    }

    pub(crate) fn teleportation_arrival(&mut self, event: &TeleportationArrivalEvent) {
        if let Some(plan) = self.partial_plan(&event.person) {
            plan.teleportation_arrival(event);
        }
    }

    pub(crate) fn pt_teleportation_arrival(&mut self, event: &PtTeleportationArrivalEvent) {
        if let Some(plan) = self.partial_plan(&event.person) {
            plan.pt_teleportation_arrival(event);
        }
    }

    pub(crate) fn person_stuck(&mut self, event: &PersonStuckEvent) {
        if let Some(plan) = self.partial_plan(&event.person) {
            plan.person_stuck(event);
        }
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

    pub(crate) fn transit_driver_starts(&mut self, event: &TransitDriverStartsEvent) {
        self.add_transit_vehicle(
            &event.vehicle,
            &event.driver,
            &event.transit_line,
            &event.transit_route,
        );
    }

    pub(crate) fn vehicle_arrives_at_facility(&mut self, event: &VehicleArrivesAtFacilityEvent) {
        let vehicle = self
            .transit_vehicles
            .get_mut(&event.vehicle)
            .unwrap_or_else(|| {
                panic!(
                    "Transit vehicle {} arrives at stop {}, but it is not active on this partition.",
                    event.vehicle, event.facility
                )
            });
        vehicle.stop = Some(event.facility.clone());
    }

    /// A transit vehicle becomes active on this partition: its departure starts here, or it arrives
    /// from another partition.
    fn add_transit_vehicle(
        &mut self,
        vehicle_id: &Id<InternalVehicle>,
        driver: &Id<InternalPerson>,
        line: &Id<TransitLine>,
        route: &Id<TransitRoute>,
    ) {
        let route_index = self
            .transit_schedule
            .schedule()
            .find_route(line, route)
            .unwrap_or_else(|| panic!("Transit route {route} of line {line} does not exist."));
        let previous = self.transit_vehicles.insert(
            vehicle_id.clone(),
            ActiveVehicle {
                line: line.clone(),
                route: route.clone(),
                route_index,
                stop: None,
            },
        );
        assert!(
            previous.is_none(),
            "Transit vehicle {vehicle_id} is already active on this partition, but each departure \
             has a vehicle of its own."
        );
        self.known_transit_drivers.insert(driver.clone());
    }

    /// The transit vehicle is no longer active on this partition: its departure ends, or it leaves
    /// for another partition.
    fn remove_transit_vehicle(
        transit_vehicles: &mut IntMap<Id<InternalVehicle>, ActiveVehicle>,
        vehicle_id: &Id<InternalVehicle>,
    ) {
        transit_vehicles.remove(vehicle_id).unwrap_or_else(|| {
            panic!("Transit vehicle {vehicle_id} is not active on this partition.")
        });
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

    fn partial_plan(&mut self, person: &Id<InternalPerson>) -> Option<&mut PartialPlan> {
        Self::partial_plan_of(
            &mut self.person_id2backpack,
            &self.known_transit_drivers,
            &self.transit_schedule,
            person,
        )
    }

    /// The partial plan of the person, or `None` for a transit driver, who has no person and thus
    /// no backpack. Drivers are only looked up if the backpack is missing, so the events of persons
    /// cost no extra lookup. The drivers of active vehicles are known; other drivers, e.g. those who
    /// still wait for their departure when the mobsim ends and get stuck, are rare, so they are
    /// searched in the schedule.
    fn partial_plan_of<'a>(
        backpacks: &'a mut IntMap<Id<InternalPerson>, Backpack>,
        transit_drivers: &IntSet<Id<InternalPerson>>,
        transit_schedule: &TransitSimSchedule,
        person: &Id<InternalPerson>,
    ) -> Option<&'a mut PartialPlan> {
        match backpacks.get_mut(person) {
            Some(backpack) => Some(backpack.partial_plan_mut()),
            None if transit_drivers.contains(person) || transit_schedule.is_driver(person) => None,
            None => panic!(
                "No backpack is available for person {} while handling an event.",
                person.external()
            ),
        }
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
        match entity {
            PartitionChangeEntity::Vehicle(vehicle) => {
                self.vehicle_id2person_ids.remove(vehicle.id());
            }
            PartitionChangeEntity::TransitVehicle(vehicle) => {
                Self::remove_transit_vehicle(&mut self.transit_vehicles, vehicle.id());
                self.known_transit_drivers.remove(vehicle.driver().id());
            }
            PartitionChangeEntity::TeleportationAgent(_) => {}
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

        match entity {
            PartitionChangeEntity::Vehicle(vehicle) => {
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
            // The vehicle's stop is known once it arrives at its next stop on this partition.
            PartitionChangeEntity::TransitVehicle(vehicle) => {
                let driver = vehicle.driver();
                self.add_transit_vehicle(vehicle.id(), driver.id(), driver.line(), driver.route());
            }
            PartitionChangeEntity::TeleportationAgent(_) => {}
        }
    }

    fn person_ids(entity: PartitionChangeEntity<'_>) -> Vec<Id<InternalPerson>> {
        match entity {
            PartitionChangeEntity::Vehicle(vehicle) => std::iter::once(vehicle.driver().id())
                .chain(vehicle.passengers().iter().map(|agent| agent.id()))
                .cloned()
                .collect(),
            PartitionChangeEntity::TeleportationAgent(agent) => vec![agent.id().clone()],
            // Transit drivers have no person and thus no backpack.
            PartitionChangeEntity::TransitVehicle(vehicle) => vehicle
                .passengers()
                .iter()
                .map(|agent| agent.id().clone())
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
    use crate::simulation::agents::TransitDriverLogic;
    use crate::simulation::config::TransitSimulation;
    use crate::simulation::events::{
        ActivityEndEventBuilder, ActivityStartEventBuilder, PersonArrivalEventBuilder,
        PersonDepartureEventBuilder, PersonEntersVehicleEventBuilder,
        PersonLeavesVehicleEventBuilder, PersonStuckEventBuilder, TransitDriverStartsEventBuilder,
        VehicleArrivesAtFacilityEventBuilder,
    };
    use crate::simulation::pt::driver::PlannedDriver;
    use crate::simulation::pt::schedule::give_each_departure_its_own_vehicle;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::population::{InternalPlanElement, InternalRoute, Population};
    use crate::simulation::scenario::transit::{DepartureIndex, TransitSchedule};
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
    use crate::simulation::time::SimTime;
    use crate::simulation::vehicles::{SimulationVehicle, TransitVehicle};
    use crate::test_utils::{create_agent, create_pt_agent};
    use macros::deterministic_id_test;
    use std::path::{Path, PathBuf};

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
            Arc::default(),
            Arc::default(),
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
            BackpackingDataCollector::new(Vec::new(), Arc::default(), Arc::default());
        arriving.receive(PartitionChangeEntity::Vehicle(&vehicle), attachment);
        assert_eq!(arriving.person_id2backpack.len(), 2);
        let expected: IntSet<_> = [driver_id, passenger_id].into_iter().collect();
        assert_eq!(
            arriving.vehicle_id2person_ids.get(vehicle.id()).unwrap(),
            &expected
        );
    }

    /// The tutorial schedule on one partition with its network, and departure `01` of route
    /// `1to3`, which leaves stop 1 at 06:00, halts at stop 2a and reaches stop 3 at 06:09.
    fn tutorial() -> (Arc<TransitSimSchedule>, Arc<Network>, DepartureIndex) {
        let network = Network::from_file("./assets/pt_tutorial/multimodalnetwork.xml");
        let mut schedule =
            TransitSchedule::from_file(Path::new("./assets/pt_tutorial/transitschedule.xml"));
        let mut garage =
            Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        give_each_departure_its_own_vehicle(&mut schedule, &mut garage);
        let transit = TransitSimSchedule::build(
            Arc::new(schedule),
            &network,
            &garage,
            &Population::default(),
            TransitSimulation::ScheduleBasedTeleportation,
        );
        let departure = transit
            .schedule()
            .indexed_departures()
            .map(|(index, _)| index)
            .find(|&index| transit.driver(index).external() == "pt_Blue Line_1to3_01")
            .unwrap();
        (Arc::new(transit), Arc::new(network), departure)
    }

    fn time(hours: u32, minutes: u32) -> SimTime {
        SimTime::from_secs(u64::from(hours * 3600 + minutes * 60))
    }

    /// The passenger ends its activity on link 11, departs with `pt` at 05:59 and boards the
    /// vehicle of `departure` at stop 1, after its driver started.
    fn start_ride(
        collector: &mut BackpackingDataCollector,
        transit: &TransitSimSchedule,
        departure: DepartureIndex,
        passenger: &Id<InternalPerson>,
    ) {
        let driver = transit.driver(departure);
        let vehicle = transit.vehicle(departure);
        collector.activity_end(
            &ActivityEndEventBuilder::default()
                .time(time(5, 59))
                .person(passenger.clone())
                .link(Id::get_from_ext("11"))
                .act_type(Id::create("pt interaction"))
                .coordinate(Coordinate::default())
                .build()
                .unwrap(),
        );
        collector.person_departure(
            &PersonDepartureEventBuilder::default()
                .time(time(5, 59))
                .person(passenger.clone())
                .link(Id::get_from_ext("11"))
                .leg_mode(Id::create("pt"))
                .routing_mode(Some(Id::create("pt")))
                .build()
                .unwrap(),
        );
        collector.transit_driver_starts(
            &TransitDriverStartsEventBuilder::default()
                .time(time(6, 0))
                .driver(driver.clone())
                .vehicle(vehicle.clone())
                .transit_line(Id::get_from_ext("Blue Line"))
                .transit_route(Id::get_from_ext("1to3"))
                .departure(Id::get_from_ext("01"))
                .build()
                .unwrap(),
        );
        // The events of the driver's own leg are ignored.
        collector.person_departure(
            &PersonDepartureEventBuilder::default()
                .time(time(6, 0))
                .person(driver.clone())
                .link(Id::get_from_ext("11"))
                .leg_mode(Id::create("train"))
                .build()
                .unwrap(),
        );
        collector.person_enters_vehicle(
            &PersonEntersVehicleEventBuilder::default()
                .time(time(6, 0))
                .person(driver.clone())
                .vehicle(vehicle.clone())
                .build()
                .unwrap(),
        );
        arrive(collector, vehicle, "1", time(6, 0));
        collector.person_enters_vehicle(
            &PersonEntersVehicleEventBuilder::default()
                .time(time(6, 0))
                .person(passenger.clone())
                .vehicle(vehicle.clone())
                .build()
                .unwrap(),
        );
    }

    fn arrive(
        collector: &mut BackpackingDataCollector,
        vehicle: &Id<InternalVehicle>,
        stop: &str,
        at: SimTime,
    ) {
        collector.vehicle_arrives_at_facility(
            &VehicleArrivesAtFacilityEventBuilder::default()
                .time(at)
                .vehicle(vehicle.clone())
                .facility(Id::get_from_ext(stop))
                .build()
                .unwrap(),
        );
    }

    /// The passenger and the driver leave the vehicle at stop 3 at 06:09, and the passenger
    /// arrives on link 33 and starts an activity there.
    fn end_ride(
        collector: &mut BackpackingDataCollector,
        transit: &TransitSimSchedule,
        departure: DepartureIndex,
        passenger: &Id<InternalPerson>,
    ) {
        let vehicle = transit.vehicle(departure);
        arrive(collector, vehicle, "3", time(6, 9));
        for person in [passenger, transit.driver(departure)] {
            collector.person_leaves_vehicle(
                &PersonLeavesVehicleEventBuilder::default()
                    .time(time(6, 9))
                    .person(person.clone())
                    .vehicle(vehicle.clone())
                    .build()
                    .unwrap(),
            );
            collector.person_arrival(
                &PersonArrivalEventBuilder::default()
                    .time(time(6, 9))
                    .person(person.clone())
                    .link(Id::get_from_ext("33"))
                    .leg_mode(Id::create("pt"))
                    .build()
                    .unwrap(),
            );
        }
        collector.activity_start(
            &ActivityStartEventBuilder::default()
                .time(time(6, 9))
                .person(passenger.clone())
                .link(Id::get_from_ext("33"))
                .act_type(Id::create("pt interaction"))
                .coordinate(Coordinate::default())
                .build()
                .unwrap(),
        );
    }

    /// Checks the PT route of the passenger's leg: route 1to3 from stop 1, boarded at 06:00, to
    /// stop 3, with the distance of MATSim's reference run of the tutorial.
    fn assert_ride_from_1_to_3(experiences: &IntMap<Id<InternalPerson>, PersonExperience>) {
        assert_eq!(1, experiences.len());
        let plan = experiences.values().next().unwrap().plan();
        let InternalPlanElement::Leg(leg) = &plan.elements[1] else {
            panic!("Expected the transit leg");
        };
        assert_eq!(leg.trav_time, Some(std::time::Duration::from_secs(600)));
        let Some(InternalRoute::Pt(route)) = &leg.route else {
            panic!("Expected a PT route, got {:?}", leg.route);
        };
        assert_eq!("11", route.start_link().external());
        assert_eq!("33", route.end_link().external());
        assert_eq!(Some(6100.0), route.generic_delegate().distance());
        let description = &route.description;
        assert_eq!("Blue Line", description.transit_line_id.external());
        assert_eq!("1to3", description.transit_route_id.external());
        assert_eq!("1", description.access_facility_id.external());
        assert_eq!("3", description.egress_facility_id.external());
        assert_eq!(Some(time(6, 0)), description.boarding_time);
    }

    #[deterministic_id_test]
    fn reconstructs_transit_ride_and_ignores_drivers() {
        let (transit, network, departure) = tutorial();
        let passenger = Id::<InternalPerson>::create("passenger");
        let mut collector =
            BackpackingDataCollector::new(vec![passenger.clone()], transit.clone(), network);
        collector.reset_iteration();

        start_ride(&mut collector, &transit, departure, &passenger);
        arrive(&mut collector, transit.vehicle(departure), "2a", time(6, 3));
        end_ride(&mut collector, &transit, departure, &passenger);
        // A driver still waiting for its departure at the end of the mobsim gets stuck.
        collector.person_stuck(
            &PersonStuckEventBuilder::default()
                .time(time(30, 0))
                .person(Id::get_from_ext("pt_Blue Line_3to1_50"))
                .build()
                .unwrap(),
        );

        assert!(collector.transit_vehicles.is_empty());
        assert!(collector.known_transit_drivers.is_empty());
        assert_ride_from_1_to_3(&collector.finish());
    }

    /// The vehicle leaves for another partition after stop 1, with the passenger's backpack. The
    /// other partition knows the departure from the driver and ends the ride at stop 3.
    #[deterministic_id_test]
    fn transit_ride_continues_on_another_partition() {
        let (transit, network, departure) = tutorial();
        let passenger_agent = create_pt_agent(1, "Blue Line", "1", "3", "11", "33");
        let passenger = passenger_agent.id().clone();
        let mut departing = BackpackingDataCollector::new(
            vec![passenger.clone()],
            transit.clone(),
            network.clone(),
        );
        departing.reset_iteration();
        start_ride(&mut departing, &transit, departure, &passenger);

        let mut driver = Box::new(PlannedDriver::new(&transit, departure, time(6, 0)));
        driver.notify_stop_arrived(time(6, 0));
        driver.notify_stop_departed(time(6, 1), time(6, 0));
        let mut vehicle = TransitVehicle::new(transit.vehicle(departure).clone(), driver);
        vehicle.add_passenger(passenger_agent);
        let attachment = departing.send(PartitionChangeEntity::TransitVehicle(&vehicle));
        assert!(departing.transit_vehicles.is_empty());
        assert!(departing.known_transit_drivers.is_empty());
        assert!(departing.person_id2backpack.is_empty());

        let mut arriving = BackpackingDataCollector::new(Vec::new(), transit.clone(), network);
        arriving.receive(PartitionChangeEntity::TransitVehicle(&vehicle), attachment);
        arrive(&mut arriving, transit.vehicle(departure), "2a", time(6, 3));
        end_ride(&mut arriving, &transit, departure, &passenger);

        assert!(arriving.transit_vehicles.is_empty());
        assert!(arriving.known_transit_drivers.is_empty());
        assert_ride_from_1_to_3(&arriving.finish());
    }
}
