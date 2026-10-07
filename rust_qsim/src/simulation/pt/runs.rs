//! Vehicle runs ("Umlaeufe") built from the transit schedule.
//!
//! Port of MATSim's `ReconstructingUmlaufBuilder` and `UmlaufInterpolator`: every departure is
//! served by the vehicle its schedule entry names, a vehicle serves its departures in time order,
//! and a deadhead leg ("Wenden") routed on freespeed travel time joins two departures whose links
//! do not meet.

use crate::simulation::id::Id;
use crate::simulation::pt::driver::{DEADHEAD_ID, DRIVER_LEG_MODE, TRANSIT_ACTIVITY_TYPE};
use crate::simulation::scenario::network::{Link, Network, Node};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scenario::transit::{
    TransitDeparture, TransitLine, TransitRoute, TransitSchedule, TransitStopFacility,
};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use nohash_hasher::{IntMap, IntSet};
use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;
use std::sync::Arc;
use std::time::Duration;

/// A transit route prepared for driving: the links it follows and the stops it serves.
#[derive(Debug, PartialEq)]
pub struct ServiceRoute {
    pub line: Id<TransitLine>,
    pub route: Id<TransitRoute>,
    /// The full route, departure and arrival link included.
    pub links: Vec<Id<Link>>,
    link_lengths: Vec<f64>,
    pub stops: Vec<ServiceStop>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServiceStop {
    pub facility: Id<TransitStopFacility>,
    pub link: Id<Link>,
    pub arrival_offset: Option<Duration>,
    pub departure_offset: Option<Duration>,
    pub await_departure: bool,
    /// A blocking stop holds the vehicle in its lane, so traffic behind it waits too.
    pub is_blocking: bool,
}

impl ServiceRoute {
    /// Distance a passenger rides between two stop links. MATSim's
    /// `RouteUtils.calcDistance(TransitRoute, ...)`: the lengths of the links after the first
    /// occurrence of the access link, up to and including the egress link.
    pub fn ride_distance(&self, access_link: &Id<Link>, egress_link: &Id<Link>) -> f64 {
        let last = self.links.len() - 1;
        let mut distance = 0.0;
        let mut count = &self.links[0] == access_link;
        // MATSim iterates the intermediate links only and adds the arrival link afterwards.
        for (link, length) in self.links[1..last].iter().zip(&self.link_lengths[1..last]) {
            if count {
                distance += length;
            }
            if link == access_link {
                count = true;
            }
            if link == egress_link {
                count = false;
                break;
            }
        }
        if count {
            distance += self.link_lengths[last];
        }
        distance
    }
}

#[derive(Debug, PartialEq)]
pub enum RunLeg {
    /// A scheduled departure. MATSim's `UmlaufStueck`.
    Service {
        route: Arc<ServiceRoute>,
        departure: Id<TransitDeparture>,
        departure_time: SimTime,
    },
    /// An empty transfer between two departures. MATSim's `Wenden`.
    Deadhead { links: Vec<Id<Link>> },
}

impl RunLeg {
    pub fn links(&self) -> &[Id<Link>] {
        match self {
            RunLeg::Service { route, .. } => &route.links,
            RunLeg::Deadhead { links } => links,
        }
    }
}

/// One vehicle's day. MATSim's `Umlauf`.
#[derive(Debug, PartialEq)]
pub struct VehicleRun {
    pub driver: Id<InternalPerson>,
    pub vehicle: Id<InternalVehicle>,
    pub legs: Vec<RunLeg>,
}

impl VehicleRun {
    pub fn start_link(&self) -> &Id<Link> {
        &self.legs[0].links()[0]
    }
}

/// All vehicle runs of a scenario, built once and shared by every partition.
#[derive(Debug, Default, PartialEq)]
pub struct TransitVehicleRuns {
    runs: Vec<Arc<VehicleRun>>,
    drivers: IntSet<Id<InternalPerson>>,
}

impl TransitVehicleRuns {
    /// Runs in vehicle id order.
    pub fn runs(&self) -> &[Arc<VehicleRun>] {
        &self.runs
    }

    /// Transit drivers are synthetic agents, not members of the population.
    pub fn is_driver(&self, person: &Id<InternalPerson>) -> bool {
        self.drivers.contains(person)
    }

