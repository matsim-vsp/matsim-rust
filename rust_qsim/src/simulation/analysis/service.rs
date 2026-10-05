//! DRT and taxi service performance from supplied request, fleet, schedule and passenger
//! association records. Nothing is simulated: every metric is derived from the supplied tables.

use super::{
    AnalysisError, classify_link_to_boundary, csv, io_error, label, mean, number_opt, quantile,
    std_dev, table_writer,
};
use crate::simulation::config::ServiceInputs;
use crate::simulation::scenario::network::{Link, Network};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::Path;

const REQUESTS_HEADER: &str = "request_id,person_id,group,status,vehicle_id,submission_seconds,pickup_seconds,dropoff_seconds,wait_seconds,ride_seconds,direct_travel_seconds,detour_ratio,service_area,wait_limit_exceeded";
const SUMMARY_HEADER: &str = "scope,group,requests,served,rejected,unserved,served_share,rejected_share,passengers_served,wait_mean_seconds,wait_std_seconds,wait_median_seconds,wait_p90_seconds,detour_mean_ratio,detour_std_ratio,detour_median_ratio,detour_p90_ratio,wait_limit_exceeded,inside_area,outside_area,area_unknown,coverage_share";
const VEHICLES_HEADER: &str = "scope,vehicle_id,capacity,service_seconds,busy_seconds,utilization,driven_meters,occupied_meters,empty_meters,empty_share,passenger_meters,mean_occupancy,load_factor,capacity_exceeded_tasks,requests_served";
const OCCUPANCY_HEADER: &str = "load_passengers,load_vehicle_meters,load_share";
const CONSTRAINTS_HEADER: &str = "constraint,value";
const AVAILABILITY_HEADER: &str = "component,status,reason";
const DIAGNOSTICS_HEADER: &str = "source,row,reason,detail";

const TABLES: [(&str, &str); 7] = [
    ("service_requests.csv", REQUESTS_HEADER),
    ("service_summary.csv", SUMMARY_HEADER),
    ("service_vehicles.csv", VEHICLES_HEADER),
    ("service_occupancy.csv", OCCUPANCY_HEADER),
    ("service_constraints.csv", CONSTRAINTS_HEADER),
    ("service_availability.csv", AVAILABILITY_HEADER),
    ("service_diagnostics.csv", DIAGNOSTICS_HEADER),
];

#[derive(Deserialize)]
struct RequestRow {
    request_id: String,
    person_id: Option<String>,
    submission_seconds: f64,
    origin_link: String,
    destination_link: String,
    /// Empty or `submitted` for an open request, `rejected` for a recorded rejection.
    status: Option<String>,
    group: Option<String>,
    direct_travel_seconds: Option<f64>,
    party_size: Option<u64>,
}

#[derive(Deserialize)]
struct PassengerRow {
    request_id: String,
    vehicle_id: String,
    pickup_seconds: f64,
    dropoff_seconds: f64,
}

#[derive(Deserialize)]
struct FleetRow {
    vehicle_id: String,
    capacity: Option<u64>,
    service_start_seconds: Option<f64>,
    service_end_seconds: Option<f64>,
}

#[derive(Deserialize)]
struct TaskRow {
    vehicle_id: String,
    /// `drive`, `stop` or `stay`; only `drive` carries distance and only `stay` is idle.
    task_type: String,
    start_seconds: f64,
    end_seconds: f64,
    distance_meters: Option<f64>,
}

struct Request {
    person_id: String,
    group: Option<String>,
    submission: f64,
    /// `inside`, `outside`, `unknown` (a link is not in the network) or empty without an area.
    area: &'static str,
    rejected: bool,
    direct: Option<f64>,
    party: u64,
}

struct Association {
    /// Row of the passenger record, kept for diagnostics.
    row: usize,
    vehicle: String,
    pickup: f64,
    dropoff: f64,
}

#[derive(Default)]
struct Vehicle {
    capacity: Option<u64>,
    window: Option<f64>,
    /// Service window start and end; busy time outside it is not counted towards utilization.
    bounds: Option<(f64, f64)>,
    busy: f64,
    windowed_busy: f64,
    driven: f64,
    occupied: f64,
    passenger_meters: f64,
    capacity_meters: f64,
    /// Passenger-metres of vehicles with a known capacity, the numerator matching `capacity_meters`.
    capacity_passenger_meters: f64,
    exceeded: u64,
    served: u64,
}

