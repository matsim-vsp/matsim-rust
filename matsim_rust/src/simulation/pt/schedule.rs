use crate::simulation::id::Id;
use crate::simulation::scenario::network::Network;
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scenario::transit::{
    DepartureIndex, TransitDeparture, TransitRoute, TransitSchedule, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use itertools::Itertools;
use nohash_hasher::IntMap;
use std::sync::Arc;
use tracing::info;

/// MATSim's `PtConstants.TRANSIT_ACTIVITY_TYPE`, which MATSim's transit drivers use as well.
const DRIVER_ACTIVITY_TYPE: &str = "pt interaction";

/// The [`TransitSchedule`] with what the mobsim adds to it: the driver and the vehicle of each
/// departure and the partition of each stop. It is built once, before the first iteration, and
/// shared by all partitions, like [`SimNetworkPartition`](crate::simulation::network::sim_network::SimNetworkPartition)
/// extends [`Network`].
///
/// The driver of a departure has the external id `pt_<line>_<route>_<departure>`, like the
/// drivers of SBB's transit engine. Driver ids are created in the order of these external ids, so
/// their internal ids are a stable order for simultaneous events, which depends neither on map
/// iteration nor on the input format.
///
/// Offsets are kept as given in the schedule. Like SBB's transit engine, transit drivers clamp
/// the resulting times: a vehicle never arrives before it left the previous stop and never
/// departs before its scheduled departure.
#[derive(Debug, Default)]
pub struct TransitSimSchedule {
    schedule: Arc<TransitSchedule>,
    /// The driver of each departure, by [`DepartureIndex`].
    drivers: Vec<Id<InternalPerson>>,
    /// The vehicle of each departure, by [`DepartureIndex`].
    vehicles: Vec<Id<InternalVehicle>>,
    /// The partition of each stop facility. Facilities that no route serves have no entry.
    stop_partitions: IntMap<Id<TransitStopFacility>, u32>,
    /// See [`Self::driver_activity_type`]. `None` for the empty schedule.
    driver_activity_type: Option<Id<String>>,
}

impl TransitSimSchedule {
    /// Builds the mobsim's view of `schedule` and creates the driver ids.
    ///
    /// This must run after the population is loaded, so that the internal ids of persons do not
    /// depend on whether transit is simulated.
    ///
    /// # Panics
    ///
    /// If the schedule cannot be simulated: a route has no stops, a stop facility has no link or
    /// an unknown link, a stop has no offset, a departure has no vehicle or a vehicle which is not
    /// in `garage`, a vehicle serves departures whose time windows overlap, or a driver id clashes
    /// with another driver or with a person.
    pub fn build(
        schedule: Arc<TransitSchedule>,
        network: &Network,
        garage: &Garage,
        population: &Population,
    ) -> Self {
        let mut pending_drivers: Vec<_> = schedule
            .indexed_departures()
            .map(|(index, departure)| {
                let route = schedule.route(schedule.departure_route(index));
                let driver = format!(
                    "pt_{}_{}_{}",
                    route.line.external(),
                    route.id.external(),
                    departure.id.external()
                );
                (driver, index)
            })
            .collect();
        pending_drivers.sort_by(|(driver_a, _), (driver_b, _)| driver_a.cmp(driver_b));
        // Ids like line "a_b" with route "c" and line "a" with route "b_c" give the same driver.
        if let Some(pair) = pending_drivers
            .windows(2)
            .find(|pair| pair[0].0 == pair[1].0)
        {
            panic!(
                "Two transit departures have the same driver id {}.",
                pair[0].0
            );
        }

        for route in schedule.routes() {
            check_route(&schedule, route, network);
        }
        let stop_partitions = stop_partitions(&schedule, network);

        let mut drivers = vec![None; schedule.departures().len()];
        let mut vehicles = vec![None; schedule.departures().len()];
        for (driver, index) in pending_drivers {
            let vehicle = resolve_vehicle(&driver, schedule.departure(index), garage);
            // Created in sorted order, so that the internal ids of drivers are stable.
            let driver_id = Id::<InternalPerson>::create(&driver);
            assert!(
                !population.persons.contains_key(&driver_id),
                "The id {driver} of a transit driver is also the id of a person."
            );
            drivers[index.index()] = Some(driver_id);
            vehicles[index.index()] = Some(vehicle);
        }

        let result = Self {
            schedule,
            drivers: drivers.into_iter().map(Option::unwrap).collect(),
            vehicles: vehicles.into_iter().map(Option::unwrap).collect(),
            stop_partitions,
            // Created here, so that the id does not depend on which partition creates its drivers
            // first.
            driver_activity_type: Some(Id::create(DRIVER_ACTIVITY_TYPE)),
        };
        result.assert_vehicles_are_not_shared_at_the_same_time();
        info!(
            "Built transit schedule for simulation with {} routes and {} departures.",
            result.schedule.routes().len(),
            result.drivers.len()
        );
        result
    }

    pub fn schedule(&self) -> &TransitSchedule {
        &self.schedule
    }

    /// The driver of the departure, who has no person.
    pub fn driver(&self, departure: DepartureIndex) -> &Id<InternalPerson> {
        &self.drivers[departure.index()]
    }

    pub fn vehicle(&self, departure: DepartureIndex) -> &Id<InternalVehicle> {
        &self.vehicles[departure.index()]
    }

    /// The departures whose first stop is on `partition`. Their drivers start there.
    pub fn departures_starting_on(
        &self,
        partition: u32,
    ) -> impl Iterator<Item = DepartureIndex> + '_ {
        self.schedule
            .indexed_departures()
            .map(|(index, _)| index)
            .filter(move |&index| {
                let route = self.schedule.route(self.schedule.departure_route(index));
                self.stop_partition(&route.stop_facilities[0]) == partition
            })
    }

    /// The type of the activities in which drivers wait for their departure and stay after it.
    pub fn driver_activity_type(&self) -> &Id<String> {
        self.driver_activity_type
            .as_ref()
            .expect("The empty transit schedule has no drivers.")
    }

    pub fn is_empty(&self) -> bool {
        self.schedule.routes().is_empty()
    }

    /// The stop facilities that a route serves, in no particular order.
    pub fn served_stops(&self) -> impl Iterator<Item = &Id<TransitStopFacility>> {
        self.stop_partitions.keys()
    }

    /// The partition that handles vehicles at `stop`.
    pub fn stop_partition(&self, stop: &Id<TransitStopFacility>) -> u32 {
        self.stop_partitions
            .get(stop)
            .copied()
            .unwrap_or_else(|| panic!("No transit route serves stop {stop}."))
    }

    /// Scheduled time at which the departure leaves its last stop and releases its vehicle. Like
    /// the drivers, a vehicle never departs before its arrival.
    fn scheduled_release(&self, index: DepartureIndex) -> SimTime {
        let route = self.schedule.route(self.schedule.departure_route(index));
        let last_stop = route.stops.last().expect("Routes have at least one stop.");
        let offset = last_stop
            .arrival()
            .max(last_stop.departure())
            .expect("Every stop has an offset.");
        self.schedule.departure(index).departure_time + SimTime::from_duration(offset)
    }

    /// A vehicle can only serve one departure at a time. Departures may share a vehicle if one
    /// releases it at or before the start of the next one.
    fn assert_vehicles_are_not_shared_at_the_same_time(&self) {
        let windows: Vec<_> = self
            .schedule
            .indexed_departures()
            .map(|(index, departure)| {
                (
                    departure.departure_time,
                    self.scheduled_release(index),
                    index,
                )
            })
            .sorted_by_key(|(start, end, index)| {
                (
                    self.vehicle(*index).internal(),
                    *start,
                    *end,
                    self.driver(*index).internal(),
                )
            })
            .collect();

        // The check stops at the first overlap, so the windows before it are disjoint and
        // comparing neighbours is enough.
        for pair in windows.windows(2) {
            let (_, previous_end, previous) = pair[0];
            let (start, _, index) = pair[1];
            assert!(
                self.vehicle(previous) != self.vehicle(index) || start >= previous_end,
                "Transit vehicle {} serves the departures of drivers {} and {} at the same time.",
                self.vehicle(index),
                self.driver(previous),
                self.driver(index)
            );
        }
    }
}

