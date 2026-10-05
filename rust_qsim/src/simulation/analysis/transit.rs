//! Public transport ridership, waiting, load, transfer and observed-demand tables.
//!
//! This build moves transit passengers by teleportation: a `travelled with pt` event names the
//! line, route, access and egress stop and the scheduled boarding time of one passenger trip, and
//! no transit vehicle ever drives through the network. Everything below is derived from those
//! records and the recorded schedule. Quantities that would need vehicle service events (actual
//! stop arrivals and departures) are reported as unavailable instead of being inferred.

use super::{
    AnalysisError, ExpectedJourney, ObservedLeg, TableSpec, csv, hour_start_seconds, io_error,
    number_opt, table_writer,
};
use crate::simulation::events::{
    EventTrait, PersonArrivalEvent, PersonDepartureEvent, PersonStuckEvent,
    PtTeleportationArrivalEvent,
};
use crate::simulation::scenario::transit::TransitSchedule;
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Write;
use std::path::Path;

/// Tolerance when comparing a recorded boarding time with a scheduled one. Event times carry
/// nanoseconds, the schedule is exact in the same unit, so this only absorbs `f64` rounding.
const TIME_EPSILON_SECONDS: f64 = 1e-6;

/// Every transit metric with its unit and the columns that identify one of its rows.
#[rustfmt::skip]
pub(super) const METRICS: &[(&str, &str, &str)] = &[
    (
        "boardings_sample",
        "persons",
        "hour_start_seconds,line_id,stop_id",
    ),
    (
        "alightings_sample",
        "persons",
        "hour_start_seconds,line_id,stop_id",
    ),
    ("boardings", "persons", "hour_start_seconds,line_id,stop_id"),
    (
        "alightings",
        "persons",
        "hour_start_seconds,line_id,stop_id",
    ),
    (
        "trips_sample",
        "trips",
        "hour_start_seconds,line_id,route_id",
    ),
    ("trips", "trips", "hour_start_seconds,line_id,route_id"),
    (
        "missed_services_sample",
        "trips",
        "hour_start_seconds,line_id,route_id",
    ),
    (
        "wait_observations",
        "trips",
        "hour_start_seconds,line_id,route_id",
    ),
    (
        "mean_wait_seconds",
        "seconds",
        "hour_start_seconds,line_id,route_id",
    ),
    (
        "in_vehicle_observations",
        "trips",
        "hour_start_seconds,line_id,route_id",
    ),
    (
        "mean_in_vehicle_seconds",
        "seconds",
        "hour_start_seconds,line_id,route_id",
    ),
    (
        "delay_observations",
        "trips",
        "hour_start_seconds,line_id,route_id",
    ),
    (
        "mean_arrival_delay_seconds",
        "seconds",
        "hour_start_seconds,line_id,route_id",
    ),
    ("wait_seconds", "seconds", "person_id,departure_seconds"),
    (
        "in_vehicle_seconds",
        "seconds",
        "person_id,departure_seconds",
    ),
    (
        "arrival_delay_seconds",
        "seconds",
        "person_id,departure_seconds",
    ),
    (
        "service_modeling",
        "category",
        "person_id,departure_seconds",
    ),
    ("outcome", "category", "person_id,departure_seconds"),
    (
        "outcome_trips_sample",
        "trips",
        "hour_start_seconds,service_modeling,outcome",
    ),
    (
        "outcome_trips",
        "trips",
        "hour_start_seconds,service_modeling,outcome",
    ),
    (
        "passengers_sample",
        "persons",
        "line_id,route_id,departure_id,segment_index",
    ),
    (
        "passengers",
        "persons",
        "line_id,route_id,departure_id,segment_index",
    ),
    (
        "capacity_persons",
        "persons",
        "line_id,route_id,departure_id,segment_index",
    ),
    (
        "load_factor",
        "ratio",
        "line_id,route_id,departure_id,segment_index",
    ),
    ("transit_legs", "legs", "person_id,journey_index"),
    ("transfers", "transfers", "person_id,journey_index"),
    ("access_seconds", "seconds", "person_id,journey_index"),
    ("egress_seconds", "seconds", "person_id,journey_index"),
    ("transfer_seconds", "seconds", "person_id,journey_index"),
    ("journey_wait_seconds", "seconds", "person_id,journey_index"),
    (
        "journey_in_vehicle_seconds",
        "seconds",
        "person_id,journey_index",
    ),
    ("availability_status", "category", "metric_group"),
    (
        "transit_observed",
        "persons",
        "scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row",
    ),
    (
        "transit_simulated_sample",
        "persons",
        "scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row",
    ),
    (
        "transit_simulated_expanded",
        "persons",
        "scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row",
    ),
    (
        "transit_residual",
        "persons",
        "scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row",
    ),
    (
        "transit_relative_error",
        "ratio",
        "scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row",
    ),
    (
        "transit_network_total_expanded",
        "persons",
        "scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row",
    ),
    ("transit_matched", "observations", "scope,metric"),
    ("transit_unmatched", "observations", "scope,metric"),
    ("transit_observed_total", "persons", "scope,metric"),
    ("transit_simulated_total", "persons", "scope,metric"),
    ("transit_bias", "persons", "scope,metric"),
    ("transit_mae", "persons", "scope,metric"),
    ("transit_rmse", "persons", "scope,metric"),
    ("transit_relative_bias", "ratio", "scope,metric"),
];