impl Vehicle {
    fn add(&mut self, other: &Vehicle) {
        self.busy += other.busy;
        self.windowed_busy += other.windowed_busy;
        self.driven += other.driven;
        self.occupied += other.occupied;
        self.passenger_meters += other.passenger_meters;
        self.capacity_meters += other.capacity_meters;
        self.capacity_passenger_meters += other.capacity_passenger_meters;
        self.exceeded += other.exceeded;
        self.served += other.served;
        self.window = Some(self.window.unwrap_or(0.0) + other.window.unwrap_or(0.0));
    }
}

/// Which optional inputs were supplied; every metric that needs a missing one is left blank.
#[derive(Clone, Copy)]
struct Supplied {
    passengers: bool,
    fleet: bool,
    schedule: bool,
}

#[derive(Default)]
struct Diagnostics(Vec<[String; 4]>);

impl Diagnostics {
    fn add(&mut self, source: &str, row: usize, reason: &str, detail: &str) {
        self.0.push([
            source.to_owned(),
            row.to_string(),
            reason.to_owned(),
            detail.to_owned(),
        ]);
    }
}

fn read<T: DeserializeOwned>(path: &Path, what: &str) -> Result<Vec<(usize, T)>, AnalysisError> {
    let mut reader = csv::Reader::from_path(path).map_err(|error| {
        AnalysisError::new(format!("cannot read {what} {}: {error}", path.display()))
    })?;
    reader
        .deserialize()
        .enumerate()
        .map(|(index, row)| {
            row.map(|row| (index + 2, row)).map_err(|error| {
                AnalysisError::new(format!("invalid {what} row {}: {error}", index + 2))
            })
        })
        .collect()
}

fn finite(value: f64, what: &str, row: usize) -> Result<f64, AnalysisError> {
    if value.is_finite() {
        Ok(value)
    } else {
        Err(AnalysisError::new(format!(
            "{what} row {row} has a non-finite number"
        )))
    }
}