/// Checks that the route can be simulated: it has stops, each stop has an offset, and each stop
/// facility has a link of the network.
fn check_route(schedule: &TransitSchedule, route: &TransitRoute, network: &Network) {
    assert!(
        !route.stops.is_empty(),
        "Transit route {} of line {} has no stops.",
        route.id,
        route.line
    );
    for (index, (facility, stop)) in route.stop_facilities.iter().zip(&route.stops).enumerate() {
        assert!(
            stop.arrival().is_some(),
            "Stop {index} ({facility}) of transit route {} of line {} has neither an arrival nor a \
             departure offset.",
            route.id,
            route.line
        );
        check_stop_link(schedule, facility, network);
    }
}

fn check_stop_link(
    schedule: &TransitSchedule,
    facility_id: &Id<TransitStopFacility>,
    network: &Network,
) {
    let facility = schedule
        .facility(facility_id)
        .unwrap_or_else(|| panic!("Transit stop facility {facility_id} does not exist."));
    let link = facility
        .link_ref_id
        .as_ref()
        .unwrap_or_else(|| panic!("Transit stop facility {facility_id} has no link."));
    assert!(
        network.links_with_ids().contains_key(link),
        "Transit stop facility {facility_id} references link {link}, which is not part of the \
         network."
    );
}