/// Schedule and vehicle capacity of the run, recorded so a standalone rerun reproduces the
/// automatic report. Absent in metadata written before transit analysis existed.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransitMetadata {
    facilities: Vec<FacilityMeta>,
    routes: Vec<RouteMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FacilityMeta {
    facility_id: String,
    /// `stop_area_id` of the facility: the station the stop belongs to.
    station_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RouteMeta {
    line_id: String,
    route_id: String,
    stops: Vec<StopMeta>,
    departures: Vec<DepartureMeta>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StopMeta {
    facility_id: String,
    arrival_offset_seconds: Option<f64>,
    departure_offset_seconds: Option<f64>,
}

impl StopMeta {
    /// A vehicle at the first stop only has a departure offset, at the last stop only an arrival
    /// offset; either side falls back to the other when the schedule omits one.
    fn boarding_offset(&self) -> Option<f64> {
        self.departure_offset_seconds
            .or(self.arrival_offset_seconds)
    }

    fn alighting_offset(&self) -> Option<f64> {
        self.arrival_offset_seconds
            .or(self.departure_offset_seconds)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DepartureMeta {
    departure_id: String,
    departure_seconds: f64,
    vehicle_id: Option<String>,
    /// Seats plus standing room of the vehicle's type; absent when the vehicle or its capacity
    /// is not defined in the run's vehicle file.
    capacity_persons: Option<u32>,
}

impl TransitMetadata {
    /// Snapshot the schedule with the capacity of each departure's vehicle. Lines, routes and
    /// facilities are sorted by external id so the recorded metadata is deterministic.
    pub fn from_schedule(schedule: &TransitSchedule, garage: &Garage) -> Self {
        let capacity_by_vehicle: BTreeMap<&str, u32> = garage
            .vehicles
            .values()
            .filter_map(|vehicle| {
                let capacity = garage.vehicle_types.get(&vehicle.vehicle_type)?.capacity?;
                Some((vehicle.id.external(), capacity.persons()))
            })
            .collect();
        let mut facilities: Vec<_> = schedule
            .facilities()
            .values()
            .map(|facility| FacilityMeta {
                facility_id: facility.id.external().to_owned(),
                station_id: facility.stop_area_id.clone(),
            })
            .collect();
        facilities.sort_by(|a, b| a.facility_id.cmp(&b.facility_id));
        let mut routes = Vec::new();
        for line in schedule.lines().values() {
            for route in line.routes.values() {
                routes.push(RouteMeta {
                    line_id: line.id.external().to_owned(),
                    route_id: route.id.external().to_owned(),
                    stops: route
                        .stops
                        .iter()
                        .map(|stop| StopMeta {
                            facility_id: stop.facility_id.external().to_owned(),
                            arrival_offset_seconds: stop
                                .arrival_offset
                                .map(|offset| offset.as_secs_f64()),
                            departure_offset_seconds: stop
                                .departure_offset
                                .map(|offset| offset.as_secs_f64()),
                        })
                        .collect(),
                    departures: route
                        .departures
                        .iter()
                        .map(|departure| DepartureMeta {
                            departure_id: departure.id.external().to_owned(),
                            departure_seconds: departure.departure_time.as_nanos() as f64 / 1e9,
                            vehicle_id: departure
                                .vehicle_ref_id
                                .as_ref()
                                .map(|id| id.external().to_owned()),
                            capacity_persons: departure
                                .vehicle_ref_id
                                .as_ref()
                                .and_then(|id| capacity_by_vehicle.get(id.external()).copied()),
                        })
                        .collect(),
                });
            }
        }
        routes.sort_by(|a, b| (&a.line_id, &a.route_id).cmp(&(&b.line_id, &b.route_id)));
        Self { facilities, routes }
    }
}

/// What the event stream recorded about one passenger transit leg.
enum TripRecord {
    /// A `travelled with pt` event: the leg was served by a teleported scheduled trip.
    Service(ServiceRecord),
    /// The leg ended without any service record, so nothing about its service is known.
    NoServiceRecord,
    Stuck,
    /// Departed and never arrived or got stuck.
    Incomplete,
}

struct ServiceRecord {
    line: String,
    route: String,
    access_stop: String,
    egress_stop: String,
    boarding_seconds: f64,
}

pub(super) struct TransitTrip {
    person: String,
    mode: String,
    departure_seconds: Option<f64>,
    arrival_seconds: Option<f64>,
    record: TripRecord,
}

/// Collects transit passenger trips from the replayed event stream, one batch of simultaneous
/// events at a time.
pub(super) struct TransitCollector {
    /// Modes known to carry transit passengers: the plan's pt-routed legs and every mode seen on
    /// a service record. Only those departures can end without a record.
    transit_modes: BTreeSet<String>,
    /// Latest departure of each person that has not arrived yet.
    open: BTreeMap<String, (String, f64)>,
    trips: Vec<TransitTrip>,
}

impl TransitCollector {
    pub(super) fn new(planned_transit_modes: BTreeSet<String>) -> Self {
        Self {
            transit_modes: planned_transit_modes,
            open: BTreeMap::new(),
            trips: Vec::new(),
        }
    }

    pub(super) fn process_timestamp(&mut self, events: &[Box<dyn EventTrait>], time: SimTime) {
        let seconds = time.as_nanos() as f64 / 1e9;
        // Service records first: the leg they describe was opened by an earlier batch, and a
        // departure of the same batch must not be mistaken for it. Only a leg with no open
        // departure falls back to a departure of this batch: a zero-duration leg.
        let mut zero_duration: BTreeSet<(String, String)> = BTreeSet::new();
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PtTeleportationArrivalEvent>() {
                let person = event.person.external().to_owned();
                let mode = event.mode.external().to_owned();
                self.transit_modes.insert(mode.clone());
                let mut departure_seconds = match self.open.get(&person) {
                    Some((open_mode, departure)) if *open_mode == mode => Some(*departure),
                    _ => None,
                };
                if departure_seconds.is_some() {
                    self.open.remove(&person);
                } else if events.iter().any(|candidate| {
                    candidate
                        .as_any()
                        .downcast_ref::<PersonDepartureEvent>()
                        .is_some_and(|departure| {
                            departure.person.external() == person
                                && departure.leg_mode.external() == mode
                        })
                }) {
                    departure_seconds = Some(seconds);
                    zero_duration.insert((person.clone(), mode.clone()));
                }
                self.trips.push(TransitTrip {
                    person,
                    mode,
                    departure_seconds,
                    arrival_seconds: Some(seconds),
                    record: TripRecord::Service(ServiceRecord {
                        line: event.line.external().to_owned(),
                        route: event.route.external().to_owned(),
                        access_stop: event.access_facility.external().to_owned(),
                        egress_stop: event.egress_facility.external().to_owned(),
                        boarding_seconds: event.boarding_time.as_nanos() as f64 / 1e9,
                    }),
                });
            }
        }
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
                let person = event.person.external();
                let mode = event.leg_mode.external();
                if let Some((open_mode, departure)) = self.open.get(person)
                    && open_mode == mode
                {
                    let departure = *departure;
                    if self.transit_modes.contains(mode) {
                        self.trips.push(TransitTrip {
                            person: person.to_owned(),
                            mode: mode.to_owned(),
                            departure_seconds: Some(departure),
                            arrival_seconds: Some(seconds),
                            record: TripRecord::NoServiceRecord,
                        });
                    }
                    self.open.remove(person);
                }
            }
        }
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PersonStuckEvent>() {
                let person = event.person.external();
                if let Some((mode, departure)) = self.open.remove(person)
                    && self.transit_modes.contains(&mode)
                {
                    self.trips.push(TransitTrip {
                        person: person.to_owned(),
                        mode,
                        departure_seconds: Some(departure),
                        arrival_seconds: None,
                        record: TripRecord::Stuck,
                    });
                }
            }
        }
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
                let person = event.person.external().to_owned();
                if zero_duration.contains(&(person.clone(), event.leg_mode.external().to_owned())) {
                    continue;
                }
                if let Some(previous) = self.open.insert(
                    person.clone(),
                    (event.leg_mode.external().to_owned(), seconds),
                ) {
                    self.close_incomplete(&person, previous);
                }
            }
        }
    }

    fn close_incomplete(&mut self, person: &str, (mode, departure): (String, f64)) {
        if self.transit_modes.contains(&mode) {
            self.trips.push(TransitTrip {
                person: person.to_owned(),
                mode,
                departure_seconds: Some(departure),
                arrival_seconds: None,
                record: TripRecord::Incomplete,
            });
        }
    }

    pub(super) fn finish(&mut self) {
        for (person, open) in std::mem::take(&mut self.open) {
            self.close_incomplete(&person, open);
        }
        // Stable order independent of the event interleaving of the partitions.
        self.trips.sort_by(|a, b| {
            (
                &a.person,
                a.departure_seconds.map(f64::to_bits),
                a.arrival_seconds.map(f64::to_bits),
                &a.mode,
            )
                .cmp(&(
                    &b.person,
                    b.departure_seconds.map(f64::to_bits),
                    b.arrival_seconds.map(f64::to_bits),
                    &b.mode,
                ))
        });
    }
}

/// Planned boarding at `access` matched against a scheduled departure of the route.
struct ScheduleMatch {
    departure: usize,
    from_stop: usize,
    to_stop: usize,
    scheduled_arrival_seconds: f64,
}

/// Find the scheduled departure a recorded trip rides: the departure whose time at the access
/// stop equals the recorded boarding time, with the egress stop later on the same route.
/// Returns nothing when the schedule does not explain the record, e.g. a different schedule
/// than the one the run used.
fn match_schedule(route: &RouteMeta, trip: &ServiceRecord) -> Option<ScheduleMatch> {
    for (from_stop, access) in route.stops.iter().enumerate() {
        if access.facility_id != trip.access_stop {
            continue;
        }
        let Some(boarding_offset) = access.boarding_offset() else {
            continue;
        };
        let Some((to_stop, egress)) = route
            .stops
            .iter()
            .enumerate()
            .skip(from_stop + 1)
            .find(|(_, stop)| stop.facility_id == trip.egress_stop)
        else {
            continue;
        };
        let Some(alighting_offset) = egress.alighting_offset() else {
            continue;
        };
        if let Some((departure, scheduled)) =
            route.departures.iter().enumerate().find(|(_, departure)| {
                (departure.departure_seconds + boarding_offset - trip.boarding_seconds).abs()
                    < TIME_EPSILON_SECONDS
            })
        {
            return Some(ScheduleMatch {
                departure,
                from_stop,
                to_stop,
                scheduled_arrival_seconds: scheduled.departure_seconds + alighting_offset,
            });
        }
    }
    None
}

/// One trip with every quantity that can be derived from it; `None` is "unavailable".
struct TripView<'a> {
    trip: &'a TransitTrip,
    outcome: &'static str,
    service_modeling: &'static str,
    wait_seconds: Option<f64>,
    in_vehicle_seconds: Option<f64>,
    arrival_delay_seconds: Option<f64>,
    schedule: Option<(usize, ScheduleMatch)>,
}

impl TripView<'_> {
    fn record(&self) -> Option<&ServiceRecord> {
        match &self.trip.record {
            TripRecord::Service(record) => Some(record),
            _ => None,
        }
    }
}

fn view<'a>(
    trip: &'a TransitTrip,
    routes: &BTreeMap<(&str, &str), usize>,
    metadata: Option<&TransitMetadata>,
) -> TripView<'a> {
    let TripRecord::Service(record) = &trip.record else {
        let outcome = match trip.record {
            TripRecord::NoServiceRecord => "no_service_record",
            TripRecord::Stuck => "stuck",
            _ => "incomplete",
        };
        return TripView {
            trip,
            outcome,
            service_modeling: "unrecorded",
            wait_seconds: None,
            in_vehicle_seconds: None,
            arrival_delay_seconds: None,
            schedule: None,
        };
    };
    let raw_wait = trip
        .departure_seconds
        .map(|departure| record.boarding_seconds - departure);
    // A passenger who reaches the stop after the scheduled departure cannot have boarded that
    // run: the wait is not a duration, so it stays unavailable instead of going negative.
    let missed = raw_wait.is_some_and(|wait| wait < 0.0);
    let in_vehicle = trip
        .arrival_seconds
        .map(|arrival| arrival - record.boarding_seconds)
        .filter(|duration| *duration >= 0.0);
    let schedule = metadata.and_then(|metadata| {
        let route = *routes.get(&(record.line.as_str(), record.route.as_str()))?;
        Some((route, match_schedule(&metadata.routes[route], record)?))
    });
    let delay = schedule
        .as_ref()
        .zip(trip.arrival_seconds)
        .map(|((_, matched), arrival)| arrival - matched.scheduled_arrival_seconds);
    TripView {
        trip,
        outcome: if missed { "missed_service" } else { "boarded" },
        service_modeling: "teleported",
        wait_seconds: raw_wait.filter(|wait| *wait >= 0.0),
        in_vehicle_seconds: in_vehicle,
        arrival_delay_seconds: delay,
        schedule,
    }
}

