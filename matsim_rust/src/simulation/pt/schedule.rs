use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalPerson, Population};
use crate::simulation::scenario::transit::{
    TransitDeparture, TransitLine, TransitRoute, TransitSchedule, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use itertools::Itertools;
use std::time::Duration;
use tracing::info;

/// Dense, simulation-ready view of the transit schedule. It is built once, before the first
/// iteration, and shared by all partitions. The maps of [`TransitSchedule`] are only read while
/// building it.
///
/// Routes are sorted by the external ids of line and route, departures by the external id of
/// their driver. A departure's index is therefore a stable order for simultaneous events, which
/// depends neither on map iteration nor on the input format.
///
/// Offsets are stored as given in the schedule. Like SBB's transit engine, the transit engine
/// clamps the resulting times: a vehicle never arrives before it left the previous stop and never
/// departs before its scheduled departure.
#[derive(Debug, Default)]
pub struct TransitSimSchedule {
    routes: Vec<SimTransitRoute>,
    departures: Vec<SimDeparture>,
}

#[derive(Debug, PartialEq)]
pub struct SimTransitRoute {
    pub line: Id<TransitLine>,
    pub route: Id<TransitRoute>,
    pub mode: Id<String>,
    /// First link of the network route, or the link of the first stop if the route has no links.
    pub start_link: Id<Link>,
    /// Last link of the network route, or the link of the last stop if the route has no links.
    pub end_link: Id<Link>,
    pub stops: Vec<SimRouteStop>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimRouteStop {
    pub facility: Id<TransitStopFacility>,
    pub link: Id<Link>,
    /// Partition of `link`, i.e. the partition which handles the vehicle at this stop.
    pub partition: u32,
    /// Arrival offset relative to the departure time. Falls back to the departure offset.
    pub arrival_offset: Duration,
    /// Departure offset relative to the departure time. Falls back to the arrival offset.
    pub departure_offset: Duration,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SimDeparture {
    /// Index into [`TransitSimSchedule::routes`].
    pub route: usize,
    pub id: Id<TransitDeparture>,
    pub time: SimTime,
    pub vehicle: Id<InternalVehicle>,
    /// The driver only exists as an id for the events. Its external id is
    /// `pt_<line>_<route>_<departure>`, like the drivers of SBB's transit engine.
    pub driver: Id<InternalPerson>,
}

impl TransitSimSchedule {
    /// Builds the dense schedule and creates the driver ids.
    ///
    /// This must run after the population is loaded, so that the internal ids of persons do not
    /// depend on whether transit is simulated.
    ///
    /// # Panics
    ///
    /// If the schedule cannot be simulated: a stop facility has no link or an unknown link, a
    /// stop has no offset, a departure has no vehicle or a vehicle which is not in `garage`, a
    /// vehicle serves departures whose time windows overlap, or a driver id clashes with another
    /// driver or with a person.
    pub fn build(
        schedule: &TransitSchedule,
        network: &Network,
        garage: &Garage,
        population: &Population,
    ) -> Self {
        let routes: Vec<_> = schedule
            .lines()
            .values()
            .flat_map(|line| line.routes.values().map(move |route| (line, route)))
            .sorted_by(|(line_a, route_a), (line_b, route_b)| {
                (line_a.id.external(), route_a.id.external())
                    .cmp(&(line_b.id.external(), route_b.id.external()))
            })
            .collect();

        let mut pending_departures: Vec<_> = routes
            .iter()
            .enumerate()
            .flat_map(|(route_index, (line, route))| {
                route.departures.iter().map(move |departure| {
                    let driver = format!(
                        "pt_{}_{}_{}",
                        line.id.external(),
                        route.id.external(),
                        departure.id.external()
                    );
                    (driver, route_index, departure)
                })
            })
            .collect();
        pending_departures.sort_by(|(driver_a, ..), (driver_b, ..)| driver_a.cmp(driver_b));
        // Ids like line "a_b" with route "c" and line "a" with route "b_c" give the same driver.
        if let Some(pair) = pending_departures
            .windows(2)
            .find(|pair| pair[0].0 == pair[1].0)
        {
            panic!(
                "Two transit departures have the same driver id {}.",
                pair[0].0
            );
        }

        let sim_routes: Vec<_> = routes
            .iter()
            .map(|(line, route)| SimTransitRoute::new(line, route, schedule, network))
            .collect();

        let departures: Vec<_> = pending_departures
            .into_iter()
            .map(|(driver, route_index, departure)| {
                let vehicle = resolve_vehicle(&driver, departure, garage);
                // Created in sorted order, so that the internal ids of drivers are stable.
                let driver_id = Id::<InternalPerson>::create(&driver);
                assert!(
                    !population.persons.contains_key(&driver_id),
                    "The id {driver} of a transit driver is also the id of a person."
                );
                SimDeparture {
                    route: route_index,
                    id: departure.id.clone(),
                    time: departure.departure_time,
                    vehicle,
                    driver: driver_id,
                }
            })
            .collect();

        let result = Self {
            routes: sim_routes,
            departures,
        };
        result.assert_vehicles_are_not_shared_at_the_same_time();
        info!(
            "Built transit schedule for simulation with {} routes and {} departures.",
            result.routes.len(),
            result.departures.len()
        );
        result
    }

    pub fn routes(&self) -> &[SimTransitRoute] {
        &self.routes
    }

    pub fn departures(&self) -> &[SimDeparture] {
        &self.departures
    }

    pub fn route(&self, index: usize) -> &SimTransitRoute {
        &self.routes[index]
    }

    pub fn departure(&self, index: usize) -> &SimDeparture {
        &self.departures[index]
    }

    pub fn is_empty(&self) -> bool {
        self.routes.is_empty()
    }

    /// Scheduled time at which the departure arrives at its last stop.
    fn scheduled_end(&self, departure: &SimDeparture) -> SimTime {
        let last_stop = self.routes[departure.route]
            .stops
            .last()
            .expect("Routes have at least one stop.");
        departure.time + SimTime::from_duration(last_stop.arrival_offset)
    }

    /// A vehicle can only serve one departure at a time. Departures may share a vehicle if one
    /// ends at or before the start of the next one.
    fn assert_vehicles_are_not_shared_at_the_same_time(&self) {
        let windows: Vec<_> = self
            .departures
            .iter()
            .map(|departure| (departure.time, self.scheduled_end(departure), departure))
            .sorted_by_key(|(start, end, departure)| {
                (
                    departure.vehicle.internal(),
                    *start,
                    *end,
                    departure.driver.internal(),
                )
            })
            .collect();

        // The check stops at the first overlap, so the windows before it are disjoint and
        // comparing neighbours is enough.
        for pair in windows.windows(2) {
            let (_, previous_end, previous) = pair[0];
            let (start, _, departure) = pair[1];
            assert!(
                previous.vehicle != departure.vehicle || start >= previous_end,
                "Transit vehicle {} serves the departures of drivers {} and {} at the same time.",
                departure.vehicle,
                previous.driver,
                departure.driver
            );
        }
    }
}

impl SimTransitRoute {
    fn new(
        line: &TransitLine,
        route: &TransitRoute,
        schedule: &TransitSchedule,
        network: &Network,
    ) -> Self {
        assert!(
            !route.stops.is_empty(),
            "Transit route {} of line {} has no stops.",
            route.id,
            line.id
        );

        let stops: Vec<_> = route
            .stops
            .iter()
            .enumerate()
            .map(|(index, stop)| {
                let (arrival_offset, departure_offset) =
                    match (stop.arrival_offset, stop.departure_offset) {
                        (Some(arrival), Some(departure)) => (arrival, departure),
                        (Some(arrival), None) => (arrival, arrival),
                        (None, Some(departure)) => (departure, departure),
                        (None, None) => panic!(
                            "Stop {index} ({}) of transit route {} of line {} has neither an \
                             arrival nor a departure offset.",
                            stop.facility_id, route.id, line.id
                        ),
                    };
                let link = stop_link(&stop.facility_id, schedule, network);
                SimRouteStop {
                    facility: stop.facility_id.clone(),
                    partition: network.get_link(&link).partition,
                    link,
                    arrival_offset,
                    departure_offset,
                }
            })
            .collect();

        let start_link = route
            .network_route
            .first()
            .cloned()
            .unwrap_or_else(|| stops.first().unwrap().link.clone());
        let end_link = route
            .network_route
            .last()
            .cloned()
            .unwrap_or_else(|| stops.last().unwrap().link.clone());

        Self {
            line: line.id.clone(),
            route: route.id.clone(),
            mode: route.transport_mode.clone(),
            start_link,
            end_link,
            stops,
        }
    }
}

fn stop_link(
    facility_id: &Id<TransitStopFacility>,
    schedule: &TransitSchedule,
    network: &Network,
) -> Id<Link> {
    let facility = schedule
        .facilities()
        .get(facility_id)
        .unwrap_or_else(|| panic!("Transit stop facility {facility_id} does not exist."));
    let link = facility
        .link_ref_id
        .clone()
        .unwrap_or_else(|| panic!("Transit stop facility {facility_id} has no link."));
    assert!(
        network.links_with_ids().contains_key(&link),
        "Transit stop facility {facility_id} references link {link}, which is not part of the \
         network."
    );
    link
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
    use crate::simulation::config::{Config, Transit, TransitSimulation};
    use crate::simulation::id::Id;
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
    use crate::simulation::scenario::transit::{TransitSchedule, TransitStopFacility};
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
    use crate::simulation::scenario::{ControllerScenario, Scenario};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    const NETWORK: &str = "./assets/pt_tutorial/multimodalnetwork.xml";
    const SCHEDULE: &str = "./assets/pt_tutorial/transitschedule.xml";
    const VEHICLES: &str = "./assets/pt_tutorial/transitVehicles.xml";

    fn tutorial_schedule() -> TransitSimSchedule {
        let network = Network::from_file(NETWORK);
        let schedule = TransitSchedule::from_file(Path::new(SCHEDULE));
        let garage = Garage::from_file(&PathBuf::from(VEHICLES));
        TransitSimSchedule::build(&schedule, &network, &garage, &Population::default())
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

        let routes = schedule.routes();
        assert_eq!(2, routes.len());
        assert_eq!("1to3", routes[0].route.external());
        assert_eq!("3to1", routes[1].route.external());
        assert_eq!("Blue Line", routes[0].line.external());
        assert_eq!("train", routes[0].mode.external());
        assert_eq!("pt", routes[1].mode.external());
        assert_eq!(Id::<Link>::get_from_ext("11"), routes[0].start_link);
        assert_eq!(Id::<Link>::get_from_ext("33"), routes[0].end_link);

        let stops = &routes[0].stops;
        assert_eq!(3, stops.len());
        assert_eq!(
            Id::<TransitStopFacility>::get_from_ext("2a"),
            stops[1].facility
        );
        assert_eq!(Id::<Link>::get_from_ext("12"), stops[1].link);
        assert_eq!(0, stops[1].partition);
        // The first stop only has a departure offset, the last one only an arrival offset.
        assert_eq!(Duration::ZERO, stops[0].arrival_offset);
        assert_eq!(Duration::ZERO, stops[0].departure_offset);
        assert_eq!(Duration::from_secs(200), stops[1].arrival_offset);
        assert_eq!(Duration::from_secs(240), stops[1].departure_offset);
        assert_eq!(Duration::from_secs(540), stops[2].arrival_offset);
        assert_eq!(Duration::from_secs(540), stops[2].departure_offset);
    }

    #[deterministic_id_test]
    fn tutorial_departures_are_sorted_by_driver_and_get_ascending_driver_ids() {
        let schedule = tutorial_schedule();

        let departures = schedule.departures();
        assert_eq!(100, departures.len());
        let first = &departures[0];
        assert_eq!("pt_Blue Line_1to3_01", first.driver.external());
        assert_eq!(0, first.route);
        assert_eq!("01", first.id.external());
        assert_eq!(SimTime::from_secs(6 * 3600), first.time);
        assert_eq!("tr_1", first.vehicle.external());
        let last = departures.last().unwrap();
        assert_eq!("pt_Blue Line_3to1_50", last.driver.external());
        assert_eq!(1, last.route);

        assert!(
            departures
                .windows(2)
                .all(|pair| pair[0].driver.external() < pair[1].driver.external()
                    && pair[0].driver.internal() < pair[1].driver.internal())
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

        let scenario: ControllerScenario = Scenario::load(config).into();

        assert!(scenario.core.transit_sim_schedule.is_empty());
        assert_eq!(2, scenario.core.transit_garage.vehicles.len());
        assert!(Id::<InternalPerson>::try_get_from_ext("pt_Blue Line_1to3_01").is_none());
    }

    #[deterministic_id_test]
    fn schedule_based_transit_builds_schedule_in_controller_scenario() {
        let mut config = Config::default();
        config.network_mut().path = Some(NETWORK.into());
        config.set_transit(Transit {
            schedule_path: Some(SCHEDULE.into()),
            vehicles_path: Some(VEHICLES.into()),
            simulation: TransitSimulation::ScheduleBased,
            ..Transit::default()
        });

        let scenario: ControllerScenario = Scenario::load(config).into();

        assert_eq!(2, scenario.core.transit_sim_schedule.routes().len());
        assert_eq!(100, scenario.core.transit_sim_schedule.departures().len());
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
        TransitSimSchedule::build(&schedule, &network, &garage, population);
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