/// The partition of each stop facility that a route serves, see [`TransitSimSchedule::stop_partition`].
/// A facility has one link, so all routes agree on its partition.
fn stop_partitions(
    schedule: &TransitSchedule,
    network: &Network,
) -> IntMap<Id<TransitStopFacility>, u32> {
    let mut partitions = IntMap::default();
    for route in schedule.routes() {
        for facility in &route.stop_facilities {
            let link = schedule
                .stop_link(facility)
                .expect("Every stop facility has a link.");
            partitions.insert(facility.clone(), network.get_link(link).partition);
        }
    }
    partitions
}

fn resolve_vehicle(
    driver: &str,
    departure: &TransitDeparture,
    garage: &Garage,
) -> Id<InternalVehicle> {
    let vehicle_ref = departure
        .vehicle_ref_id
        .as_ref()
        .unwrap_or_else(|| panic!("The transit departure of driver {driver} has no vehicle."));
    Id::<InternalVehicle>::try_get_from_ext(vehicle_ref.external())
        .filter(|vehicle| garage.vehicles.contains_key(vehicle))
        .unwrap_or_else(|| {
            panic!(
                "Vehicle {vehicle_ref} of the transit departure of driver {driver} is not in the \
                 transit vehicles. Check transit.vehicles_path."
            )
        })
}

#[cfg(test)]
mod tests {
    use super::TransitSimSchedule;
    use crate::simulation::config::{Config, ScoringMode, Transit, TransitSimulation};
    use crate::simulation::id::Id;
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
    use crate::simulation::scenario::transit::{TransitSchedule, TransitStopFacility};
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
    use crate::simulation::scenario::{ControllerScenario, Scenario};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    const NETWORK: &str = "./assets/pt_tutorial/multimodalnetwork.xml";
    const SCHEDULE: &str = "./assets/pt_tutorial/transitschedule.xml";
    const VEHICLES: &str = "./assets/pt_tutorial/transitVehicles.xml";

    fn tutorial_schedule() -> TransitSimSchedule {
        let network = Network::from_file(NETWORK);
        let schedule = TransitSchedule::from_file(Path::new(SCHEDULE));
        let garage = Garage::from_file(&PathBuf::from(VEHICLES));
        TransitSimSchedule::build(
            Arc::new(schedule),
            &network,
            &garage,
            &Population::default(),
        )
    }

    #[deterministic_id_test]
    fn transit_garage_reads_vehicle_definitions_v1() {
        let garage = Garage::from_file(&PathBuf::from(VEHICLES));

        assert_eq!(1, garage.vehicle_types.len());
        assert_eq!(2, garage.vehicles.len());
        assert!(
            garage
                .vehicles
                .contains_key(&Id::<InternalVehicle>::get_from_ext("tr_1"))
        );
    }