fn interval_start(seconds: f64, interval: u32) -> u64 {
    hour_start_seconds((seconds.max(0.0) * 1e9) as u64, interval)
}

#[derive(Default)]
struct LineAccumulator {
    trips: u64,
    missed: u64,
    wait: Mean,
    in_vehicle: Mean,
    delay: Mean,
}

#[derive(Default)]
struct Mean {
    sum: f64,
    count: u64,
}

impl Mean {
    fn add(&mut self, value: Option<f64>) {
        if let Some(value) = value {
            self.sum += value;
            self.count += 1;
        }
    }

    fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }
}

/// Result of writing the performance tables, for the module status.
pub(super) struct TransitSummary {
    pub(super) has_service_records: bool,
}

const TRIPS_HEADER: &str = "person_id,leg_mode,service_modeling,outcome,line_id,route_id,access_stop_id,egress_stop_id,departure_seconds,boarding_seconds,arrival_seconds,wait_seconds,in_vehicle_seconds,scheduled_arrival_seconds,arrival_delay_seconds,departure_id,vehicle_id";
const STOP_HEADER: &str =
    "hour_start_seconds,line_id,stop_id,boardings_sample,alightings_sample,boardings,alightings";
const LINE_HEADER: &str = "hour_start_seconds,line_id,route_id,trips_sample,trips,missed_services_sample,wait_observations,mean_wait_seconds,in_vehicle_observations,mean_in_vehicle_seconds,delay_observations,mean_arrival_delay_seconds";
const OUTCOME_HEADER: &str =
    "hour_start_seconds,service_modeling,outcome,outcome_trips_sample,outcome_trips";
const OCCUPANCY_HEADER: &str = "hour_start_seconds,line_id,route_id,departure_id,vehicle_id,segment_index,from_stop_id,to_stop_id,segment_departure_seconds,passengers_sample,passengers,capacity_persons,load_factor";
const JOURNEY_HEADER: &str = "person_id,journey_index,origin,destination,purpose,transit_legs,transfers,access_modes,access_seconds,egress_modes,egress_seconds,transfer_seconds,journey_wait_seconds,journey_in_vehicle_seconds,status";
const AVAILABILITY_HEADER: &str = "metric_group,availability_status,reason";

/// Simulated boardings and alightings per interval, line and stop, the basis of the stop table
/// and of the observed comparison.
type StopCounts = BTreeMap<(u64, String, String), (u64, u64)>;

/// Write every transit performance table. Tables are written even when the run recorded no
/// transit, so every report has the same files and a missing metric is visible as an empty
/// table plus an unavailable row in `transit_availability.csv`.
pub(super) fn write_tables(
    path: &Path,
    metadata: Option<&TransitMetadata>,
    collector: &TransitCollector,
    observed_legs: &[ObservedLeg],
    expected_journeys: &BTreeMap<String, Vec<ExpectedJourney>>,
    interval: u32,
    sample_size: f64,
) -> Result<(TransitSummary, StopCounts), AnalysisError> {
    let expansion = 1.0 / sample_size;
    let routes: BTreeMap<(&str, &str), usize> = metadata
        .map(|metadata| {
            metadata
                .routes
                .iter()
                .enumerate()
                .map(|(index, route)| ((route.line_id.as_str(), route.route_id.as_str()), index))
                .collect()
        })
        .unwrap_or_default();
    let views: Vec<_> = collector
        .trips
        .iter()
        .map(|trip| view(trip, &routes, metadata))
        .collect();

    write_trips(path, metadata, &views)?;

    let mut stops = StopCounts::new();
    let mut lines: BTreeMap<(u64, String, String), LineAccumulator> = BTreeMap::new();
    let mut outcomes: BTreeMap<(u64, &str, &str), u64> = BTreeMap::new();
    let mut loads: BTreeMap<(usize, usize), Vec<u64>> = BTreeMap::new();
    for view in &views {
        let hour = view
            .trip
            .departure_seconds
            .or(view.trip.arrival_seconds)
            .map_or(0, |seconds| interval_start(seconds, interval));
        *outcomes
            .entry((hour, view.service_modeling, view.outcome))
            .or_default() += 1;
        let Some(record) = view.record() else {
            continue;
        };
        let boarding_hour = interval_start(record.boarding_seconds, interval);
        stops
            .entry((
                boarding_hour,
                record.line.clone(),
                record.access_stop.clone(),
            ))
            .or_default()
            .0 += 1;
        if let Some(arrival) = view.trip.arrival_seconds {
            stops
                .entry((
                    interval_start(arrival, interval),
                    record.line.clone(),
                    record.egress_stop.clone(),
                ))
                .or_default()
                .1 += 1;
        }
        let line = lines
            .entry((boarding_hour, record.line.clone(), record.route.clone()))
            .or_default();
        line.trips += 1;
        line.missed += u64::from(view.outcome == "missed_service");
        line.wait.add(view.wait_seconds);
        line.in_vehicle.add(view.in_vehicle_seconds);
        line.delay.add(view.arrival_delay_seconds);
        if let Some((route, matched)) = &view.schedule {
            let segments = &mut loads.entry((*route, matched.departure)).or_insert_with(|| {
                vec![0; metadata.map_or(0, |m| m.routes[*route].stops.len().saturating_sub(1))]
            });
            for segment in matched.from_stop..matched.to_stop {
                segments[segment] += 1;
            }
        }
    }

    let mut writer = table_writer(path, "transit_stop_hourly.csv")?;
    writeln!(writer, "{STOP_HEADER}").map_err(io_error)?;
    for ((hour, line, stop), (boardings, alightings)) in &stops {
        writeln!(
            writer,
            "{hour},{},{},{boardings},{alightings},{:.6},{:.6}",
            csv(line),
            csv(stop),
            *boardings as f64 * expansion,
            *alightings as f64 * expansion
        )
        .map_err(io_error)?;
    }

    let mut writer = table_writer(path, "transit_line_summary.csv")?;
    writeln!(writer, "{LINE_HEADER}").map_err(io_error)?;
    for ((hour, line, route), acc) in &lines {
        writeln!(
            writer,
            "{hour},{},{},{},{:.6},{},{},{},{},{},{},{}",
            csv(line),
            csv(route),
            acc.trips,
            acc.trips as f64 * expansion,
            acc.missed,
            acc.wait.count,
            number_opt(acc.wait.mean()),
            acc.in_vehicle.count,
            number_opt(acc.in_vehicle.mean()),
            acc.delay.count,
            number_opt(acc.delay.mean()),
        )
        .map_err(io_error)?;
    }

    let mut writer = table_writer(path, "transit_outcomes.csv")?;
    writeln!(writer, "{OUTCOME_HEADER}").map_err(io_error)?;
    for ((hour, modeling, outcome), count) in &outcomes {
        writeln!(
            writer,
            "{hour},{modeling},{outcome},{count},{:.6}",
            *count as f64 * expansion
        )
        .map_err(io_error)?;
    }

    let has_capacity = write_occupancy(path, metadata, &loads, expansion, interval)?;
    let journeys_complete =
        write_journeys(path, collector, observed_legs, expected_journeys, &views)?;

    let service_trips = views.iter().filter(|view| view.record().is_some()).count();
    let matched_trips = views.iter().filter(|view| view.schedule.is_some()).count();
    let no_records = "no transit service records (travelled with pt events) in the final iteration";
    let schedule_reason = if service_trips == 0 {
        Some(no_records.to_owned())
    } else if metadata.is_none() {
        Some("no transit schedule is recorded for this run".to_owned())
    } else if matched_trips == 0 {
        Some("no recorded trip matches a scheduled departure and stop pair".to_owned())
    } else {
        None
    };
    let mut writer = table_writer(path, "transit_availability.csv")?;
    writeln!(writer, "{AVAILABILITY_HEADER}").map_err(io_error)?;
    let rows: [(&str, Option<String>, &str); 8] = [
        (
            "boardings_alightings",
            (service_trips == 0).then(|| no_records.to_owned()),
            "",
        ),
        (
            "wait_in_vehicle",
            (service_trips == 0).then(|| no_records.to_owned()),
            "",
        ),
        (
            "service_delay",
            schedule_reason.clone(),
            "arrival compared with the scheduled arrival at the egress stop; teleported service follows the schedule by construction",
        ),
        ("occupancy", schedule_reason.clone(), ""),
        (
            "load_factor",
            schedule_reason
                .clone()
                .or_else(|| (!has_capacity).then(|| "no vehicle capacity is recorded for the matched departures".to_owned())),
            "",
        ),
        (
            "access_egress_transfers",
            (!journeys_complete)
                .then(|| "no transit journey has complete observed components".to_owned()),
            "",
        ),
        (
            "missed_service",
            (service_trips == 0).then(|| no_records.to_owned()),
            "passenger reached the stop after the scheduled departure; vehicle-level missed stops need physical service events",
        ),
        (
            "physical_service",
            Some("transit vehicle service events are not recorded; public transport is modeled by teleportation".to_owned()),
            "",
        ),
    ];
    for (group, unavailable, note) in rows {
        let (status, reason) = match &unavailable {
            Some(reason) => ("unavailable", reason.as_str()),
            None => ("available", note),
        };
        writeln!(writer, "{},{status},{}", csv(group), csv(reason)).map_err(io_error)?;
    }

    Ok((
        TransitSummary {
            has_service_records: service_trips > 0,
        },
        stops,
    ))
}