/// Read the supplied records and write every service table. Input errors fail only this module.
pub(super) fn write(
    report_dir: &Path,
    output_dir: &Path,
    inputs: &ServiceInputs,
    network: &Network,
    links: &[&Link],
) -> Result<(), AnalysisError> {
    if let Some(area) = &inputs.service_area
        && (area.len() < 3 || area.iter().flatten().any(|value| !value.is_finite()))
    {
        return Err(AnalysisError::new(
            "analysis.service.service_area must contain at least three finite coordinates",
        ));
    }
    if inputs
        .max_wait_seconds
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err(AnalysisError::new(
            "analysis.service.max_wait_seconds must be a non-negative finite number",
        ));
    }
    let resolve = |path: &Path| {
        if path.is_absolute() {
            path.to_path_buf()
        } else {
            output_dir.join(path)
        }
    };
    let supplied = Supplied {
        passengers: inputs.passengers.is_some(),
        fleet: inputs.fleet.is_some(),
        schedule: inputs.schedule.is_some(),
    };
    let mut diagnostics = Diagnostics::default();

    // A link is inside the area only when its whole segment is, so a link crossing the border
    // is not claimed as covered.
    let inside_links: BTreeMap<&str, bool> = inputs
        .service_area
        .as_ref()
        .map(|area| {
            links
                .iter()
                .filter_map(|link| {
                    let from = network.nodes_with_ids().get(&link.from)?;
                    let to = network.nodes_with_ids().get(&link.to)?;
                    Some((
                        link.id.external(),
                        classify_link_to_boundary(from, to, area) == "inner",
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut requests = BTreeMap::new();
    for (row, request) in read::<RequestRow>(&resolve(&inputs.requests), "request")? {
        finite(request.submission_seconds, "request", row)?;
        if let Some(direct) = request.direct_travel_seconds {
            finite(direct, "request", row)?;
            if direct <= 0.0 {
                diagnostics.add(
                    "requests",
                    row,
                    "non_positive_direct_travel",
                    &request.request_id,
                );
            }
        }
        if request.party_size == Some(0) {
            return Err(AnalysisError::new(format!(
                "request row {row} has party_size 0"
            )));
        }
        let rejected = match request.status.as_deref().map(str::trim) {
            None | Some("") | Some("submitted") => false,
            Some("rejected") => true,
            Some(other) => {
                return Err(AnalysisError::new(format!(
                    "request row {row} has unknown status {other:?}; expected submitted or rejected"
                )));
            }
        };
        let area = match (
            &inputs.service_area,
            inside_links.get(request.origin_link.as_str()),
            inside_links.get(request.destination_link.as_str()),
        ) {
            (None, ..) => "",
            (Some(_), Some(true), Some(true)) => "inside",
            (Some(_), Some(_), Some(_)) => "outside",
            _ => "unknown",
        };
        let id = request.request_id;
        let parsed = Request {
            person_id: request.person_id.unwrap_or_default(),
            group: request.group.filter(|group| !group.trim().is_empty()),
            submission: request.submission_seconds,
            area,
            rejected,
            direct: request.direct_travel_seconds.filter(|direct| *direct > 0.0),
            party: request.party_size.unwrap_or(1),
        };
        if requests.insert(id.clone(), parsed).is_some() {
            return Err(AnalysisError::new(format!(
                "request row {row} repeats request_id {id}"
            )));
        }
    }

    let mut associations: BTreeMap<String, Association> = BTreeMap::new();
    if let Some(path) = &inputs.passengers {
        for (row, passenger) in read::<PassengerRow>(&resolve(path), "passenger")? {
            finite(passenger.pickup_seconds, "passenger", row)?;
            finite(passenger.dropoff_seconds, "passenger", row)?;
            let id = passenger.request_id;
            let reason = match requests.get(&id) {
                None => Some("unknown_request"),
                Some(request) if request.rejected => Some("rejected_request_has_passenger"),
                Some(_) if associations.contains_key(&id) => Some("duplicate_association"),
                Some(request) if passenger.pickup_seconds < request.submission => {
                    Some("pickup_before_submission")
                }
                Some(_) if passenger.dropoff_seconds < passenger.pickup_seconds => {
                    Some("dropoff_before_pickup")
                }
                Some(_) => None,
            };
            match reason {
                Some(reason) => diagnostics.add("passengers", row, reason, &id),
                None => {
                    associations.insert(
                        id,
                        Association {
                            row,
                            vehicle: passenger.vehicle_id,
                            pickup: passenger.pickup_seconds,
                            dropoff: passenger.dropoff_seconds,
                        },
                    );
                }
            }
        }
    }

    let mut vehicles: BTreeMap<String, Vehicle> = BTreeMap::new();
    if let Some(path) = &inputs.fleet {
        for (row, vehicle) in read::<FleetRow>(&resolve(path), "fleet")? {
            for value in [vehicle.service_start_seconds, vehicle.service_end_seconds]
                .into_iter()
                .flatten()
            {
                finite(value, "fleet", row)?;
            }
            let bounds = match (vehicle.service_start_seconds, vehicle.service_end_seconds) {
                (Some(start), Some(end)) if end > start => Some((start, end)),
                (Some(_), Some(_)) => {
                    diagnostics.add("fleet", row, "invalid_service_window", &vehicle.vehicle_id);
                    None
                }
                _ => None,
            };
            let entry = Vehicle {
                capacity: vehicle.capacity,
                window: bounds.map(|(start, end)| end - start),
                bounds,
                ..Vehicle::default()
            };
            if vehicles.insert(vehicle.vehicle_id.clone(), entry).is_some() {
                return Err(AnalysisError::new(format!(
                    "fleet row {row} repeats vehicle_id {}",
                    vehicle.vehicle_id
                )));
            }
        }
    }
    // With a fleet supplied, any other vehicle is reported once at its first record, but still
    // listed so its served requests and distance are not dropped.
    let fleet_ids: BTreeSet<String> = vehicles.keys().cloned().collect();
    if supplied.fleet {
        let mut unknown: BTreeMap<&str, usize> = BTreeMap::new();
        for association in associations.values() {
            if !fleet_ids.contains(&association.vehicle) {
                let first = unknown
                    .entry(association.vehicle.as_str())
                    .or_insert(association.row);
                *first = (*first).min(association.row);
            }
        }
        for (vehicle, row) in unknown {
            diagnostics.add("passengers", row, "unknown_vehicle", vehicle);
        }
    }
    let mut unknown_in_schedule = BTreeSet::new();
    // Passengers by vehicle, so a drive task only looks at its own vehicle's riders.
    let mut riders: BTreeMap<&str, Vec<(f64, f64, u64)>> = BTreeMap::new();
    for (id, association) in &associations {
        riders
            .entry(association.vehicle.as_str())
            .or_default()
            .push((association.pickup, association.dropoff, requests[id].party));
    }
    for association in associations.values() {
        vehicles
            .entry(association.vehicle.clone())
            .or_default()
            .served += 1;
    }

    let mut load_meters: BTreeMap<u64, f64> = BTreeMap::new();
    if let Some(path) = &inputs.schedule {
        for (row, task) in read::<TaskRow>(&resolve(path), "schedule")? {
            let (start, end) = (
                finite(task.start_seconds, "schedule", row)?,
                finite(task.end_seconds, "schedule", row)?,
            );
            if !matches!(task.task_type.as_str(), "drive" | "stop" | "stay") {
                return Err(AnalysisError::new(format!(
                    "schedule row {row} has unknown task_type {:?}; expected drive, stop or stay",
                    task.task_type
                )));
            }
            if end < start {
                diagnostics.add("schedule", row, "end_before_start", &task.vehicle_id);
                continue;
            }
            if supplied.fleet
                && !fleet_ids.contains(&task.vehicle_id)
                && unknown_in_schedule.insert(task.vehicle_id.clone())
            {
                diagnostics.add("schedule", row, "unknown_vehicle", &task.vehicle_id);
            }
            let vehicle = vehicles.entry(task.vehicle_id.clone()).or_default();
            let busy = if task.task_type == "stay" {
                0.0
            } else {
                end - start
            };
            vehicle.busy += busy;
            if let Some((window_start, window_end)) = vehicle.bounds
                && busy > 0.0
            {
                vehicle.windowed_busy += (end.min(window_end) - start.max(window_start)).max(0.0);
            }
            if task.task_type != "drive" {
                continue;
            }
            let Some(distance) = task.distance_meters.filter(|d| d.is_finite() && *d >= 0.0) else {
                diagnostics.add(
                    "schedule",
                    row,
                    "drive_distance_unavailable",
                    &task.vehicle_id,
                );
                continue;
            };
            // Pickups and drop-offs happen at stops, so a passenger rides a whole drive task or
            // none of it. The midpoint decides, so a boarding time that lands slightly inside
            // a task does not turn the whole task into empty relocation.
            let midpoint = (start + end) / 2.0;
            let load: u64 = riders
                .get(task.vehicle_id.as_str())
                .into_iter()
                .flatten()
                .filter(|(pickup, dropoff, _)| *pickup <= midpoint && *dropoff >= midpoint)
                .map(|(.., party)| party)
                .sum();
            vehicle.driven += distance;
            if load > 0 {
                vehicle.occupied += distance;
                vehicle.passenger_meters += load as f64 * distance;
            }
            if let Some(capacity) = vehicle.capacity {
                vehicle.capacity_meters += capacity as f64 * distance;
                vehicle.capacity_passenger_meters += load as f64 * distance;
                if load > capacity {
                    vehicle.exceeded += 1;
                }
            }
            // Without rider records every task would read as empty, so no load is reported.
            if supplied.passengers {
                *load_meters.entry(load).or_default() += distance;
            }
        }
    }
    write_requests_and_summary(report_dir, &requests, &associations, supplied, inputs)?;
    write_vehicles(report_dir, &vehicles, supplied)?;
    write_occupancy(report_dir, &load_meters)?;
    write_constraints(report_dir, inputs, &vehicles)?;
    write_availability(
        report_dir,
        inputs,
        supplied,
        vehicles.values().any(|v| v.window.is_some()),
        vehicles.values().any(|v| v.capacity.is_some()),
    )?;
    let mut writer = table_writer(report_dir, "service_diagnostics.csv")?;
    writeln!(writer, "{DIAGNOSTICS_HEADER}").map_err(io_error)?;
    for [source, row, reason, detail] in &diagnostics.0 {
        writeln!(
            writer,
            "{},{row},{},{}",
            csv(source),
            csv(reason),
            csv(detail)
        )
        .map_err(io_error)?;
    }
    writer.flush().map_err(io_error)
}

/// One computed request, the unit both the per-request table and the summaries are built from.
struct Outcome<'a> {
    id: &'a str,
    request: &'a Request,
    /// `served`, `rejected`, `unserved`, or `unknown` when no passenger records were supplied.
    status: &'static str,
    association: Option<&'a Association>,
    wait: Option<f64>,
    ride: Option<f64>,
    detour: Option<f64>,
    wait_exceeded: Option<bool>,
}

fn write_requests_and_summary(
    report_dir: &Path,
    requests: &BTreeMap<String, Request>,
    associations: &BTreeMap<String, Association>,
    supplied: Supplied,
    inputs: &ServiceInputs,
) -> Result<(), AnalysisError> {
    let outcomes: Vec<Outcome> = requests
        .iter()
        .map(|(id, request)| {
            let association = associations.get(id);
            let status = match (request.rejected, association, supplied.passengers) {
                (true, ..) => "rejected",
                (_, Some(_), _) => "served",
                (_, None, true) => "unserved",
                (_, None, false) => "unknown",
            };
            let wait = association.map(|a| a.pickup - request.submission);
            let ride = association.map(|a| a.dropoff - a.pickup);
            Outcome {
                id,
                request,
                status,
                association,
                wait,
                ride,
                detour: ride.zip(request.direct).map(|(ride, direct)| ride / direct),
                wait_exceeded: wait
                    .zip(inputs.max_wait_seconds)
                    .map(|(wait, limit)| wait > limit),
            }
        })
        .collect();

    let mut table = table_writer(report_dir, "service_requests.csv")?;
    writeln!(table, "{REQUESTS_HEADER}").map_err(io_error)?;
    for o in &outcomes {
        writeln!(
            table,
            "{},{},{},{},{},{:.6},{},{},{},{},{},{},{},{}",
            csv(o.id),
            csv(&o.request.person_id),
            csv(&label(o.request.group.as_deref())),
            o.status,
            o.association.map_or(String::new(), |a| csv(&a.vehicle)),
            o.request.submission,
            number_opt(o.association.map(|a| a.pickup)),
            number_opt(o.association.map(|a| a.dropoff)),
            number_opt(o.wait),
            number_opt(o.ride),
            number_opt(o.request.direct),
            number_opt(o.detour),
            o.request.area,
            o.wait_exceeded.map_or(String::new(), |e| e.to_string()),
        )
        .map_err(io_error)?;
    }
    table.flush().map_err(io_error)?;

    let mut summary = table_writer(report_dir, "service_summary.csv")?;
    writeln!(summary, "{SUMMARY_HEADER}").map_err(io_error)?;
    summarize(
        &mut summary,
        "total",
        "",
        &outcomes.iter().collect::<Vec<_>>(),
        inputs,
    )?;
    // Group rows are only meaningful when the requests carry labels.
    let groups: BTreeSet<String> = outcomes
        .iter()
        .map(|o| label(o.request.group.as_deref()))
        .collect();
    if outcomes.iter().any(|o| o.request.group.is_some()) {
        for group in &groups {
            let members: Vec<_> = outcomes
                .iter()
                .filter(|o| label(o.request.group.as_deref()) == *group)
                .collect();
            summarize(&mut summary, "group", group, &members, inputs)?;
        }
    }
    summary.flush().map_err(io_error)
}

fn summarize(
    out: &mut impl Write,
    scope: &str,
    group: &str,
    members: &[&Outcome],
    inputs: &ServiceInputs,
) -> Result<(), AnalysisError> {
    let count = |status: &str| members.iter().filter(|o| o.status == status).count();
    let share = |n: usize| (!members.is_empty()).then(|| n as f64 / members.len() as f64);
    let known = !members.iter().any(|o| o.status == "unknown");
    let sorted = |values: Vec<f64>| {
        let mut values = values;
        values.sort_by(f64::total_cmp);
        values
    };
    let waits = sorted(members.iter().filter_map(|o| o.wait).collect());
    let detours = sorted(members.iter().filter_map(|o| o.detour).collect());
    let area = |name: &str| members.iter().filter(|o| o.request.area == name).count();
    let area_configured = inputs.service_area.is_some();
    let optional = |value: Option<usize>| value.map_or(String::new(), |v| v.to_string());
    writeln!(
        out,
        "{scope},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        csv(group),
        members.len(),
        optional(known.then(|| count("served"))),
        count("rejected"),
        optional(known.then(|| count("unserved"))),
        number_opt(known.then(|| share(count("served"))).flatten()),
        number_opt(share(count("rejected"))),
        optional(known.then(|| {
            members
                .iter()
                .filter(|o| o.status == "served")
                .map(|o| o.request.party as usize)
                .sum()
        })),
        number_opt(mean(&waits)),
        number_opt(std_dev(&waits)),
        number_opt(quantile(&waits, 0.5)),
        number_opt(quantile(&waits, 0.9)),
        number_opt(mean(&detours)),
        number_opt(std_dev(&detours)),
        number_opt(quantile(&detours, 0.5)),
        number_opt(quantile(&detours, 0.9)),
        optional(inputs.max_wait_seconds.map(|_| {
            members
                .iter()
                .filter(|o| o.wait_exceeded == Some(true))
                .count()
        })),
        optional(area_configured.then(|| area("inside"))),
        optional(area_configured.then(|| area("outside"))),
        optional(area_configured.then(|| area("unknown"))),
        number_opt(area_configured.then(|| share(area("inside"))).flatten()),
    )
    .map_err(io_error)
}

fn write_vehicles(
    report_dir: &Path,
    vehicles: &BTreeMap<String, Vehicle>,
    supplied: Supplied,
) -> Result<(), AnalysisError> {
    let mut table = table_writer(report_dir, "service_vehicles.csv")?;
    writeln!(table, "{VEHICLES_HEADER}").map_err(io_error)?;
    let mut fleet = Vehicle::default();
    for vehicle in vehicles.values() {
        fleet.add(vehicle);
    }
    // Totals first, then one row per vehicle.
    vehicle_row(&mut table, "fleet", "", &fleet, supplied)?;
    for (id, vehicle) in vehicles {
        vehicle_row(&mut table, "vehicle", id, vehicle, supplied)?;
    }
    table.flush().map_err(io_error)
}

fn vehicle_row(
    out: &mut impl Write,
    scope: &str,
    id: &str,
    v: &Vehicle,
    supplied: Supplied,
) -> Result<(), AnalysisError> {
    let schedule = supplied.schedule;
    let occupancy = schedule && supplied.passengers;
    let ratio =
        |numerator: f64, denominator: f64| (denominator > 0.0).then(|| numerator / denominator);
    let window = v.window.filter(|window| supplied.fleet && *window > 0.0);
    writeln!(
        out,
        "{scope},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        csv(id),
        v.capacity.map_or(String::new(), |c| c.to_string()),
        number_opt(window),
        number_opt(schedule.then_some(v.busy)),
        number_opt(
            schedule
                .then(|| ratio(v.windowed_busy, v.window.unwrap_or(0.0)))
                .flatten()
        ),
        number_opt(schedule.then_some(v.driven)),
        number_opt(occupancy.then_some(v.occupied)),
        number_opt(occupancy.then_some(v.driven - v.occupied)),
        number_opt(
            occupancy
                .then(|| ratio(v.driven - v.occupied, v.driven))
                .flatten()
        ),
        number_opt(occupancy.then_some(v.passenger_meters)),
        number_opt(
            occupancy
                .then(|| ratio(v.passenger_meters, v.driven))
                .flatten()
        ),
        number_opt(
            occupancy
                .then(|| ratio(v.capacity_passenger_meters, v.capacity_meters))
                .flatten()
        ),
        if occupancy {
            v.exceeded.to_string()
        } else {
            String::new()
        },
        if supplied.passengers {
            v.served.to_string()
        } else {
            String::new()
        },
    )
    .map_err(io_error)
}

/// Driven distance by number of passengers on board; load zero is empty relocation.
fn write_occupancy(
    report_dir: &Path,
    load_meters: &BTreeMap<u64, f64>,
) -> Result<(), AnalysisError> {
    let mut table = table_writer(report_dir, "service_occupancy.csv")?;
    writeln!(table, "{OCCUPANCY_HEADER}").map_err(io_error)?;
    let total: f64 = load_meters.values().sum();
    for (load, meters) in load_meters {
        writeln!(
            table,
            "{load},{meters:.6},{}",
            number_opt((total > 0.0).then(|| meters / total))
        )
        .map_err(io_error)?;
    }
    table.flush().map_err(io_error)
}

fn write_constraints(
    report_dir: &Path,
    inputs: &ServiceInputs,
    vehicles: &BTreeMap<String, Vehicle>,
) -> Result<(), AnalysisError> {
    let capacities: Vec<u64> = vehicles.values().filter_map(|v| v.capacity).collect();
    let area = inputs.service_area.as_ref().map(|area| {
        area.iter()
            .map(|[x, y]| format!("{x} {y}"))
            .collect::<Vec<_>>()
            .join(";")
    });
    let mut table = table_writer(report_dir, "service_constraints.csv")?;
    writeln!(table, "{CONSTRAINTS_HEADER}").map_err(io_error)?;
    for (name, value) in [
        ("service_area_polygon", area),
        (
            "max_wait_seconds",
            inputs.max_wait_seconds.map(|v| v.to_string()),
        ),
        ("capacity_min", capacities.iter().min().map(u64::to_string)),
        ("capacity_max", capacities.iter().max().map(u64::to_string)),
        (
            "capacity_total",
            (!capacities.is_empty()).then(|| capacities.iter().sum::<u64>().to_string()),
        ),
    ] {
        writeln!(table, "{name},{}", csv(&value.unwrap_or_default())).map_err(io_error)?;
    }
    table.flush().map_err(io_error)
}

fn write_availability(
    report_dir: &Path,
    inputs: &ServiceInputs,
    supplied: Supplied,
    has_window: bool,
    has_capacity: bool,
) -> Result<(), AnalysisError> {
    let mut table = table_writer(report_dir, "service_availability.csv")?;
    writeln!(table, "{AVAILABILITY_HEADER}").map_err(io_error)?;
    // Each metric group lists the inputs it needs; a missing one makes the group unavailable.
    for (component, needs) in [
        ("requests", vec![]),
        (
            "request_outcomes",
            vec![("passengers", supplied.passengers)],
        ),
        ("wait_detour", vec![("passengers", supplied.passengers)]),
        (
            "occupancy_distance",
            vec![
                ("passengers", supplied.passengers),
                ("schedule", supplied.schedule),
            ],
        ),
        (
            "utilization",
            vec![
                ("fleet service windows", has_window),
                ("schedule", supplied.schedule),
            ],
        ),
        (
            "load_factor",
            vec![
                ("fleet capacities", has_capacity),
                ("passengers", supplied.passengers),
                ("schedule", supplied.schedule),
            ],
        ),
        (
            "coverage",
            vec![("service_area", inputs.service_area.is_some())],
        ),
    ] {
        let missing: Vec<_> = needs
            .iter()
            .filter(|(_, ok)| !ok)
            .map(|(n, _)| *n)
            .collect();
        if missing.is_empty() {
            writeln!(table, "{component},available,").map_err(io_error)?;
        } else {
            writeln!(
                table,
                "{component},unavailable,{}",
                csv(&format!("missing input: {}", missing.join(", ")))
            )
            .map_err(io_error)?;
        }
    }
    table.flush().map_err(io_error)
}

/// Header-only tables, used when no service records are configured or they cannot be read.
pub(super) fn write_empty(report_dir: &Path) -> Result<(), AnalysisError> {
    for (name, header) in TABLES {
        std::fs::write(report_dir.join(name), format!("{header}\n")).map_err(io_error)?;
    }
    Ok(())
}