    #[deterministic_id_test]
    fn tutorial_routes_are_sorted_and_offsets_fall_back() {
        let schedule = tutorial_schedule();

        let transit = schedule.schedule();
        let routes = transit.routes();
        assert_eq!(2, routes.len());
        assert_eq!("1to3", routes[0].id.external());
        assert_eq!("3to1", routes[1].id.external());
        assert_eq!("Blue Line", routes[0].line.external());
        assert_eq!("train", routes[0].transport_mode.external());
        assert_eq!("pt", routes[1].transport_mode.external());
        assert_eq!(
            Some(&Id::<Link>::get_from_ext("11")),
            transit.start_link(&routes[0])
        );
        assert_eq!(
            Some(&Id::<Link>::get_from_ext("33")),
            transit.end_link(&routes[0])
        );

        let stops = &routes[0].stops;
        assert_eq!(3, stops.len());
        let stop_2a = Id::<TransitStopFacility>::get_from_ext("2a");
        assert_eq!(stop_2a, routes[0].stop_facilities[1]);
        assert_eq!(
            Some(&Id::<Link>::get_from_ext("12")),
            transit.stop_link(&stop_2a)
        );
        assert_eq!(0, schedule.stop_partition(&stop_2a));
        // The first stop only has a departure offset, the last one only an arrival offset.
        assert_eq!(Some(Duration::ZERO), stops[0].arrival());
        assert_eq!(Some(Duration::ZERO), stops[0].departure());
        assert_eq!(Some(Duration::from_secs(200)), stops[1].arrival());
        assert_eq!(Some(Duration::from_secs(240)), stops[1].departure());
        assert_eq!(Some(Duration::from_secs(540)), stops[2].arrival());
        assert_eq!(Some(Duration::from_secs(540)), stops[2].departure());
    }

    #[deterministic_id_test]
    fn tutorial_drivers_get_ascending_ids_in_the_order_of_their_names() {
        let schedule = tutorial_schedule();
        let transit = schedule.schedule();

        let mut departures: Vec<_> = transit
            .indexed_departures()
            .map(|(index, _)| index)
            .collect();
        assert_eq!(100, departures.len());
        departures.sort_by_key(|&index| schedule.driver(index).external().to_owned());
        let first = departures[0];
        assert_eq!("pt_Blue Line_1to3_01", schedule.driver(first).external());
        let route = transit.route(transit.departure_route(first));
        assert_eq!("1to3", route.id.external());
        assert_eq!("01", transit.departure(first).id.external());
        assert_eq!(
            SimTime::from_secs(6 * 3600),
            transit.departure(first).departure_time
        );
        assert_eq!("tr_1", schedule.vehicle(first).external());
        let last = *departures.last().unwrap();
        assert_eq!("pt_Blue Line_3to1_50", schedule.driver(last).external());
        let route = transit.route(transit.departure_route(last));
        assert_eq!("3to1", route.id.external());

        assert!(
            departures
                .windows(2)
                .all(|pair| schedule.driver(pair[0]).internal()
                    < schedule.driver(pair[1]).internal())
        );
    }

    #[deterministic_id_test]
    fn teleported_transit_builds_no_schedule_and_no_driver_ids() {
        let mut config = Config::default();
        config.network_mut().path = Some(NETWORK.into());
        config.set_transit(Transit {
            schedule_path: Some(SCHEDULE.into()),
            vehicles_path: Some(VEHICLES.into()),
            ..Transit::default()
        });

        let scenario = Scenario::load(config);
        assert_eq!(2, scenario.transit_garage.vehicles.len());
        let scenario: ControllerScenario = scenario.into();

        assert!(scenario.transit_sim_schedule.is_empty());
        assert!(Id::<InternalPerson>::try_get_from_ext("pt_Blue Line_1to3_01").is_none());
    }

    fn schedule_based_config(scoring: ScoringMode) -> Config {
        let mut config = Config::default();
        config.network_mut().path = Some(NETWORK.into());
        config.set_transit(Transit {
            schedule_path: Some(SCHEDULE.into()),
            vehicles_path: Some(VEHICLES.into()),
            simulation: TransitSimulation::ScheduleBased,
            ..Transit::default()
        });
        config.scoring_mut().mode = scoring;
        config
    }