fn write_trips(
    path: &Path,
    metadata: Option<&TransitMetadata>,
    views: &[TripView<'_>],
) -> Result<(), AnalysisError> {
    let mut writer = table_writer(path, "transit_trips.csv")?;
    writeln!(writer, "{TRIPS_HEADER}").map_err(io_error)?;
    for view in views {
        let trip = view.trip;
        let record = view.record();
        let schedule = view.schedule.as_ref();
        let departure = schedule.and_then(|(route, matched)| {
            metadata.map(|metadata| &metadata.routes[*route].departures[matched.departure])
        });
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv(&trip.person),
            csv(&trip.mode),
            view.service_modeling,
            view.outcome,
            record.map_or_else(String::new, |r| csv(&r.line)),
            record.map_or_else(String::new, |r| csv(&r.route)),
            record.map_or_else(String::new, |r| csv(&r.access_stop)),
            record.map_or_else(String::new, |r| csv(&r.egress_stop)),
            number_opt(trip.departure_seconds),
            number_opt(record.map(|r| r.boarding_seconds)),
            number_opt(trip.arrival_seconds),
            number_opt(view.wait_seconds),
            number_opt(view.in_vehicle_seconds),
            number_opt(schedule.map(|(_, matched)| matched.scheduled_arrival_seconds)),
            number_opt(view.arrival_delay_seconds),
            departure.map_or_else(String::new, |d| csv(&d.departure_id)),
            departure
                .and_then(|d| d.vehicle_id.as_deref())
                .map_or_else(String::new, csv),
        )
        .map_err(io_error)?;
    }
    Ok(())
}

/// Returns whether any row carries a load factor, i.e. a capacity was recorded.
fn write_occupancy(
    path: &Path,
    metadata: Option<&TransitMetadata>,
    loads: &BTreeMap<(usize, usize), Vec<u64>>,
    expansion: f64,
    interval: u32,
) -> Result<bool, AnalysisError> {
    let mut writer = table_writer(path, "transit_occupancy.csv")?;
    writeln!(writer, "{OCCUPANCY_HEADER}").map_err(io_error)?;
    let mut has_capacity = false;
    let Some(metadata) = metadata else {
        return Ok(false);
    };
    for ((route_index, departure_index), segments) in loads {
        let route = &metadata.routes[*route_index];
        let departure = &route.departures[*departure_index];
        for (segment, passengers) in segments.iter().enumerate() {
            let from = &route.stops[segment];
            let to = &route.stops[segment + 1];
            let departs = departure.departure_seconds + from.boarding_offset().unwrap_or(0.0);
            let expanded = *passengers as f64 * expansion;
            let load_factor = departure
                .capacity_persons
                .filter(|capacity| *capacity > 0)
                .map(|capacity| expanded / f64::from(capacity));
            has_capacity |= load_factor.is_some();
            writeln!(
                writer,
                "{},{},{},{},{},{segment},{},{},{departs:.6},{passengers},{expanded:.6},{},{}",
                interval_start(departs, interval),
                csv(&route.line_id),
                csv(&route.route_id),
                csv(&departure.departure_id),
                departure
                    .vehicle_id
                    .as_deref()
                    .map_or_else(String::new, csv),
                csv(&from.facility_id),
                csv(&to.facility_id),
                departure
                    .capacity_persons
                    .map_or_else(String::new, |capacity| capacity.to_string()),
                number_opt(load_factor),
            )
            .map_err(io_error)?;
        }
    }
    Ok(has_capacity)
}