    /// Builds the runs and validates the schedule against the network and the vehicles, so a
    /// schedule MATSim would reject mid-simulation fails here with the offending entry named.
    pub fn build(
        schedule: &TransitSchedule,
        garage: &Garage,
        network: &Network,
    ) -> Result<Self, String> {
        let mut pieces = Vec::new();
        let mut lines: Vec<_> = schedule.lines().values().collect();
        lines.sort_by(|a, b| a.id.external().cmp(b.id.external()));
        for line in lines {
            let mut routes: Vec<_> = line.routes.values().collect();
            routes.sort_by(|a, b| a.id.external().cmp(b.id.external()));
            for route in routes {
                if route.departures.is_empty() {
                    continue;
                }
                let service = Arc::new(service_route(schedule, network, line.id.clone(), route)?);
                for departure in &route.departures {
                    let vehicle = departure
                        .vehicle_ref_id
                        .as_ref()
                        .and_then(|id| Id::<InternalVehicle>::try_get_from_ext(id.external()))
                        .filter(|id| garage.vehicles.contains_key(id))
                        .ok_or_else(|| {
                            format!(
                                "Departure {} of transit route {} on line {} names no vehicle of the vehicles file.",
                                departure.id, route.id, line.id
                            )
                        })?;
                    pieces.push((
                        departure.departure_time,
                        vehicle,
                        service.clone(),
                        departure.id.clone(),
                    ));
                }
            }
        }
        // MATSim sorts all departures by time. Lines and routes are visited in id order above and
        // departures in document order, which both readers preserve, so equal times keep a fixed
        // order. That tiebreak is this build's own: Java's order comes from an unordered map.
        pieces.sort_by_key(|(time, ..)| *time);

        let mut by_vehicle: IntMap<Id<InternalVehicle>, Vec<RunLeg>> = IntMap::default();
        for (departure_time, vehicle, route, departure) in pieces {
            let legs = by_vehicle.entry(vehicle).or_default();
            if let Some(previous) = legs.last() {
                // MATSim's `UmlaufInterpolator.addUmlaufStueckToUmlauf`: a deadhead joins two
                // consecutive pieces of a run when the last link of the previous one is not
                // the first link of the next. No deadhead is inserted before the first piece.
                let from = previous.links().last().unwrap();
                let to = &route.links[0];
                if from != to {
                    legs.push(RunLeg::Deadhead {
                        links: deadhead(network, from, to)?,
                    });
                }
            }
            legs.push(RunLeg::Service {
                route,
                departure,
                departure_time,
            });
        }

        // Drivers build their plans and events from these ids on the mobsim threads. Creating
        // them here, once, keeps id assignment independent of thread scheduling.
        Id::<String>::create(DRIVER_LEG_MODE);
        Id::<String>::create(TRANSIT_ACTIVITY_TYPE);
        Id::<TransitLine>::create(DEADHEAD_ID);
        Id::<TransitRoute>::create(DEADHEAD_ID);
        Id::<TransitDeparture>::create(DEADHEAD_ID);

        let mut vehicles: Vec<_> = by_vehicle.into_iter().collect();
        vehicles.sort_by(|(a, _), (b, _)| a.external().cmp(b.external()));
        let mut drivers = IntSet::default();
        let mut runs = Vec::with_capacity(vehicles.len());
        for (vehicle, legs) in vehicles {
            let vehicle_type = &garage.vehicles[&vehicle].vehicle_type;
            let capacity = garage.vehicle_types[vehicle_type]
                .capacity
                .map(|capacity| capacity.persons())
                .unwrap_or(0);
            if capacity == 0 {
                return Err(format!(
                    "Transit vehicle {vehicle} has type {vehicle_type}, which declares no passenger capacity."
                ));
            }
            // MATSim names the driver after the run, which it names after vehicle and type.
            let driver = Id::create(&format!("pt_{vehicle}_{vehicle_type}"));
            drivers.insert(driver.clone());
            runs.push(Arc::new(VehicleRun {
                driver,
                vehicle,
                legs,
            }));
        }
        Ok(Self { runs, drivers })
    }
}

fn service_route(
    schedule: &TransitSchedule,
    network: &Network,
    line: Id<TransitLine>,
    route: &TransitRoute,
) -> Result<ServiceRoute, String> {
    let describe = || format!("transit route {} on line {}", route.id, line);
    if route.network_route.is_empty() {
        return Err(format!("The {} has no network route.", describe()));
    }
    let link_lengths = route
        .network_route
        .iter()
        .map(|id| {
            network
                .links_with_ids()
                .get(id)
                .map(|link| link.length)
                .ok_or_else(|| {
                    format!(
                        "The {} uses link {id}, which is not in the network.",
                        describe()
                    )
                })
        })
        .collect::<Result<Vec<_>, _>>()?;

    let mut stops = Vec::with_capacity(route.stops.len());
    // A vehicle serves its next stop whenever it is on that stop's link, so the stops must
    // appear along the route in order; consecutive stops may share a link.
    let mut position = 0;
    for stop in &route.stops {
        let facility = schedule
            .facilities()
            .get(&stop.facility_id)
            .ok_or_else(|| {
                format!(
                    "The {} serves unknown stop {}.",
                    describe(),
                    stop.facility_id
                )
            })?;
        let link = facility
            .link_ref_id
            .clone()
            .ok_or_else(|| format!("Stop {} has no link.", facility.id))?;
        position = route.network_route[position..]
            .iter()
            .position(|id| *id == link)
            .map(|offset| position + offset)
            .ok_or_else(|| {
                format!(
                    "The {} serves stop {} on link {link}, which the route does not pass after its previous stop.",
                    describe(),
                    facility.id
                )
            })?;
        stops.push(ServiceStop {
            facility: facility.id.clone(),
            link,
            arrival_offset: stop.arrival_offset,
            departure_offset: stop.departure_offset,
            await_departure: stop.await_departure.unwrap_or(false),
            is_blocking: facility.is_blocking.unwrap_or(false),
        });
    }

    Ok(ServiceRoute {
        line,
        route: route.id.clone(),
        links: route.network_route.clone(),
        link_lengths,
        stops,
    })
}

/// The freespeed-fastest links from the end of `from` to the start of `to`, framed by both.
fn deadhead(network: &Network, from: &Id<Link>, to: &Id<Link>) -> Result<Vec<Id<Link>>, String> {
    let start = &network.get_link(from).to;
    let target = &network.get_link(to).from;
    let path = fastest_path(network, start, target).ok_or_else(|| {
        format!("No deadhead route from link {from} to link {to}: node {target} is unreachable from node {start}.")
    })?;
    let mut links = Vec::with_capacity(path.len() + 2);
    links.push(from.clone());
    links.extend(path);
    links.push(to.clone());
    Ok(links)
}

#[derive(PartialEq)]
struct Entry(f64, u64);

impl Eq for Entry {}

impl PartialOrd for Entry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Entry {
    fn cmp(&self, other: &Self) -> Ordering {
        // Travel times are finite sums of positive link times; ties fall back to the node id so
        // the chosen path does not depend on heap internals.
        self.0.total_cmp(&other.0).then(self.1.cmp(&other.1))
    }
}

/// Dijkstra on `length / freespeed`, MATSim's `FreespeedTravelTimeAndDisutility` with the
/// default zero distance cost.
fn fastest_path(network: &Network, start: &Id<Node>, target: &Id<Node>) -> Option<Vec<Id<Link>>> {
    let mut best: IntMap<Id<Node>, f64> = IntMap::default();
    let mut via: IntMap<Id<Node>, Id<Link>> = IntMap::default();
    let mut heap = BinaryHeap::new();
    best.insert(start.clone(), 0.0);
    heap.push(Reverse(Entry(0.0, start.internal())));
    while let Some(Reverse(Entry(time, node))) = heap.pop() {
        let node = Id::<Node>::get(node);
        if &node == target {
            let mut path = Vec::new();
            let mut at = node;
            while &at != start {
                let link = via[&at].clone();
                at = network.get_link(&link).from.clone();
                path.push(link);
            }
            path.reverse();
            return Some(path);
        }
        if time > best[&node] {
            continue;
        }
        for link_id in &network.get_node(&node).out_links {
            let link = network.get_link(link_id);
            // A link whose freespeed is zero or negative has no usable freespeed travel time:
            // the division is infinite or negative and would poison the search. `routing::cost`
            // classifies the same case instead of letting it reach a queue, so skip the link.
            let travel = link.length / link.freespeed;
            if !travel.is_finite() || travel < 0.0 {
                continue;
            }
            let arrival = time + travel;
            if best.get(&link.to).is_none_or(|known| arrival < *known) {
                best.insert(link.to.clone(), arrival);
                via.insert(link.to.clone(), link_id.clone());
                heap.push(Reverse(Entry(arrival, link.to.internal())));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use macros::deterministic_id_test;
    use std::path::PathBuf;

    fn pt_tutorial() -> (TransitSchedule, Garage, Network) {
        let schedule =
            TransitSchedule::from_file(&PathBuf::from("./assets/pt_tutorial/transitschedule.xml"));
        let garage = Garage::from_file(&PathBuf::from("./assets/pt_tutorial/transitVehicles.xml"));
        let network =
            Network::from_file_as_is(&PathBuf::from("./assets/pt_tutorial/multimodalnetwork.xml"));
        (schedule, garage, network)
    }

    #[deterministic_id_test]
    fn tutorial_vehicles_alternate_directions_without_deadheads() {
        let (schedule, garage, network) = pt_tutorial();
        let runs = TransitVehicleRuns::build(&schedule, &garage, &network).unwrap();

        let drivers: Vec<_> = runs.runs().iter().map(|r| r.driver.external()).collect();
        assert_eq!(vec!["pt_tr_1_1", "pt_tr_2_1"], drivers);
        assert!(runs.is_driver(&Id::get_from_ext("pt_tr_1_1")));

        let first = &runs.runs()[0];
        let services: Vec<_> = first
            .legs
            .iter()
            .map(|leg| match leg {
                RunLeg::Service {
                    route,
                    departure_time,
                    ..
                } => (route.route.external().to_owned(), departure_time.as_secs()),
                RunLeg::Deadhead { .. } => panic!("unexpected deadhead"),
            })
            .collect();
        // Matches the TransitDriverStarts sequence of tr_1 in MATSim's pt-tutorial run.
        assert_eq!(
            vec![
                ("1to3".to_owned(), 21600),
                ("3to1".to_owned(), 22500),
                ("1to3".to_owned(), 23400),
                ("3to1".to_owned(), 24000),
            ],
            services[..4]
        );
    }

    #[deterministic_id_test]
    fn unconnected_departures_get_a_freespeed_deadhead() {
        let (mut schedule, garage, network) = pt_tutorial();
        // One vehicle serves two 1to3 departures in a row, so it has to return from link 33
        // to link 11 empty in between.
        let line = schedule
            .lines_mut()
            .get_mut(&Id::get_from_ext("Blue Line"))
            .unwrap();
        for route in line.routes.values_mut() {
            if route.id.external() == "1to3" {
                route.departures.truncate(2);
                for departure in &mut route.departures {
                    departure.vehicle_ref_id = Some(Id::get_from_ext("tr_1"));
                }
            } else {
                route.departures.clear();
            }
        }

        let runs = TransitVehicleRuns::build(&schedule, &garage, &network).unwrap();
        assert_eq!(1, runs.runs().len());
        let run = &runs.runs()[0];
        assert_eq!(3, run.legs.len());
        let RunLeg::Deadhead { links } = &run.legs[1] else {
            panic!("expected a deadhead between the two departures");
        };
        assert_eq!("33", links.first().unwrap().external());
        assert_eq!("11", links.last().unwrap().external());
        for pair in links.windows(2) {
            assert_eq!(
                network.get_link(&pair[0]).to,
                network.get_link(&pair[1]).from
            );
        }
    }

    #[deterministic_id_test]
    fn ride_distance_counts_links_after_access_through_egress() {
        let (schedule, garage, network) = pt_tutorial();
        let runs = TransitVehicleRuns::build(&schedule, &garage, &network).unwrap();
        let RunLeg::Service { route, .. } = &runs.runs()[0].legs[0] else {
            panic!()
        };
        let length = |id: &str| network.get_link(&Id::get_from_ext(id)).length;
        let link = |id: &str| Id::<Link>::get_from_ext(id);
        // 1to3 drives 11 12 23 33.
        assert_eq!(
            length("12") + length("23") + length("33"),
            route.ride_distance(&link("11"), &link("33"))
        );
        assert_eq!(length("12"), route.ride_distance(&link("11"), &link("12")));
        assert_eq!(
            length("23") + length("33"),
            route.ride_distance(&link("12"), &link("33"))
        );
    }

    #[deterministic_id_test]
    fn departure_without_known_vehicle_is_rejected() {
        let (mut schedule, garage, network) = pt_tutorial();
        let unknown = Id::create("no_such_vehicle");
        for line in schedule.lines_mut().values_mut() {
            for route in line.routes.values_mut() {
                for departure in &mut route.departures {
                    departure.vehicle_ref_id = Some(unknown.clone());
                }
            }
        }
        let error = TransitVehicleRuns::build(&schedule, &garage, &network).unwrap_err();
        assert!(error.contains("names no vehicle"), "{error}");
    }

    #[deterministic_id_test]
    fn stop_off_the_route_is_rejected() {
        let (mut schedule, garage, network) = pt_tutorial();
        let line = schedule
            .lines_mut()
            .get_mut(&Id::get_from_ext("Blue Line"))
            .unwrap();
        let route = line.routes.get_mut(&Id::get_from_ext("1to3")).unwrap();
        // Stop 2b lies on link 32, which 1to3 never drives.
        route.stops[1].facility_id = Id::get_from_ext("2b");
        let error = TransitVehicleRuns::build(&schedule, &garage, &network).unwrap_err();
        assert!(error.contains("stop 2b on link 32"), "{error}");
    }
}