    #[deterministic_id_test]
    fn schedule_based_transit_builds_schedule_in_controller_scenario() {
        let config = schedule_based_config(ScoringMode::Disabled);

        let scenario: ControllerScenario = Scenario::load(config).into();

        let transit = scenario.transit_sim_schedule.schedule();
        assert_eq!(2, transit.routes().len());
        assert_eq!(100, transit.departures().len());
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Schedule-based transit does not support scoring yet.")]
    fn schedule_based_transit_with_scoring_panics() {
        let config = schedule_based_config(ScoringMode::Enabled);

        let _: ControllerScenario = Scenario::load(config).into();
    }

    /// Builds a schedule with one line `L` and one route `R` on the tutorial network. Stop `s1`
    /// is on link 11 and `s2` on link 12.
    fn build_small(facilities: &str, stops: &str, departures: &str, population: &Population) {
        let xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE transitSchedule SYSTEM "http://www.matsim.org/files/dtd/transitSchedule_v1.dtd">
<transitSchedule>
    <transitStops>{facilities}</transitStops>
    <transitLine id="L">
        <transitRoute id="R">
            <transportMode>train</transportMode>
            <routeProfile>{stops}</routeProfile>
            <route><link refId="11"/><link refId="12"/></route>
            <departures>{departures}</departures>
        </transitRoute>
    </transitLine>
</transitSchedule>"#
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("schedule.xml");
        std::fs::write(&path, xml).unwrap();

        let network = Network::from_file(NETWORK);
        let schedule = TransitSchedule::from_file(&path);
        let garage = Garage::from_file(&PathBuf::from(VEHICLES));
        TransitSimSchedule::build(Arc::new(schedule), &network, &garage, population);
    }

    const FACILITIES: &str = r#"
        <stopFacility id="s1" x="0" y="0" linkRefId="11"/>
        <stopFacility id="s2" x="0" y="0" linkRefId="12"/>"#;
    const STOPS: &str = r#"
        <stop refId="s1" departureOffset="00:00:00"/>
        <stop refId="s2" arrivalOffset="00:10:00"/>"#;

    #[deterministic_id_test]
    fn vehicle_may_serve_departures_which_touch() {
        build_small(
            FACILITIES,
            STOPS,
            r#"<departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/>
               <departure id="2" departureTime="06:10:00" vehicleRefId="tr_1"/>"#,
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "serves the departures of drivers pt_L_R_1 and pt_L_R_2")]
    fn vehicle_serving_overlapping_departures_panics() {
        build_small(
            FACILITIES,
            STOPS,
            r#"<departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/>
               <departure id="2" departureTime="06:09:59" vehicleRefId="tr_1"/>"#,
            &Population::default(),
        );
    }

    /// The vehicle halts at the last stop of the first departure until 06:11, so it cannot start
    /// the second one at 06:10:30.
    #[deterministic_id_test]
    #[should_panic(expected = "serves the departures of drivers pt_L_R_1 and pt_L_R_2")]
    fn vehicle_halting_at_its_last_stop_cannot_start_another_departure() {
        build_small(
            FACILITIES,
            r#"<stop refId="s1" departureOffset="00:00:00"/>
               <stop refId="s2" arrivalOffset="00:10:00" departureOffset="00:11:00"/>"#,
            r#"<departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/>
               <departure id="2" departureTime="06:10:30" vehicleRefId="tr_1"/>"#,
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Transit stop facility s1 has no link.")]
    fn stop_facility_without_link_panics() {
        build_small(
            r#"<stopFacility id="s1" x="0" y="0"/>
               <stopFacility id="s2" x="0" y="0" linkRefId="12"/>"#,
            STOPS,
            "",
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "references link unknown, which is not part of the network")]
    fn stop_facility_with_unknown_link_panics() {
        build_small(
            r#"<stopFacility id="s1" x="0" y="0" linkRefId="unknown"/>
               <stopFacility id="s2" x="0" y="0" linkRefId="12"/>"#,
            STOPS,
            "",
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "has neither an arrival nor a departure offset")]
    fn stop_without_offsets_panics() {
        build_small(
            FACILITIES,
            r#"<stop refId="s1" departureOffset="00:00:00"/><stop refId="s2"/>"#,
            "",
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "The transit departure of driver pt_L_R_1 has no vehicle.")]
    fn departure_without_vehicle_panics() {
        build_small(
            FACILITIES,
            STOPS,
            r#"<departure id="1" departureTime="06:00:00"/>"#,
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "Vehicle tr_9 of the transit departure of driver pt_L_R_1")]
    fn departure_with_unknown_vehicle_panics() {
        build_small(
            FACILITIES,
            STOPS,
            r#"<departure id="1" departureTime="06:00:00" vehicleRefId="tr_9"/>"#,
            &Population::default(),
        );
    }

    #[deterministic_id_test]
    #[should_panic(expected = "The id pt_L_R_1 of a transit driver is also the id of a person.")]
    fn driver_id_of_existing_person_panics() {
        let population = Population::from_persons(vec![InternalPerson::new(
            Id::create("pt_L_R_1"),
            InternalPlan::default(),
        )]);
        build_small(
            FACILITIES,
            STOPS,
            r#"<departure id="1" departureTime="06:00:00" vehicleRefId="tr_1"/>"#,
            &population,
        );
    }
}