/// Per-journey access, egress, waiting, in-vehicle and transfer times. Returns whether any
/// journey had every quantity available.
fn write_journeys(
    path: &Path,
    collector: &TransitCollector,
    observed_legs: &[ObservedLeg],
    expected_journeys: &BTreeMap<String, Vec<ExpectedJourney>>,
    views: &[TripView<'_>],
) -> Result<bool, AnalysisError> {
    let mut writer = table_writer(path, "transit_journeys.csv")?;
    writeln!(writer, "{JOURNEY_HEADER}").map_err(io_error)?;
    let legs: BTreeMap<(&str, usize), &ObservedLeg> = observed_legs
        .iter()
        .map(|leg| ((leg.person_id.as_str(), leg.leg_index), leg))
        .collect();
    // An observed leg and the trip that describes it share person, mode and departure instant.
    let trips: BTreeMap<(&str, &str, u64), &TripView<'_>> = views
        .iter()
        .filter_map(|view| {
            Some((
                (
                    view.trip.person.as_str(),
                    view.trip.mode.as_str(),
                    view.trip.departure_seconds?.to_bits(),
                ),
                view,
            ))
        })
        .collect();
    let mut any_complete = false;
    for (person, journeys) in expected_journeys {
        for journey in journeys {
            let components: Vec<_> = journey
                .leg_indices
                .iter()
                .zip(&journey.component_modes)
                .map(|(index, mode)| (legs.get(&(person.as_str(), *index)).copied(), mode))
                .collect();
            let is_transit = |component: &(Option<&ObservedLeg>, &String)| {
                collector.transit_modes.contains(component.1)
            };
            let Some(first) = components.iter().position(is_transit) else {
                continue;
            };
            let last = components
                .iter()
                .rposition(is_transit)
                .expect("one transit component exists");
            let leg_trips: Vec<Option<&TripView<'_>>> = components
                .iter()
                .filter(|component| is_transit(component))
                .map(|(leg, mode)| {
                    let leg = (*leg)?;
                    trips
                        .get(&(
                            person.as_str(),
                            mode.as_str(),
                            leg.departure_seconds.to_bits(),
                        ))
                        .copied()
                })
                .collect();
            let duration = |part: &[(Option<&ObservedLeg>, &String)]| -> Option<f64> {
                part.iter().try_fold(0.0, |sum, (leg, _)| {
                    let leg = (*leg)?;
                    Some(sum + leg.completion.duration(leg.departure_seconds)?)
                })
            };
            let access = duration(&components[..first]);
            let egress = duration(&components[last + 1..]);
            let sum = |select: fn(&TripView<'_>) -> Option<f64>| -> Option<f64> {
                leg_trips
                    .iter()
                    .try_fold(0.0, |sum, view| Some(sum + select((*view)?)?))
            };
            let wait = sum(|view| view.wait_seconds);
            let in_vehicle = sum(|view| view.in_vehicle_seconds);
            // Time between leaving one vehicle and boarding the next, walking and waiting
            // included; it needs both trips' records.
            let transfer = leg_trips.windows(2).try_fold(0.0, |sum, pair| {
                let previous = pair[0]?;
                let next = pair[1]?;
                Some(sum + next.record()?.boarding_seconds - previous.trip.arrival_seconds?)
            });
            let modes = |part: &[(Option<&ObservedLeg>, &String)]| {
                part.iter()
                    .map(|(_, mode)| mode.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            };
            let complete = [access, egress, wait, in_vehicle, transfer]
                .iter()
                .all(Option::is_some);
            any_complete |= complete;
            writeln!(
                writer,
                "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                csv(person),
                journey.journey_index,
                csv(&journey.origin),
                csv(&journey.destination),
                csv(&journey.purpose),
                leg_trips.len(),
                leg_trips.len() - 1,
                csv(&modes(&components[..first])),
                number_opt(access),
                csv(&modes(&components[last + 1..])),
                number_opt(egress),
                number_opt(transfer),
                number_opt(wait),
                number_opt(in_vehicle),
                if complete { "complete" } else { "incomplete" },
            )
            .map_err(io_error)?;
        }
    }
    Ok(any_complete)
}

const MATCHES_HEADER: &str = "scope,line_id,stop_id,station_id,period_start_seconds,metric,observed,transit_simulated_sample,expansion_factor,transit_simulated_expanded,transit_residual,transit_relative_error,transit_network_total_expanded,observation_source,source_row";
const UNMATCHED_HEADER: &str = "source_row,scope,line_id,stop_id,station_id,period_start_seconds,period_end_seconds,metric,unit,value,source,reason";
const SUMMARY_HEADER: &str = "scope,metric,transit_matched,transit_unmatched,transit_observed_total,transit_simulated_total,transit_bias,transit_mae,transit_rmse,transit_relative_bias";

#[derive(Debug, Deserialize)]
struct Observation {
    scope: String,
    line_id: String,
    stop_id: String,
    station_id: String,
    period_start_seconds: String,
    period_end_seconds: String,
    metric: String,
    unit: String,
    value: String,
    #[serde(default)]
    source: String,
}

struct Match {
    observation: Observation,
    row: usize,
    period_start: u64,
    observed: f64,
    simulated_sample: u64,
    network_total_sample: u64,
    provenance: String,
}

/// Write only the headers of the observed-demand tables.
pub(super) fn write_empty_observed(path: &Path) -> Result<(), AnalysisError> {
    for (name, header) in [
        ("transit_validation_matches.csv", MATCHES_HEADER),
        ("transit_validation_unmatched.csv", UNMATCHED_HEADER),
        ("transit_validation_summary.csv", SUMMARY_HEADER),
    ] {
        std::fs::write(path.join(name), format!("{header}\n")).map_err(io_error)?;
    }
    Ok(())
}

/// Compare supplied observed boardings and alightings with the simulated ones.
///
/// An observation names its entity by `scope`: `stop`, `station` (the stop's `stop_area_id`),
/// `line` or `line_stop`. The period has to be exactly one analysis interval. Counts are
/// expanded by the reciprocal of the sample size, like the road count validation.
pub(super) fn write_observed(
    path: &Path,
    source: &Path,
    metadata: Option<&TransitMetadata>,
    stops: &StopCounts,
    interval: u32,
    sample_size: f64,
) -> Result<(), AnalysisError> {
    let stations: BTreeMap<&str, &str> = metadata
        .into_iter()
        .flat_map(|metadata| &metadata.facilities)
        .filter_map(|facility| {
            Some((
                facility.facility_id.as_str(),
                facility.station_id.as_deref()?,
            ))
        })
        .collect();
    let mut known_stops: BTreeSet<&str> = stops.keys().map(|(_, _, stop)| stop.as_str()).collect();
    let mut known_lines: BTreeSet<&str> = stops.keys().map(|(_, line, _)| line.as_str()).collect();
    if let Some(metadata) = metadata {
        known_stops.extend(metadata.facilities.iter().map(|f| f.facility_id.as_str()));
        known_lines.extend(metadata.routes.iter().map(|route| route.line_id.as_str()));
    }
    let known_stations: BTreeSet<&str> = stations.values().copied().collect();

    let file = File::open(source).map_err(io_error)?;
    let mut reader = csv::Reader::from_reader(file);
    let mut matched = Vec::new();
    let mut unmatched: Vec<(usize, Observation, &'static str)> = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, record) in reader.deserialize::<Observation>().enumerate() {
        let row = index + 2;
        let observation = record.map_err(|error| {
            AnalysisError::new(format!(
                "invalid transit observation CSV row {row}: {error}"
            ))
        })?;
        let start = observation.period_start_seconds.trim().parse::<u64>().ok();
        let end = observation.period_end_seconds.trim().parse::<u64>().ok();
        let value = observation.value.trim().parse::<f64>().ok();
        let line = observation.line_id.trim();
        let stop = observation.stop_id.trim();
        let station = observation.station_id.trim();
        let required_ids_present = match observation.scope.as_str() {
            "stop" => !stop.is_empty(),
            "station" => !station.is_empty(),
            "line" => !line.is_empty(),
            "line_stop" => !line.is_empty() && !stop.is_empty(),
            _ => true,
        };
        let entity_known = match observation.scope.as_str() {
            "stop" => known_stops.contains(stop),
            "station" => {
                known_stations.contains(station)
                    || stops
                        .keys()
                        .any(|(_, _, s)| stations.get(s.as_str()) == Some(&station))
            }
            "line" => known_lines.contains(line),
            _ => known_lines.contains(line) && known_stops.contains(stop),
        };
        let reason = if !matches!(
            observation.scope.as_str(),
            "stop" | "station" | "line" | "line_stop"
        ) {
            Some("unsupported_scope")
        } else if !required_ids_present {
            Some("missing_entity_id")
        } else if !matches!(observation.metric.as_str(), "boardings" | "alightings") {
            Some("unsupported_metric")
        } else if !matches!(
            observation.unit.as_str(),
            "persons" | "person" | "passengers"
        ) {
            Some("unsupported_unit")
        } else if value.is_none_or(|value| !value.is_finite() || value < 0.0) {
            Some("invalid_observation_value")
        } else if let (Some(start), Some(end)) = (start, end) {
            if start.checked_add(u64::from(interval)) != Some(end)
                || start % u64::from(interval) != 0
            {
                Some("period_mismatch")
            } else if !seen.insert((
                observation.scope.clone(),
                line.to_owned(),
                stop.to_owned(),
                station.to_owned(),
                start,
                observation.metric.clone(),
            )) {
                Some("duplicate_observation_key")
            } else if stops.is_empty() {
                // The schedule alone knows the entities, but nothing was simulated to compare:
                // a zero here would be a made-up result, not an observation of no riders.
                Some("no_service_records")
            } else if !entity_known {
                Some("unknown_entity")
            } else {
                None
            }
        } else {
            Some("invalid_period")
        };
        if let Some(reason) = reason {
            unmatched.push((row, observation, reason));
            continue;
        }
        let period_start = start.expect("period checked above");
        let boardings = observation.metric == "boardings";
        let pick = |counts: &(u64, u64)| if boardings { counts.0 } else { counts.1 };
        let in_scope = |(hour, l, s): &(u64, String, String)| {
            *hour == period_start
                && match observation.scope.as_str() {
                    "stop" => s == stop,
                    "station" => stations.get(s.as_str()) == Some(&station),
                    "line" => l == line,
                    _ => l == line && s == stop,
                }
        };
        let simulated_sample = stops
            .iter()
            .filter(|(key, _)| in_scope(key))
            .map(|(_, counts)| pick(counts))
            .sum();
        let network_total_sample = stops
            .iter()
            .filter(|((hour, _, _), _)| *hour == period_start)
            .map(|(_, counts)| pick(counts))
            .sum();
        let provenance = if observation.source.trim().is_empty() {
            source.display().to_string()
        } else {
            format!("{}:{}", source.display(), observation.source.trim())
        };
        matched.push(Match {
            row,
            period_start,
            observed: value.expect("value checked above"),
            simulated_sample,
            network_total_sample,
            provenance,
            observation,
        });
    }

    let expansion = 1.0 / sample_size;
    let mut writer = table_writer(path, "transit_validation_matches.csv")?;
    writeln!(writer, "{MATCHES_HEADER}").map_err(io_error)?;
    for m in &matched {
        let expanded = m.simulated_sample as f64 * expansion;
        let residual = expanded - m.observed;
        writeln!(
            writer,
            "{},{},{},{},{},{},{:.6},{},{expansion:.6},{expanded:.6},{residual:.6},{},{:.6},{},{}",
            csv(&m.observation.scope),
            csv(m.observation.line_id.trim()),
            csv(m.observation.stop_id.trim()),
            csv(m.observation.station_id.trim()),
            m.period_start,
            csv(&m.observation.metric),
            m.observed,
            m.simulated_sample,
            // Blank when the observed reference is zero.
            if m.observed == 0.0 {
                String::new()
            } else {
                format!("{:.6}", residual / m.observed)
            },
            m.network_total_sample as f64 * expansion,
            csv(&m.provenance),
            m.row,
        )
        .map_err(io_error)?;
    }

    let mut writer = table_writer(path, "transit_validation_unmatched.csv")?;
    writeln!(writer, "{UNMATCHED_HEADER}").map_err(io_error)?;
    for (row, o, reason) in &unmatched {
        writeln!(
            writer,
            "{row},{},{},{},{},{},{},{},{},{},{},{reason}",
            csv(&o.scope),
            csv(o.line_id.trim()),
            csv(o.stop_id.trim()),
            csv(o.station_id.trim()),
            csv(&o.period_start_seconds),
            csv(&o.period_end_seconds),
            csv(&o.metric),
            csv(&o.unit),
            csv(&o.value),
            csv(&o.source),
        )
        .map_err(io_error)?;
    }

    // Summary per scope and metric. The totals are the denominators of the relative bias, and the
    // matched and unmatched counts show how much of the supplied data the comparison covers.
    let mut groups: BTreeMap<(String, String), (Vec<&Match>, u64)> = BTreeMap::new();
    for m in &matched {
        groups
            .entry((m.observation.scope.clone(), m.observation.metric.clone()))
            .or_default()
            .0
            .push(m);
    }
    for (_, o, _) in &unmatched {
        groups
            .entry((o.scope.clone(), o.metric.clone()))
            .or_default()
            .1 += 1;
    }
    let mut writer = table_writer(path, "transit_validation_summary.csv")?;
    writeln!(writer, "{SUMMARY_HEADER}").map_err(io_error)?;
    for ((scope, metric), (rows, unmatched_count)) in &groups {
        let residuals: Vec<f64> = rows
            .iter()
            .map(|m| m.simulated_sample as f64 * expansion - m.observed)
            .collect();
        let n = residuals.len() as f64;
        let observed_total: f64 = rows.iter().map(|m| m.observed).sum();
        let simulated_total: f64 = rows
            .iter()
            .map(|m| m.simulated_sample as f64 * expansion)
            .sum();
        let stat = |value: f64| (n > 0.0).then_some(value);
        writeln!(
            writer,
            "{},{},{},{unmatched_count},{observed_total:.6},{simulated_total:.6},{},{},{},{}",
            csv(scope),
            csv(metric),
            rows.len(),
            number_opt(stat(residuals.iter().sum::<f64>() / n)),
            number_opt(stat(residuals.iter().map(|r| r.abs()).sum::<f64>() / n)),
            number_opt(stat(
                (residuals.iter().map(|r| r * r).sum::<f64>() / n).sqrt()
            )),
            number_opt(
                (observed_total > 0.0).then(|| (simulated_total - observed_total) / observed_total)
            ),
        )
        .map_err(io_error)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::{
        AnalysisInputPaths, AnalysisRunMetadata, ExpectedLeg, PersonExpectedTravel,
        analyze_final_iteration, read_json,
    };
    use super::*;
    use crate::simulation::config::{Analysis, CompressionType};
    use crate::simulation::scenario::network::Network;
    use macros::deterministic_id_test;
    use std::fs;
    use std::path::PathBuf;

    const SCHEDULE: &str = r#"<transitSchedule>
        <transitStops>
            <stopFacility id="1" x="0" y="0" linkRefId="l"/>
            <stopFacility id="2a" x="1" y="0" linkRefId="l" stopAreaId="central"/>
            <stopFacility id="3" x="2" y="0" linkRefId="l" stopAreaId="central"/>
        </transitStops>
        <transitLine id="Blue">
            <transitRoute id="1to3">
                <transportMode>train</transportMode>
                <routeProfile>
                    <stop refId="1" departureOffset="00:00:00"/>
                    <stop refId="2a" arrivalOffset="00:03:20" departureOffset="00:04:00"/>
                    <stop refId="3" arrivalOffset="00:09:00"/>
                </routeProfile>
                <route><link refId="l"/></route>
                <departures>
                    <departure id="01" departureTime="00:16:40" vehicleRefId="tr_1"/>
                    <departure id="02" departureTime="00:33:20" vehicleRefId="tr_2"/>
                </departures>
            </transitRoute>
        </transitLine>
    </transitSchedule>"#;

    // tr_1 declares seats and standing room, tr_2's type declares no capacity.
    const VEHICLES: &str = r#"<vehicleDefinitions xmlns="http://www.matsim.org/files/dtd">
        <vehicleType id="with-capacity">
            <capacity><seats persons="3"/><standingRoom persons="1"/></capacity>
        </vehicleType>
        <vehicleType id="no-capacity"/>
        <vehicle id="tr_1" type="with-capacity"/>
        <vehicle id="tr_2" type="no-capacity"/>
    </vehicleDefinitions>"#;

    const OBSERVATIONS: &str = "scope,line_id,stop_id,station_id,period_start_seconds,period_end_seconds,metric,unit,value,source\n\
        stop,,1,,0,3600,boardings,persons,5,counter-a\n\
        line,Blue,,,0,3600,alightings,persons,8,counter-line\n\
        station,,,central,0,3600,boardings,persons,2,counter-station\n\
        line_stop,Blue,3,,0,3600,alightings,persons,0,counter-zero\n\
        stop,,zz,,0,3600,boardings,persons,1,counter-unknown\n\
        stop,,1,,0,1800,boardings,persons,1,counter-period\n\
        stop,,1,,0,3600,delay,persons,1,counter-metric\n\
        depot,,1,,0,3600,boardings,persons,1,counter-scope\n\
        stop,,1,,0,3600,boardings,persons,6,counter-duplicate\n";

    fn event(time: u32, body: String) -> (u32, String) {
        (time, format!("<event time=\"{time}\" {body}/>"))
    }

    fn departure(time: u32, person: &str, mode: &str) -> (u32, String) {
        event(
            time,
            format!(
                "type=\"departure\" person=\"{person}\" link=\"l\" legMode=\"{mode}\" computationalRoutingMode=\"{mode}\""
            ),
        )
    }

    fn arrival(time: u32, person: &str, mode: &str) -> (u32, String) {
        event(
            time,
            format!("type=\"arrival\" person=\"{person}\" link=\"l\" legMode=\"{mode}\""),
        )
    }

    fn service(time: u32, person: &str, boarding: u32, from: &str, to: &str) -> (u32, String) {
        event(
            time,
            format!(
                "type=\"travelled with pt\" person=\"{person}\" distance=\"1000\" mode=\"pt\" line=\"Blue\" route=\"1to3\" boardingTime=\"{boarding}\" accessFacility=\"{from}\" egressFacility=\"{to}\""
            ),
        )
    }

    fn leg(leg_index: usize, mode: &str) -> ExpectedLeg {
        ExpectedLeg {
            leg_index,
            mode: mode.to_owned(),
            departure_seconds: None,
            expected_travel_seconds: None,
            distance_meters: None,
            transit: mode == "pt",
        }
    }

    fn traveller(person: &str, modes: &[&str]) -> PersonExpectedTravel {
        let legs: Vec<_> = modes
            .iter()
            .enumerate()
            .map(|(index, mode)| leg(index * 2, mode))
            .collect();
        PersonExpectedTravel {
            person_id: person.to_owned(),
            journeys: vec![ExpectedJourney {
                journey_index: 0,
                origin: "home".to_owned(),
                destination: "work".to_owned(),
                origin_link: "l".to_owned(),
                destination_link: "l".to_owned(),
                purpose: "work".to_owned(),
                leg_indices: legs.iter().map(|leg| leg.leg_index).collect(),
                component_modes: modes.iter().map(|mode| (*mode).to_owned()).collect(),
                distance_meters: None,
                distance_provenance: "unavailable".to_owned(),
            }],
            legs,
        }
    }

    struct Run {
        dir: tempfile::TempDir,
    }

    impl Run {
        fn report(&self, name: &str) -> String {
            fs::read_to_string(self.dir.path().join("analysis").join(name)).unwrap()
        }

        fn status(&self, module: &str) -> serde_json::Value {
            let statuses: serde_json::Value =
                read_json(&self.dir.path().join("analysis/module_status.json")).unwrap();
            statuses
                .as_array()
                .unwrap()
                .iter()
                .find(|status| status["module"] == module)
                .unwrap()
                .clone()
        }
    }

    /// Runs the shared analysis on the given events (one partition) and plans, with the test
    /// schedule and vehicles recorded unless `with_schedule` is false.
    fn analyze(
        mut events: Vec<(u32, String)>,
        expected: Vec<PersonExpectedTravel>,
        with_schedule: bool,
        observations: Option<&str>,
    ) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let events_dir = dir.path().join("ITERS/it.0/events");
        fs::create_dir_all(&events_dir).unwrap();
        events.sort_by_key(|(time, _)| *time);
        let rows: String = events.into_iter().map(|(_, row)| row).collect();
        fs::write(
            events_dir.join("events.0.xml"),
            format!("<events>{rows}</events>"),
        )
        .unwrap();
        let inputs = dir.path().join("inputs");
        fs::create_dir_all(&inputs).unwrap();
        fs::write(inputs.join("schedule.xml"), SCHEDULE).unwrap();
        fs::write(inputs.join("vehicles.xml"), VEHICLES).unwrap();
        let schedule = TransitSchedule::from_file(&inputs.join("schedule.xml"));
        let garage = Garage::from_file(&inputs.join("vehicles.xml"));
        let mut metadata = AnalysisRunMetadata::from_run(
            0,
            // Half of the population is simulated, so counts are expanded by two.
            0.5,
            &garage,
            expected,
            AnalysisInputPaths::default(),
        );
        if with_schedule {
            metadata = metadata.with_transit(&schedule, &garage);
        }
        let transit_observed_data = observations.map(|rows| {
            let path = dir.path().join("observed_transit.csv");
            fs::write(&path, rows).unwrap();
            PathBuf::from("observed_transit.csv")
        });
        analyze_final_iteration(
            dir.path(),
            0,
            1,
            CompressionType::None,
            86400,
            &metadata,
            &Network::new(),
            &Analysis {
                enabled: true,
                interval_seconds: 3600,
                transit_observed_data,
                ..Analysis::default()
            },
        )
        .unwrap();
        Run { dir }
    }

    fn full_day() -> Vec<(u32, String)> {
        vec![
            // p1: walk, a full ride 1 -> 3 on departure 01 after a 100 s wait, walk.
            departure(600, "p1", "walk"),
            arrival(900, "p1", "walk"),
            departure(900, "p1", "pt"),
            service(1540, "p1", 1000, "1", "3"),
            arrival(1540, "p1", "pt"),
            departure(1540, "p1", "walk"),
            arrival(1600, "p1", "walk"),
            // p2: rides 1 -> 2a on departure 01, five seconds late.
            departure(950, "p2", "pt"),
            service(1205, "p2", 1000, "1", "2a"),
            arrival(1205, "p2", "pt"),
            // p3: reaches the stop after departure 02 left, so the service is missed.
            departure(2100, "p3", "pt"),
            service(2540, "p3", 2000, "1", "3"),
            arrival(2540, "p3", "pt"),
            // p4 arrives from a pt leg without any service record, p5 gets stuck.
            departure(3000, "p4", "pt"),
            arrival(3100, "p4", "pt"),
            departure(4000, "p5", "pt"),
            event(4100, "type=\"stuckAndAbort\" person=\"p5\"".to_owned()),
            // p6: walk, two pt legs with a transfer at 2a, walk.
            departure(1800, "p6", "walk"),
            arrival(1950, "p6", "walk"),
            departure(1950, "p6", "pt"),
            service(2205, "p6", 2000, "1", "2a"),
            arrival(2205, "p6", "pt"),
            departure(2205, "p6", "pt"),
            service(2540, "p6", 2240, "2a", "3"),
            arrival(2540, "p6", "pt"),
            departure(2540, "p6", "walk"),
            arrival(2600, "p6", "walk"),
        ]
    }

    fn full_day_plans() -> Vec<PersonExpectedTravel> {
        vec![
            traveller("p1", &["walk", "pt", "walk"]),
            traveller("p4", &["pt"]),
            traveller("p5", &["pt"]),
            traveller("p6", &["walk", "pt", "pt", "walk"]),
        ]
    }

    #[deterministic_id_test]
    fn exports_trips_with_waiting_in_vehicle_delay_and_missed_service() {
        let run = analyze(full_day(), full_day_plans(), true, None);
        let trips = run.report("transit_trips.csv");
        assert!(trips.contains("\"p1\",\"pt\",teleported,boarded,\"Blue\",\"1to3\",\"1\",\"3\",900.000000,1000.000000,1540.000000,100.000000,540.000000,1540.000000,0.000000,\"01\",\"tr_1\""));
        assert!(trips.contains("\"p2\",\"pt\",teleported,boarded,\"Blue\",\"1to3\",\"1\",\"2a\",950.000000,1000.000000,1205.000000,50.000000,205.000000,1200.000000,5.000000,\"01\",\"tr_1\""));
        // Reaching the stop after the departure leaves the wait unavailable, not negative.
        assert!(trips.contains("\"p3\",\"pt\",teleported,missed_service,\"Blue\",\"1to3\",\"1\",\"3\",2100.000000,2000.000000,2540.000000,,540.000000,2540.000000,0.000000,\"02\",\"tr_2\""));
        // Without a service record or after being stuck nothing about the service is inferred.
        assert!(trips.contains(
            "\"p4\",\"pt\",unrecorded,no_service_record,,,,,3000.000000,,3100.000000,,,,,,"
        ));
        assert!(trips.contains("\"p5\",\"pt\",unrecorded,stuck,,,,,4000.000000,,,,,,,,"));
        let outcomes = run.report("transit_outcomes.csv");
        assert!(outcomes.contains("0,teleported,boarded,4,8.000000"));
        assert!(outcomes.contains("0,teleported,missed_service,1,2.000000"));
        assert!(outcomes.contains("0,unrecorded,no_service_record,1,2.000000"));
        assert!(outcomes.contains("3600,unrecorded,stuck,1,2.000000"));
        let lines = run.report("transit_line_summary.csv");
        // Waits 100, 50, 50 and 35 (the missed trip has none); in-vehicle 540, 205, 540, 205, 300.
        assert!(lines.contains("0,\"Blue\",\"1to3\",5,10.000000,1,4,"));
        assert!(
            lines.contains("58.750000,5,358.000000,5,2.000000"),
            "{lines}"
        );
        assert_eq!(run.status("transit_performance")["status"], "complete");
    }

    #[deterministic_id_test]
    fn a_zero_duration_leg_keeps_its_departure_and_leaves_no_open_leg() {
        // Departure, service record and arrival share one timestamp.
        let run = analyze(
            vec![
                departure(1000, "p1", "pt"),
                service(1000, "p1", 1000, "1", "2a"),
                arrival(1000, "p1", "pt"),
            ],
            vec![traveller("p1", &["pt"])],
            true,
            None,
        );
        let trips = run.report("transit_trips.csv");
        assert!(trips.contains("\"p1\",\"pt\",teleported,boarded,\"Blue\",\"1to3\",\"1\",\"2a\",1000.000000,1000.000000,1000.000000,0.000000,0.000000,"));
        assert_eq!(
            trips.lines().count(),
            2,
            "no phantom incomplete leg: {trips}"
        );
    }

    #[deterministic_id_test]
    fn exports_boardings_alightings_and_occupancy_with_capacity() {
        let run = analyze(full_day(), full_day_plans(), true, None);
        let stops = run.report("transit_stop_hourly.csv");
        assert!(stops.contains("0,\"Blue\",\"1\",4,0,8.000000,0.000000"));
        assert!(stops.contains("0,\"Blue\",\"2a\",1,2,2.000000,4.000000"));
        assert!(stops.contains("0,\"Blue\",\"3\",0,3,0.000000,6.000000"));
        let occupancy = run.report("transit_occupancy.csv");
        // Departure 01 carries p1 and p2 first and p1 alone afterwards, against 3 + 1 persons.
        assert!(
            occupancy.contains("\"01\",\"tr_1\",0,\"1\",\"2a\",1000.000000,2,4.000000,4,1.000000")
        );
        assert!(
            occupancy.contains("\"01\",\"tr_1\",1,\"2a\",\"3\",1240.000000,1,2.000000,4,0.500000")
        );
        // tr_2's type declares no capacity, so the load exists but its factor does not.
        assert!(occupancy.contains("\"02\",\"tr_2\",0,\"1\",\"2a\",2000.000000,2,4.000000,,\n"));
        assert!(occupancy.contains("\"02\",\"tr_2\",1,\"2a\",\"3\",2240.000000,2,4.000000,,\n"));
    }

    #[deterministic_id_test]
    fn exports_access_egress_waiting_and_transfers_per_journey() {
        let run = analyze(full_day(), full_day_plans(), true, None);
        let journeys = run.report("transit_journeys.csv");
        assert!(journeys.contains("\"p1\",0,\"home\",\"work\",\"work\",1,0,\"walk\",300.000000,\"walk\",60.000000,0.000000,100.000000,540.000000,complete"));
        // One transfer at 2a: leaving the first vehicle at 2205 and boarding at 2240.
        assert!(journeys.contains("\"p6\",0,\"home\",\"work\",\"work\",2,1,\"walk\",150.000000,\"walk\",60.000000,35.000000,85.000000,505.000000,complete"));
        // The stuck passenger's journey keeps its leg but no derived time.
        assert!(journeys.contains("\"p5\",0,\"home\",\"work\",\"work\",1,0,\"\",0.000000,\"\",0.000000,0.000000,,,incomplete"));
        assert!(journeys.contains("\"p4\",0,\"home\",\"work\",\"work\",1,0,\"\",0.000000,\"\",0.000000,0.000000,,,incomplete"));
    }

    #[deterministic_id_test]
    fn compares_observed_demand_with_provenance_and_denominators() {
        let run = analyze(full_day(), full_day_plans(), true, Some(OBSERVATIONS));
        let matches = run.report("transit_validation_matches.csv");
        // 4 sampled boardings at stop 1 expand to 8 against 5 observed; 10 is the network total.
        assert!(matches.contains("\"stop\",\"\",\"1\",\"\",0,\"boardings\",5.000000,4,2.000000,8.000000,3.000000,0.600000,10.000000,\""));
        assert!(
            matches.contains("observed_transit.csv:counter-a\",2"),
            "{matches}"
        );
        assert!(matches.contains("\"line\",\"Blue\",\"\",\"\",0,\"alightings\",8.000000,5,2.000000,10.000000,2.000000,0.250000,"));
        assert!(matches.contains("\"station\",\"\",\"\",\"central\",0,\"boardings\",2.000000,1,2.000000,2.000000,0.000000,0.000000,"));
        // A zero observation has no relative error.
        assert!(matches.contains("\"line_stop\",\"Blue\",\"3\",\"\",0,\"alightings\",0.000000,3,2.000000,6.000000,6.000000,,"));
        let unmatched = run.report("transit_validation_unmatched.csv");
        for reason in [
            "unknown_entity",
            "period_mismatch",
            "unsupported_metric",
            "unsupported_scope",
            "duplicate_observation_key",
        ] {
            assert!(
                unmatched.contains(reason),
                "{reason} missing from {unmatched}"
            );
        }
        let summary = run.report("transit_validation_summary.csv");
        assert!(summary.contains(
            "\"stop\",\"boardings\",1,3,5.000000,8.000000,3.000000,3.000000,3.000000,0.600000"
        ));
        assert_eq!(run.status("transit_validation")["status"], "complete");
        assert!(
            run.report("index.html")
                .contains("transit-validation-matches")
        );
    }

    #[deterministic_id_test]
    fn absent_service_records_leave_metrics_unavailable() {
        // pt departures and arrivals without any service record, and no recorded schedule.
        let run = analyze(
            vec![departure(100, "p4", "pt"), arrival(200, "p4", "pt")],
            vec![traveller("p4", &["pt"])],
            false,
            Some(OBSERVATIONS),
        );
        let trips = run.report("transit_trips.csv");
        assert!(trips.contains("\"p4\",\"pt\",unrecorded,no_service_record"));
        for table in [
            "transit_stop_hourly.csv",
            "transit_line_summary.csv",
            "transit_occupancy.csv",
        ] {
            assert_eq!(
                run.report(table).lines().count(),
                1,
                "{table} must only hold its header"
            );
        }
        let availability = run.report("transit_availability.csv");
        for group in [
            "boardings_alightings",
            "wait_in_vehicle",
            "service_delay",
            "occupancy",
            "load_factor",
            "access_egress_transfers",
            "missed_service",
            "physical_service",
        ] {
            assert!(
                availability.contains(&format!("\"{group}\",unavailable,")),
                "{group} must be unavailable: {availability}"
            );
        }
        assert_eq!(run.status("transit_performance")["status"], "unavailable");
        // Without any simulated service nothing can be compared, not even against a zero.
        let unmatched = run.report("transit_validation_unmatched.csv");
        assert!(unmatched.contains("no_service_records"));
        assert_eq!(
            run.report("transit_validation_matches.csv").lines().count(),
            1
        );
    }

    #[deterministic_id_test]
    fn a_recorded_schedule_without_service_records_does_not_match_observations_to_zero() {
        let run = analyze(
            vec![departure(100, "p4", "pt"), arrival(200, "p4", "pt")],
            vec![traveller("p4", &["pt"])],
            true,
            Some(OBSERVATIONS),
        );
        assert_eq!(
            run.report("transit_validation_matches.csv").lines().count(),
            1
        );
        assert!(
            run.report("transit_validation_unmatched.csv")
                .contains("no_service_records")
        );
    }

    #[deterministic_id_test]
    fn missing_capacity_makes_only_the_load_factor_unavailable() {
        // Only departure 02 (tr_2, no capacity) is ridden.
        let run = analyze(
            vec![
                departure(1950, "p3", "pt"),
                service(2540, "p3", 2000, "1", "3"),
                arrival(2540, "p3", "pt"),
            ],
            Vec::new(),
            true,
            None,
        );
        let availability = run.report("transit_availability.csv");
        assert!(availability.contains("\"occupancy\",available,"));
        assert!(availability.contains("\"service_delay\",available,"));
        assert!(availability.contains("\"load_factor\",unavailable,\"no vehicle capacity is recorded for the matched departures\""));
        assert!(
            run.report("transit_occupancy.csv")
                .contains("\"02\",\"tr_2\",0,\"1\",\"2a\",2000.000000,1,2.000000,,\n")
        );
        // Physically modeled transit would need vehicle service events, which are never recorded.
        assert!(availability.contains(
            "\"physical_service\",unavailable,\"transit vehicle service events are not recorded"
        ));
    }

    #[test]
    fn schedule_match_requires_the_boarding_time_and_stop_order() {
        let route = RouteMeta {
            line_id: "Blue".to_owned(),
            route_id: "r".to_owned(),
            stops: ["a", "b", "a"]
                .iter()
                .enumerate()
                .map(|(index, stop)| StopMeta {
                    facility_id: (*stop).to_owned(),
                    arrival_offset_seconds: Some(index as f64 * 100.0),
                    departure_offset_seconds: Some(index as f64 * 100.0),
                })
                .collect(),
            departures: vec![DepartureMeta {
                departure_id: "d".to_owned(),
                departure_seconds: 1000.0,
                vehicle_id: None,
                capacity_persons: None,
            }],
        };
        let record = |boarding, from: &str, to: &str| ServiceRecord {
            line: "Blue".to_owned(),
            route: "r".to_owned(),
            access_stop: from.to_owned(),
            egress_stop: to.to_owned(),
            boarding_seconds: boarding,
        };
        // A loop route visits `a` twice: boarding at its second visit rides to no later `a`.
        assert_eq!(
            match_schedule(&route, &record(1000.0, "a", "b"))
                .unwrap()
                .to_stop,
            1
        );
        assert!(match_schedule(&route, &record(1200.0, "a", "b")).is_none());
        assert!(match_schedule(&route, &record(1100.0, "b", "a")).is_some());
        assert!(match_schedule(&route, &record(1001.0, "a", "b")).is_none());
        assert!(match_schedule(&route, &record(1000.0, "b", "a")).is_none());
    }
}

/// Metrics from this module that completed-run comparison can compare.
pub(super) const COMPARISON_TABLES: &[TableSpec] = &[
    TableSpec {
        file: "transit_trips.csv",
        metrics: &[
            ("wait_seconds", "wait_seconds"),
            ("in_vehicle_seconds", "in_vehicle_seconds"),
            ("arrival_delay_seconds", "arrival_delay_seconds"),
        ],
    },
    TableSpec {
        file: "transit_stop_hourly.csv",
        metrics: &[
            ("boardings_sample", "boardings_sample"),
            ("alightings_sample", "alightings_sample"),
            ("boardings", "boardings"),
            ("alightings", "alightings"),
        ],
    },
    TableSpec {
        file: "transit_line_summary.csv",
        metrics: &[
            ("trips_sample", "trips_sample"),
            ("trips", "trips"),
            ("missed_services_sample", "missed_services_sample"),
            ("wait_observations", "wait_observations"),
            ("mean_wait_seconds", "mean_wait_seconds"),
            ("in_vehicle_observations", "in_vehicle_observations"),
            ("mean_in_vehicle_seconds", "mean_in_vehicle_seconds"),
            ("delay_observations", "delay_observations"),
            ("mean_arrival_delay_seconds", "mean_arrival_delay_seconds"),
        ],
    },
    TableSpec {
        file: "transit_outcomes.csv",
        metrics: &[
            ("outcome_trips_sample", "outcome_trips_sample"),
            ("outcome_trips", "outcome_trips"),
        ],
    },
    TableSpec {
        file: "transit_occupancy.csv",
        metrics: &[
            ("passengers_sample", "passengers_sample"),
            ("passengers", "passengers"),
            ("capacity_persons", "capacity_persons"),
            ("load_factor", "load_factor"),
        ],
    },
    TableSpec {
        file: "transit_journeys.csv",
        metrics: &[
            ("transit_legs", "transit_legs"),
            ("transfers", "transfers"),
            ("access_seconds", "access_seconds"),
            ("egress_seconds", "egress_seconds"),
            ("transfer_seconds", "transfer_seconds"),
            ("journey_wait_seconds", "journey_wait_seconds"),
            ("journey_in_vehicle_seconds", "journey_in_vehicle_seconds"),
        ],
    },
    TableSpec {
        file: "transit_validation_matches.csv",
        metrics: &[
            ("transit_observed", "observed"),
            ("transit_simulated_sample", "transit_simulated_sample"),
            ("transit_simulated_expanded", "transit_simulated_expanded"),
            ("transit_residual", "transit_residual"),
            ("transit_relative_error", "transit_relative_error"),
            (
                "transit_network_total_expanded",
                "transit_network_total_expanded",
            ),
        ],
    },
    TableSpec {
        file: "transit_validation_summary.csv",
        metrics: &[
            ("transit_matched", "transit_matched"),
            ("transit_unmatched", "transit_unmatched"),
            ("transit_observed_total", "transit_observed_total"),
            ("transit_simulated_total", "transit_simulated_total"),
            ("transit_bias", "transit_bias"),
            ("transit_mae", "transit_mae"),
            ("transit_rmse", "transit_rmse"),
            ("transit_relative_bias", "transit_relative_bias"),
        ],
    },
];
