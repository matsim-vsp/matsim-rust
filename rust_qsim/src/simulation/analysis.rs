//! Final-iteration link coverage reporting.

use crate::simulation::config::{Analysis, CompressionType};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, PersonArrivalEvent, PersonDepartureEvent,
    PersonStuckEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct AnalysisError(String);

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AnalysisError {}

/// Leg rows embedded in the local report before it defers to the full `legs.csv`.
const LEGS_PREVIEW_ROWS: usize = 200;

#[derive(Serialize)]
struct Metric<'a> {
    name: &'a str,
    unit: &'a str,
    aggregation_key: &'a str,
}

#[derive(Serialize)]
struct Manifest<'a> {
    status: &'a str,
    iteration: u32,
    interval_seconds: u32,
    partitions: Vec<u32>,
    input_format: &'a str,
    eligible_links: usize,
    random_seed: u64,
    network_input: Option<String>,
    population_input: Option<String>,
    software_version: &'static str,
}

#[derive(Debug, Serialize)]
pub struct PersonExpectedTravel {
    person_id: String,
    legs: Vec<ExpectedLeg>,
}

#[derive(Debug, Serialize)]
struct ExpectedLeg {
    leg_index: usize,
    mode: String,
    departure_seconds: Option<f64>,
    expected_travel_seconds: Option<f64>,
}

#[derive(Serialize)]
struct VehiclePce {
    vehicle_id: String,
    vehicle_type_id: String,
    pce: f64,
}

struct ObservedLeg {
    person_id: String,
    leg_index: usize,
    mode: String,
    expected_plan_leg: bool,
    departure_seconds: f64,
    departure_hour: u64,
    completion: LegCompletion,
}

#[derive(Clone, Copy)]
enum LegCompletion {
    Pending,
    Completed { arrival_seconds: f64 },
    MissingArrival,
    Stuck,
}

impl LegCompletion {
    fn status(&self) -> &'static str {
        match self {
            Self::Pending => "incomplete",
            Self::Completed { .. } => "completed",
            Self::MissingArrival => "missing_arrival",
            Self::Stuck => "stuck",
        }
    }

    fn arrival_seconds(&self) -> Option<f64> {
        match self {
            Self::Completed { arrival_seconds } => Some(*arrival_seconds),
            _ => None,
        }
    }

    fn duration(&self, departure_seconds: f64) -> Option<f64> {
        self.arrival_seconds()
            .map(|arrival_seconds| arrival_seconds - departure_seconds)
    }
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct ModeHour {
    hour_start_seconds: u64,
    mode: String,
}

#[derive(Default)]
struct HourlyLegs {
    departures: u64,
    persons: BTreeSet<String>,
    duration_sum: f64,
    completed: u64,
}

#[derive(Default)]
struct PersonActivity {
    departures: usize,
    expected_departures: usize,
    completed_legs: usize,
}

#[derive(Serialize)]
struct ModuleStatus<'a> {
    module: &'a str,
    status: &'a str,
    reason: Option<&'a str>,
}

pub struct AnalysisRunMetadata<'a> {
    pub random_seed: u64,
    pub network_input: Option<&'a Path>,
    pub population_input: Option<&'a Path>,
    pub vehicles_input: Option<&'a Path>,
    pub expected_travel: &'a [PersonExpectedTravel],
    pub garage: &'a Garage,
}

/// Capture compact plan expectations immediately before the final iteration's mobsim.
pub fn capture_expected_travel(population: &Population) -> Vec<PersonExpectedTravel> {
    let mut persons: Vec<_> = population.persons.values().collect();
    persons.sort_by(|a, b| a.id().external().cmp(b.id().external()));
    persons
        .into_iter()
        .filter_map(|person| {
            let plan = person.selected_plan()?;
            let legs: Vec<_> = plan
                .elements
                .iter()
                .enumerate()
                .filter_map(|(element_index, element)| {
                    let InternalPlanElement::Leg(leg) = element else {
                        return None;
                    };
                    let expected = leg.trav_time.or_else(|| {
                        leg.route
                            .as_ref()
                            .and_then(|route| route.as_generic().trav_time())
                    });
                    Some(ExpectedLeg {
                        leg_index: element_index,
                        mode: leg.mode.external().to_owned(),
                        departure_seconds: leg.dep_time.map(|time| time.as_nanos() as f64 / 1e9),
                        expected_travel_seconds: expected.map(|time| time.as_secs_f64()),
                    })
                })
                .collect();
            Some(PersonExpectedTravel {
                person_id: person.id().external().to_owned(),
                legs,
            })
        })
        .collect()
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct LinkHour {
    hour_start_seconds: u64,
    link_id: String,
}

#[derive(Clone, Copy, Default)]
struct LinkVolumes {
    entries: u64,
    exits: u64,
}

type LinkVolumesByHour = BTreeMap<LinkHour, LinkVolumes>;

/// Replay every final-iteration partition and publish deterministic coverage tables and HTML.
pub fn analyze_final_iteration(
    output_dir: &Path,
    iteration: u32,
    partitions: u32,
    compression: CompressionType,
    simulation_end_time: u32,
    run_metadata: &AnalysisRunMetadata<'_>,
    network: &Network,
    settings: &Analysis,
) -> Result<PathBuf, AnalysisError> {
    if !settings.enabled {
        return Err(AnalysisError("analysis is disabled".into()));
    }
    if settings.interval_seconds == 0 {
        return Err(AnalysisError(
            "analysis.interval_seconds must be greater than zero".into(),
        ));
    }
    let events_dir = output_dir
        .join("ITERS")
        .join(format!("it.{iteration}"))
        .join("events");
    let ext = compression.extension();
    let files: Vec<_> = (0..partitions)
        .map(|rank| events_dir.join(format!("events.{rank}.{ext}")))
        .collect();
    for path in &files {
        if !path.is_file() {
            return Err(AnalysisError(format!(
                "missing final-iteration event partition: {}",
                path.display()
            )));
        }
    }

    let links = network.links();
    let mut ordered_links: Vec<_> = links.into_iter().collect();
    ordered_links.sort_by(|a, b| a.id.external().cmp(b.id.external()));
    let ids: BTreeSet<_> = ordered_links
        .iter()
        .map(|link| link.id.external().to_owned())
        .collect();
    let mut readers: Vec<_> = files
        .iter()
        .map(|path| match compression {
            CompressionType::Proto => PartitionReader::Proto {
                reader: ProtoEventsReader::from_file(path),
                pending: None,
            },
            CompressionType::None | CompressionType::Gz | CompressionType::Zst => {
                PartitionReader::Xml(XmlEventsReader::new(path))
            }
        })
        .collect();
    let mut heads = readers
        .iter_mut()
        .map(PartitionReader::next_event)
        .collect::<Result<Vec<_>, _>>()?;
    let mut counts = LinkVolumesByHour::new();
    let expected: BTreeMap<_, _> = run_metadata
        .expected_travel
        .iter()
        .map(|person| {
            (
                person.person_id.clone(),
                person
                    .legs
                    .iter()
                    .map(|leg| (leg.leg_index, leg.mode.clone()))
                    .collect::<Vec<_>>(),
            )
        })
        .collect();
    let mut agent_travel = AgentTravelAccumulator::new(settings.interval_seconds, expected);
    loop {
        let Some(time) = heads
            .iter()
            .filter_map(|event| event.as_ref().map(|(time, _)| *time))
            .min()
        else {
            break;
        };
        // Link counts commute; process all same-time agent events together so that pairing an
        // arrival with a same-time departure does not depend on which partition delivered
        // either event first. Within a batch, an arrival still matches the person's open leg
        // before any leg created in the same batch.
        let mut simultaneous_events = Vec::new();
        for rank in 0..heads.len() {
            while heads[rank]
                .as_ref()
                .is_some_and(|(event_time, _)| *event_time == time)
            {
                let (_, event) = heads[rank].take().expect("selected reader head exists");
                accumulate(
                    event.as_ref(),
                    time,
                    settings.interval_seconds,
                    &ids,
                    &mut counts,
                );
                simultaneous_events.push(event);
                heads[rank] = readers[rank].next_event()?;
            }
        }
        agent_travel.process_timestamp(&simultaneous_events, time);
    }

    let staging = output_dir.join(".analysis-staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_error)?;
    }
    fs::create_dir_all(&staging).map_err(io_error)?;
    write_tables(
        &staging,
        &ordered_links,
        &counts,
        &agent_travel.observed_legs,
        &agent_travel.expected,
        &agent_travel.stuck_people,
        settings.interval_seconds,
        simulation_end_time,
    )?;
    let manifest = Manifest {
        status: "complete",
        iteration,
        interval_seconds: settings.interval_seconds,
        partitions: (0..partitions).collect(),
        input_format: ext,
        eligible_links: ordered_links.len(),
        random_seed: run_metadata.random_seed,
        network_input: run_metadata
            .network_input
            .map(|path| path.display().to_string()),
        population_input: run_metadata
            .population_input
            .map(|path| path.display().to_string()),
        software_version: env!("CARGO_PKG_VERSION"),
    };
    fs::write(
        staging.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).map_err(|e| AnalysisError(e.to_string()))?,
    )
    .map_err(io_error)?;
    let mut vehicles: Vec<_> = run_metadata
        .garage
        .vehicles
        .values()
        .map(|vehicle| VehiclePce {
            vehicle_id: vehicle.id.external().to_owned(),
            vehicle_type_id: vehicle.vehicle_type.external().to_owned(),
            pce: vehicle.pce,
        })
        .collect();
    vehicles.sort_by(|a, b| a.vehicle_id.cmp(&b.vehicle_id));
    let mut vehicle_types: Vec<_> = run_metadata
        .garage
        .vehicle_types
        .values()
        .map(|vehicle_type| (vehicle_type.id.external().to_owned(), vehicle_type.pce))
        .collect();
    vehicle_types.sort_by(|a, b| a.0.cmp(&b.0));
    fs::write(
        staging.join("run_metadata.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "expected_travel": run_metadata.expected_travel,
            "vehicles": vehicles,
            "vehicle_types": vehicle_types.iter().map(|(id, pce)| serde_json::json!({
                "vehicle_type_id": id,
                "pce": pce,
            })).collect::<Vec<_>>(),
            "vehicles_input": run_metadata.vehicles_input.map(|path| path.display().to_string()),
        }))
        .map_err(|e| AnalysisError(e.to_string()))?,
    )
    .map_err(io_error)?;
    let metrics = [
        Metric {
            name: "link_entry_vehicles",
            unit: "vehicles",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "link_exit_vehicles",
            unit: "vehicles",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "used_links",
            unit: "links",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "unused_links",
            unit: "links",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "used_link_percent",
            unit: "percent",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "leg_departures",
            unit: "legs",
            aggregation_key: "departure_hour_seconds,mode",
        },
        Metric {
            name: "departing_persons",
            unit: "persons",
            aggregation_key: "departure_hour_seconds,mode",
        },
        Metric {
            name: "leg_duration_mean",
            unit: "seconds",
            aggregation_key: "departure_hour_seconds,mode",
        },
        Metric {
            name: "person_completed_leg_duration_sum",
            unit: "seconds",
            aggregation_key: "person_id",
        },
        Metric {
            name: "daily_mean_completed_travel_burden",
            unit: "seconds",
            aggregation_key: "cohort",
        },
        Metric {
            name: "person_completed_leg_duration_mean",
            unit: "seconds",
            aggregation_key: "person_id",
        },
        Metric {
            name: "leg_completion_status",
            unit: "category",
            aggregation_key: "person_id,leg_index",
        },
    ];
    fs::write(
        staging.join("metric_catalog.json"),
        serde_json::to_vec_pretty(&metrics).map_err(|e| AnalysisError(e.to_string()))?,
    )
    .map_err(io_error)?;
    let statuses = [
        ModuleStatus {
            module: "link_coverage",
            status: "complete",
            reason: None,
        },
        ModuleStatus {
            module: "link_speed",
            status: "unavailable",
            reason: Some("Traversal timing metrics are not implemented yet"),
        },
        ModuleStatus {
            module: "agent_travel",
            status: "complete",
            reason: None,
        },
        ModuleStatus {
            module: "validation",
            status: "unavailable",
            reason: Some("No observed validation datasets are configured"),
        },
        ModuleStatus {
            module: "cross_run_comparison",
            status: "unavailable",
            reason: Some("No comparison runs are configured"),
        },
        ModuleStatus {
            module: "transit_and_research",
            status: "unavailable",
            reason: Some("Optional module inputs are not configured"),
        },
    ];
    fs::write(
        staging.join("module_status.json"),
        serde_json::to_vec_pretty(&statuses).map_err(|e| AnalysisError(e.to_string()))?,
    )
    .map_err(io_error)?;
    write_report(&staging, iteration, ordered_links.len())?;
    let published = output_dir.join("analysis");
    let backup = output_dir.join(".analysis-backup");
    if backup.exists() {
        if published.exists() {
            fs::remove_dir_all(&backup).map_err(io_error)?;
        } else {
            fs::rename(&backup, &published).map_err(io_error)?;
        }
    }
    let had_published = published.exists();
    if had_published {
        fs::rename(&published, &backup).map_err(io_error)?;
    }
    if let Err(error) = fs::rename(&staging, &published) {
        if had_published {
            let _ = fs::rename(&backup, &published);
        }
        return Err(io_error(error));
    }
    if had_published {
        fs::remove_dir_all(&backup).map_err(io_error)?;
    }
    Ok(published.join("index.html"))
}

enum PartitionReader {
    Xml(XmlEventsReader),
    Proto {
        reader: ProtoEventsReader<File>,
        pending: Option<(
            SimTime,
            std::vec::IntoIter<crate::generated::events::GenericEvent>,
        )>,
    },
}

impl PartitionReader {
    fn next_event(&mut self) -> Result<Option<(SimTime, Box<dyn EventTrait>)>, AnalysisError> {
        match self {
            Self::Xml(reader) => reader
                .try_read_next()
                .map_err(|error| AnalysisError(format!("failed to parse event XML: {error}"))),
            Self::Proto { reader, pending } => loop {
                if let Some((time, events)) = pending.as_mut()
                    && let Some(event) = events.next()
                {
                    return Ok(Some((*time, event_from_proto(*time, &event))));
                }
                let Some((time, events)) = reader.try_next().map_err(|error| {
                    AnalysisError(format!("failed to parse protobuf events: {error}"))
                })?
                else {
                    return Ok(None);
                };
                *pending = Some((time, events.into_iter()));
            },
        }
    }
}

fn accumulate(
    event: &dyn EventTrait,
    time: SimTime,
    interval: u32,
    ids: &BTreeSet<String>,
    counts: &mut LinkVolumesByHour,
) {
    let (link, entry) = if let Some(event) = event.as_any().downcast_ref::<LinkEnterEvent>() {
        (&event.link, true)
    } else if let Some(event) = event.as_any().downcast_ref::<LinkLeaveEvent>() {
        (&event.link, false)
    } else if let Some(event) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
        (&event.link, true)
    } else if let Some(event) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
        (&event.link, false)
    } else {
        return;
    };
    let id = link.external();
    if !ids.contains(id) {
        return;
    }
    let hour = time.as_nanos() / 1_000_000_000 / u64::from(interval) * u64::from(interval);
    let count = counts
        .entry(LinkHour {
            hour_start_seconds: hour,
            link_id: id.to_owned(),
        })
        .or_default();
    if entry {
        count.entries += 1;
    } else {
        count.exits += 1;
    }
}

/// Reconstructs per-leg completion from replayed person events.
///
/// The rules are a deliberate deviation from MATSim Java, which reads leg status
/// off its own leg objects instead of inferring it from the event stream. QSim's
/// event files carry a person, a mode and a time, so leg status has to be inferred:
///
/// - A departure consumes the next planned leg whose mode matches, starting from
///   the person's plan offset. Departures that match no remaining planned leg are
///   unplanned: they keep indices after the plan and never advance the offset.
/// - An arrival completes the person's open leg when the mode matches it, first the
///   leg opened by an earlier timestamp, then any leg opened in the same batch.
///   Same-timestamp events are processed as one batch, so pairing does not depend
///   on which partition delivered an event first.
/// - A departure while a leg is still open closes that leg as `MissingArrival`;
///   among legs opened in the same batch only the last one can stay open.
/// - `PersonStuckEvent` marks the person's open leg `Stuck` and flags the day as
///   stuck. It names no leg, so a planned leg that was never departed stays
///   `not_departed`; only `person_daily.csv` reports the stuck day.
/// - A leg still open after the last event is reported as `incomplete`, never as a
///   zero-duration leg: missing arrivals are excluded from every duration mean.
struct AgentTravelAccumulator {
    interval: u32,
    expected: BTreeMap<String, Vec<(usize, String)>>,
    expected_offsets: BTreeMap<String, usize>,
    unplanned_offsets: BTreeMap<String, usize>,
    pending: BTreeMap<String, usize>,
    observed_legs: Vec<ObservedLeg>,
    stuck_people: BTreeSet<String>,
}

impl AgentTravelAccumulator {
    fn new(interval: u32, expected: BTreeMap<String, Vec<(usize, String)>>) -> Self {
        Self {
            interval,
            expected,
            expected_offsets: BTreeMap::new(),
            unplanned_offsets: BTreeMap::new(),
            pending: BTreeMap::new(),
            observed_legs: Vec::new(),
            stuck_people: BTreeSet::new(),
        }
    }

    fn process_timestamp(&mut self, events: &[Box<dyn EventTrait>], time: SimTime) {
        let seconds = time.as_nanos() as f64 / 1_000_000_000.0;
        let arrivals: Vec<_> = events
            .iter()
            .filter_map(|event| {
                event
                    .as_any()
                    .downcast_ref::<PersonArrivalEvent>()
                    .map(|event| {
                        (
                            event.person.external().to_owned(),
                            event.leg_mode.external().to_owned(),
                        )
                    })
            })
            .collect();

        let mut matched_arrivals = BTreeSet::new();
        for (arrival_index, (person, mode)) in arrivals.iter().enumerate() {
            if let Some(leg_id) = self.pending.get(person).copied()
                && self.observed_legs[leg_id].mode == *mode
            {
                self.complete_leg(person, leg_id, seconds);
                matched_arrivals.insert(arrival_index);
            }
        }

        let mut departures = BTreeMap::<String, Vec<String>>::new();
        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
                departures
                    .entry(event.person.external().to_owned())
                    .or_default()
                    .push(event.leg_mode.external().to_owned());
            }
        }
        let mut created_legs = BTreeMap::<String, Vec<usize>>::new();
        for (person, mut modes) in departures {
            if let Some(previous_leg) = self.pending.remove(&person) {
                self.observed_legs[previous_leg].completion = LegCompletion::MissingArrival;
            }
            let offset = self
                .expected_offsets
                .get(&person)
                .copied()
                .unwrap_or_default();
            let expected_legs = self.expected.get(&person).map_or(&[][..], Vec::as_slice);
            let mut ordered_modes = Vec::with_capacity(modes.len());
            let mut next_offset = offset;
            for (expected_offset, (leg_index, expected_mode)) in
                expected_legs.iter().enumerate().skip(offset)
            {
                if let Some(mode_index) = modes.iter().position(|mode| mode == expected_mode) {
                    ordered_modes.push((modes.remove(mode_index), *leg_index, true));
                    next_offset = expected_offset + 1;
                }
            }
            modes.sort();
            // Unplanned departures keep indices after the plan so that
            // `person_id,leg_index` stays unique in legs.csv across batches.
            let planned_end = expected_legs
                .iter()
                .map(|(leg_index, _)| leg_index.saturating_add(1))
                .max()
                .unwrap_or_default();
            let unplanned_count = modes.len();
            let unplanned_offset = self
                .unplanned_offsets
                .entry(person.clone())
                .or_insert(planned_end);
            let unplanned_start = *unplanned_offset;
            *unplanned_offset = unplanned_start.saturating_add(unplanned_count);
            ordered_modes.extend(
                modes
                    .into_iter()
                    .enumerate()
                    .map(|(index, mode)| (mode, unplanned_start.saturating_add(index), false)),
            );
            self.expected_offsets.insert(person.clone(), next_offset);
            for (mode, leg_index, expected_plan_leg) in ordered_modes {
                let leg_id = self.observed_legs.len();
                self.observed_legs.push(ObservedLeg {
                    person_id: person.clone(),
                    leg_index,
                    mode,
                    expected_plan_leg,
                    departure_seconds: seconds,
                    departure_hour: time.as_nanos() / 1_000_000_000 / u64::from(self.interval)
                        * u64::from(self.interval),
                    completion: LegCompletion::Pending,
                });
                created_legs.entry(person.clone()).or_default().push(leg_id);
            }
        }

        for (arrival_index, (person, mode)) in arrivals.iter().enumerate() {
            if matched_arrivals.contains(&arrival_index) {
                continue;
            }
            if let Some(leg_ids) = created_legs.get(person)
                && let Some(leg_id) = leg_ids.iter().find(|&&leg_id| {
                    self.observed_legs[leg_id].mode == *mode
                        && matches!(
                            self.observed_legs[leg_id].completion,
                            LegCompletion::Pending
                        )
                })
            {
                self.observed_legs[*leg_id].completion = LegCompletion::Completed {
                    arrival_seconds: seconds,
                };
            }
        }

        for (person, leg_ids) in created_legs {
            let mut incomplete: Vec<_> = leg_ids
                .into_iter()
                .filter(|&leg_id| {
                    matches!(
                        self.observed_legs[leg_id].completion,
                        LegCompletion::Pending
                    )
                })
                .collect();
            if let Some(last_leg) = incomplete.pop() {
                for leg_id in incomplete {
                    self.observed_legs[leg_id].completion = LegCompletion::MissingArrival;
                }
                self.pending.insert(person, last_leg);
            }
        }

        for event in events {
            if let Some(event) = event.as_any().downcast_ref::<PersonStuckEvent>() {
                let person = event.person.external().to_owned();
                self.stuck_people.insert(person.clone());
                if let Some(leg_id) = self.pending.remove(&person) {
                    self.observed_legs[leg_id].completion = LegCompletion::Stuck;
                }
            }
        }
    }

    fn complete_leg(&mut self, person: &str, leg_id: usize, arrival_seconds: f64) {
        self.observed_legs[leg_id].completion = LegCompletion::Completed { arrival_seconds };
        self.pending.remove(person);
    }
}

fn write_tables(
    path: &Path,
    links: &[&Link],
    counts: &LinkVolumesByHour,
    observed_legs: &[ObservedLeg],
    expected: &BTreeMap<String, Vec<(usize, String)>>,
    stuck_people: &BTreeSet<String>,
    interval: u32,
    simulation_end_time: u32,
) -> Result<(), AnalysisError> {
    let mut hourly = BufWriter::new(File::create(path.join("link_hourly.csv")).map_err(io_error)?);
    writeln!(
        hourly,
        "link_id,hour_start_seconds,entry_vehicles,exit_vehicles"
    )
    .map_err(io_error)?;
    let mut hours: BTreeSet<u64> = counts.keys().map(|key| key.hour_start_seconds).collect();
    hours.extend((0..u64::from(simulation_end_time)).step_by(interval as usize));
    hours.insert(0);
    for hour in &hours {
        let hour = *hour;
        for link in links {
            let volumes = counts
                .get(&LinkHour {
                    hour_start_seconds: hour,
                    link_id: link.id.external().to_owned(),
                })
                .copied()
                .unwrap_or_default();
            writeln!(
                hourly,
                "{},{hour},{},{}",
                csv(link.id.external()),
                volumes.entries,
                volumes.exits,
            )
            .map_err(io_error)?;
        }
    }
    let mut coverage = BufWriter::new(File::create(path.join("coverage.csv")).map_err(io_error)?);
    writeln!(
        coverage,
        "hour_start_seconds,eligible_links,used_links,unused_links,used_percent"
    )
    .map_err(io_error)?;
    for hour in hours {
        let used = links
            .iter()
            .filter(|link| {
                counts
                    .get(&LinkHour {
                        hour_start_seconds: hour,
                        link_id: link.id.external().to_owned(),
                    })
                    .is_some_and(|volumes| volumes.entries + volumes.exits > 0)
            })
            .count();
        let total = links.len();
        let percent = if total == 0 {
            0.0
        } else {
            used as f64 * 100.0 / total as f64
        };
        writeln!(
            coverage,
            "{hour},{total},{used},{},{percent:.6}",
            total - used
        )
        .map_err(io_error)?;
    }
    let mut legs = BufWriter::new(File::create(path.join("legs.csv")).map_err(io_error)?);
    writeln!(legs, "person_id,leg_index,mode,departure_seconds,departure_hour_seconds,arrival_seconds,duration_seconds,status").map_err(io_error)?;
    let mut by_mode_hour = BTreeMap::<ModeHour, HourlyLegs>::new();
    let mut person_totals = BTreeMap::<String, f64>::new();
    let mut person_activity = BTreeMap::<String, PersonActivity>::new();
    // Every observed leg, planned or not, occupies its `person_id,leg_index`
    // row, so planned legs are only added below when no observed leg covers them.
    let mut observed_leg_keys = BTreeSet::new();
    for leg in observed_legs {
        observed_leg_keys.insert((leg.person_id.clone(), leg.leg_index));
        let duration = leg.completion.duration(leg.departure_seconds);
        writeln!(
            legs,
            "{},{},{},{:.6},{},{},{},{}",
            csv(&leg.person_id),
            leg.leg_index,
            csv(&leg.mode),
            leg.departure_seconds,
            leg.departure_hour,
            leg.completion
                .arrival_seconds()
                .map(|v| format!("{v:.6}"))
                .unwrap_or_default(),
            duration.map(|v| format!("{v:.6}")).unwrap_or_default(),
            leg.completion.status()
        )
        .map_err(io_error)?;
        let aggregate = by_mode_hour
            .entry(ModeHour {
                hour_start_seconds: leg.departure_hour,
                mode: leg.mode.clone(),
            })
            .or_default();
        aggregate.departures += 1;
        aggregate.persons.insert(leg.person_id.clone());
        let activity = person_activity.entry(leg.person_id.clone()).or_default();
        activity.departures += 1;
        activity.expected_departures += usize::from(leg.expected_plan_leg);
        activity.completed_legs += usize::from(duration.is_some());
        if let Some(duration) = duration {
            aggregate.duration_sum += duration;
            aggregate.completed += 1;
            *person_totals.entry(leg.person_id.clone()).or_default() += duration;
        }
    }
    for (person, expected_legs) in expected {
        for (leg_index, mode) in expected_legs {
            if !observed_leg_keys.contains(&(person.clone(), *leg_index)) {
                // A leg that was never departed is `not_departed` even for a stuck person: the
                // stuck event names no leg, so only `person_daily.csv` can report the day as
                // stuck. A leg that was open when the person got stuck is an observed leg and
                // already carries `LegCompletion::Stuck`.
                writeln!(
                    legs,
                    "{},{},{},,,,,not_departed",
                    csv(person),
                    leg_index,
                    csv(mode),
                )
                .map_err(io_error)?;
            }
        }
    }
    let mut hourly_legs =
        BufWriter::new(File::create(path.join("leg_hourly.csv")).map_err(io_error)?);
    writeln!(hourly_legs, "departure_hour_seconds,mode,departures,departing_persons,completed_legs,mean_duration_seconds").map_err(io_error)?;
    for (key, value) in by_mode_hour {
        let mean = if value.completed == 0 {
            String::new()
        } else {
            format!("{:.6}", value.duration_sum / value.completed as f64)
        };
        writeln!(
            hourly_legs,
            "{},{},{},{},{},{}",
            key.hour_start_seconds,
            csv(&key.mode),
            value.departures,
            value.persons.len(),
            value.completed,
            mean
        )
        .map_err(io_error)?;
    }
    let mut daily = BufWriter::new(File::create(path.join("person_daily.csv")).map_err(io_error)?);
    writeln!(daily, "person_id,expected_legs,departed_legs,completed_legs,completed_duration_sum_seconds,completed_duration_mean_seconds,completion_status").map_err(io_error)?;
    let mut complete_all = Vec::new();
    let mut complete_travelers = Vec::new();
    let mut person_ids: BTreeSet<_> = expected.keys().cloned().collect();
    person_ids.extend(person_activity.keys().cloned());
    person_ids.extend(stuck_people.iter().cloned());
    for person in person_ids {
        let expected_legs = expected.get(&person).map_or(&[][..], Vec::as_slice);
        let default_activity = PersonActivity::default();
        let activity = person_activity.get(&person).unwrap_or(&default_activity);
        let sum = person_totals.get(&person).copied().unwrap_or_default();
        let status = if stuck_people.contains(&person) {
            "stuck"
        } else if expected_legs.is_empty() && activity.departures == 0 {
            "no_travel"
        } else if activity.departures == expected_legs.len()
            && activity.expected_departures == expected_legs.len()
            && activity.completed_legs == expected_legs.len()
        {
            "complete"
        } else {
            "incomplete"
        };
        let mean = if activity.completed_legs == 0 {
            String::new()
        } else {
            format!("{:.6}", sum / activity.completed_legs as f64)
        };
        writeln!(
            daily,
            "{},{},{},{},{:.6},{},{}",
            csv(&person),
            expected_legs.len(),
            activity.departures,
            activity.completed_legs,
            sum,
            mean,
            status
        )
        .map_err(io_error)?;
        if matches!(status, "complete" | "no_travel") {
            complete_all.push(sum);
            if activity.departures > 0 {
                complete_travelers.push(sum);
            }
        }
    }
    let mut daily_summary =
        BufWriter::new(File::create(path.join("daily_summary.csv")).map_err(io_error)?);
    writeln!(
        daily_summary,
        "cohort,persons,mean_completed_leg_duration_sum_seconds"
    )
    .map_err(io_error)?;
    for (label, values) in [
        ("all_complete_persons", complete_all),
        ("travelers", complete_travelers),
    ] {
        let mean = if values.is_empty() {
            String::new()
        } else {
            format!("{:.6}", values.iter().sum::<f64>() / values.len() as f64)
        };
        writeln!(daily_summary, "{label},{},{}", values.len(), mean).map_err(io_error)?;
    }
    Ok(())
}

fn write_report(path: &Path, iteration: u32, links: usize) -> Result<(), AnalysisError> {
    let coverage = csv_for_script(&path.join("coverage.csv"))?;
    let modules = fs::read_to_string(path.join("module_status.json")).map_err(io_error)?;
    let hourly = csv_for_script(&path.join("link_hourly.csv"))?;
    let leg_hourly = csv_for_script(&path.join("leg_hourly.csv"))?;
    let daily = csv_for_script(&path.join("daily_summary.csv"))?;
    let persons = csv_for_script(&path.join("person_daily.csv"))?;
    // One row per leg, so only a bounded prefix is embedded and the rest stays in the CSV.
    let (legs, legs_truncated) = csv_preview_for_script(&path.join("legs.csv"), LEGS_PREVIEW_ROWS)?;
    let legs_note = if legs_truncated {
        format!(
            "Showing the first {LEGS_PREVIEW_ROWS} rows of <a href=\"legs.csv\">legs.csv</a>, which holds every leg."
        )
    } else {
        "Every observed and planned leg is listed in <a href=\"legs.csv\">legs.csv</a>.".to_owned()
    };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>body{{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}}table{{border-collapse:collapse;margin-bottom:2rem}}td,th{{border:1px solid #ccd;padding:.5rem}}a{{color:#075ea8}}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links.</p><h2>Hourly volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end.</p><h3>Per-link hourly entry and exit vehicles</h3><div id=\"hourly\"></div><h3>Hourly coverage</h3><div id=\"coverage\"></div><h2>Agent travel</h2><p>Leg completion uses observed departure and arrival events. Incomplete persons retain completed-leg duration totals; missing arrivals are excluded from duration means. Verified non-travelers have an expected plan with no legs.</p><h3>Departures and duration by hour/mode</h3><div id=\"leg-hourly\"></div><h3>Daily cohort means</h3><div id=\"daily\"></div><h3>Person daily totals and status</h3><div id=\"persons\"></div><h3>Observed and planned legs</h3><p>{legs_note}</p><div id=\"legs\"></div><h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>const h={hourly};const c={coverage};const a={leg_hourly};const d={daily};const p={persons};const g={legs};const m={modules};function table(root,headers,rows){{const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)}});const body=t.createTBody();rows.forEach(row=>{{const tr=body.insertRow();row.forEach(x=>{{const cell=tr.insertCell();cell.textContent=x}})}});root.appendChild(t)}}function parseCsv(line){{const fields=[];let field='',quoted=false;for(let i=0;i<line.length;i++){{const ch=line[i];if(ch.charCodeAt(0)===34){{if(quoted&&line.charCodeAt(i+1)===34){{field+=String.fromCharCode(34);i++}}else{{quoted=!quoted}}}}else if(ch===','&&!quoted){{fields.push(field);field=''}}else{{field+=ch}}}}fields.push(field);return fields}}function csvTable(id,rows){{table(document.querySelector(id),parseCsv(rows[0]),rows.slice(1).map(parseCsv))}}csvTable('#hourly',h);csvTable('#coverage',c);csvTable('#leg-hourly',a);csvTable('#daily',d);csvTable('#persons',p);csvTable('#legs',g);table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))</script></body></html>"
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
}

fn csv_for_script(path: &Path) -> Result<String, AnalysisError> {
    let csv = fs::read_to_string(path).map_err(io_error)?;
    json_for_script(&csv.lines().collect::<Vec<_>>())
}

/// Embeds at most `rows` lines, header included, without reading the whole file.
/// Reports whether the file had more lines than were embedded.
fn csv_preview_for_script(path: &Path, rows: usize) -> Result<(String, bool), AnalysisError> {
    let file = File::open(path).map_err(io_error)?;
    let mut lines = Vec::new();
    let mut truncated = false;
    for line in BufReader::new(file).lines() {
        if lines.len() == rows {
            truncated = true;
            break;
        }
        lines.push(line.map_err(io_error)?);
    }
    Ok((json_for_script(&lines)?, truncated))
}

fn json_for_script(value: &impl Serialize) -> Result<String, AnalysisError> {
    serde_json::to_string(value)
        .map(|json| {
            json.replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
        })
        .map_err(|e| AnalysisError(e.to_string()))
}

fn csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn io_error(error: std::io::Error) -> AnalysisError {
    AnalysisError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::InternalAttributes;
    use crate::simulation::events::{PersonArrivalEvent, PersonDepartureEvent};
    use crate::simulation::id::Id;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn same_time_arrival_and_departure_pair_independently_of_partition_order() {
        fn events(arrival_first: bool) -> Vec<Box<dyn EventTrait>> {
            let person = Id::create("p");
            let arrival: Box<dyn EventTrait> = Box::new(PersonArrivalEvent {
                time: SimTime::from_secs(10),
                person: person.clone(),
                link: Id::create("l"),
                leg_mode: Id::create("car"),
                attributes: InternalAttributes::default(),
            });
            let departure: Box<dyn EventTrait> = Box::new(PersonDepartureEvent {
                time: SimTime::from_secs(10),
                person,
                link: Id::create("l"),
                leg_mode: Id::create("car"),
                routing_mode: Id::create("car"),
                attributes: InternalAttributes::default(),
            });
            if arrival_first {
                vec![arrival, departure]
            } else {
                vec![departure, arrival]
            }
        }

        // Partitions can deliver the same-timestamp events in either order.
        for arrival_first in [true, false] {
            let events = events(arrival_first);
            let mut accumulator = AgentTravelAccumulator::new(3600, BTreeMap::new());

            accumulator.process_timestamp(&events, SimTime::from_secs(10));

            assert_eq!(accumulator.observed_legs.len(), 1);
            assert_eq!(
                accumulator.observed_legs[0].completion.status(),
                "completed"
            );
            assert_eq!(
                accumulator.observed_legs[0].completion.arrival_seconds(),
                Some(10.0)
            );
        }
    }

    #[deterministic_id_test]
    fn incomplete_days_keep_partial_sums_out_of_complete_cohort_means() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("ITERS/it.0/events");
        fs::create_dir_all(&events).unwrap();
        fs::write(
            events.join("events.0.xml"),
            r#"<events>
                <event time="100" type="departure" person="partial" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="120" type="departure" person="partial" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="130" type="departure" person="partial" link="l" legMode="walk" computationalRoutingMode="walk" />
                <event time="140" type="departure" person="stuck" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="200" type="arrival" person="zero" link="l" legMode="walk" />
                <event time="200" type="arrival" person="zero" link="l" legMode="car" />
                <event time="36100" type="departure" person="missed_plan_leg" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="40000" type="departure" person="unplanned" link="l" legMode="bike" computationalRoutingMode="bike" />
                <event time="50000" type="departure" person="unplanned" link="l" legMode="train" computationalRoutingMode="train" />
                <event time="60000" type="departure" person="stuck_midway" link="l" legMode="walk" computationalRoutingMode="walk" />
                <event time="86390" type="departure" person="traveler" link="l" legMode="car" computationalRoutingMode="car" />
            </events>"#,
        )
        .unwrap();
        fs::write(
            events.join("events.1.xml"),
            r#"<events>
                <event time="125" type="arrival" person="partial" link="l" legMode="car" />
                <event time="150" type="stuckAndAbort" person="stuck" />
                <event time="160" type="stuckAndAbort" person="stuck_no_departure" />
                <event time="170" type="stuckAndAbort" person="orphan_stuck" />
                <event time="200" type="departure" person="zero" link="l" legMode="walk" computationalRoutingMode="walk" />
                <event time="200" type="departure" person="zero" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="36110" type="arrival" person="missed_plan_leg" link="l" legMode="car" />
                <event time="40010" type="arrival" person="unplanned" link="l" legMode="bike" />
                <event time="50020" type="arrival" person="unplanned" link="l" legMode="train" />
                <event time="60010" type="stuckAndAbort" person="stuck_midway" />
                <event time="86400" type="arrival" person="traveler" link="l" legMode="car" />
            </events>"#,
        )
        .unwrap();
        let expected_travel = vec![
            expected_person("traveler", &[(0, "car")]),
            expected_person("partial", &[(0, "car"), (1, "car"), (2, "walk")]),
            expected_person("stuck", &[(0, "car")]),
            expected_person("nontraveler", &[]),
            expected_person("missing", &[(0, "bike")]),
            expected_person("zero", &[(0, "walk"), (1, "car")]),
            expected_person("stuck_no_departure", &[(0, "car")]),
            expected_person("missed_plan_leg", &[(1, "walk"), (3, "car")]),
            expected_person("unplanned", &[(0, "car")]),
            expected_person("stuck_midway", &[(0, "walk"), (1, "car"), (2, "train")]),
        ];
        let garage = Garage::default();
        let metadata = AnalysisRunMetadata {
            random_seed: 0,
            network_input: None,
            population_input: None,
            vehicles_input: None,
            expected_travel: &expected_travel,
            garage: &garage,
        };
        let report = analyze_final_iteration(
            dir.path(),
            0,
            2,
            CompressionType::None,
            86400,
            &metadata,
            &Network::new(),
            &Analysis {
                enabled: true,
                interval_seconds: 3600,
            },
        )
        .unwrap();
        let output = report.parent().unwrap();

        let persons = fs::read_to_string(output.join("person_daily.csv")).unwrap();
        assert!(persons.contains("\"partial\",3,3,1,5.000000,5.000000,incomplete"));
        assert!(persons.contains("\"stuck\",1,1,0,0.000000,,stuck"));
        assert!(persons.contains("\"nontraveler\",0,0,0,0.000000,,no_travel"));
        assert!(persons.contains("\"missing\",1,0,0,0.000000,,incomplete"));
        assert!(persons.contains("\"zero\",2,2,2,0.000000,0.000000,complete"));
        assert!(persons.contains("\"stuck_no_departure\",1,0,0,0.000000,,stuck"));
        assert!(persons.contains("\"orphan_stuck\",0,0,0,0.000000,,stuck"));
        assert!(persons.contains("\"missed_plan_leg\",2,1,1,10.000000,10.000000,incomplete"));
        let legs = fs::read_to_string(output.join("legs.csv")).unwrap();
        assert!(legs.contains("\"partial\",0,\"car\",100.000000,0,,,missing_arrival"));
        assert!(legs.contains("\"missing\",0,\"bike\",,,,,not_departed"));
        assert!(legs.contains("\"stuck_no_departure\",0,\"car\",,,,,not_departed"));
        // A stuck event names no leg: only the leg that was open is `stuck`, and the
        // planned legs the person never reached stay `not_departed`.
        assert!(legs.contains("\"stuck\",0,\"car\",140.000000,0,,,stuck"));
        assert!(legs.contains("\"stuck_midway\",0,\"walk\",60000.000000,57600,,,stuck"));
        assert!(legs.contains("\"stuck_midway\",1,\"car\",,,,,not_departed"));
        assert!(legs.contains("\"stuck_midway\",2,\"train\",,,,,not_departed"));
        assert!(persons.contains("\"stuck_midway\",3,1,0,0.000000,,stuck"));
        assert!(legs.contains("\"missed_plan_leg\",1,\"walk\",,,,,not_departed"));
        assert!(legs.contains(
            "\"missed_plan_leg\",3,\"car\",36100.000000,36000,36110.000000,10.000000,completed"
        ));
        // Unplanned legs are indexed after the plan and stay unique per person
        // across departure batches, so they never collide with planned legs.
        assert!(legs.contains(
            "\"unplanned\",1,\"bike\",40000.000000,39600,40010.000000,10.000000,completed"
        ));
        assert!(legs.contains(
            "\"unplanned\",2,\"train\",50000.000000,46800,50020.000000,20.000000,completed"
        ));
        assert!(legs.contains("\"unplanned\",0,\"car\",,,,,not_departed"));
        assert!(persons.contains("\"unplanned\",1,2,2,30.000000,15.000000,incomplete"));
        let summary = fs::read_to_string(output.join("daily_summary.csv")).unwrap();
        assert!(summary.contains("all_complete_persons,3,3.333333"));
        assert!(summary.contains("travelers,2,5.000000"));
        // The local report presents the agent-travel tables, not only the CSVs.
        let report_html = fs::read_to_string(output.join("index.html")).unwrap();
        assert!(report_html.contains("<h2>Agent travel</h2>"));
        assert!(report_html.contains("href=\"legs.csv\""));
        assert!(report_html.contains("travelers,2,5.000000"));
        assert!(report_html.contains("missed_plan_leg"));
        // Leg rows are embedded, so the leg-level metric has a presentation and not just a link.
        assert!(report_html.contains("person_id,leg_index,mode,departure_seconds"));
        assert!(report_html.contains("stuck_midway"));
        let statuses = read_json(&output.join("module_status.json"));
        let agent_travel = statuses
            .as_array()
            .expect("module status is an array")
            .iter()
            .find(|status| status["module"] == "agent_travel")
            .expect("agent_travel module status is reported");
        assert_eq!(agent_travel["status"], "complete");
        assert!(agent_travel["reason"].is_null());
        let catalog = read_json(&output.join("metric_catalog.json"));
        let names: BTreeSet<&str> = catalog
            .as_array()
            .expect("metric catalog is an array")
            .iter()
            .map(|metric| metric["name"].as_str().expect("metric name"))
            .collect();
        for metric in [
            "leg_departures",
            "departing_persons",
            "leg_duration_mean",
            "person_completed_leg_duration_sum",
            "person_completed_leg_duration_mean",
            "daily_mean_completed_travel_burden",
            "leg_completion_status",
        ] {
            assert!(names.contains(metric), "missing catalogued metric {metric}");
        }
        let hourly = fs::read_to_string(output.join("leg_hourly.csv")).unwrap();
        assert!(hourly.contains("0,\"car\",4,3,2,2.500000"));
        assert!(hourly.contains("0,\"walk\",2,2,1,0.000000"));
        assert!(hourly.contains("82800,\"car\",1,1,1,10.000000"));
        assert!(hourly.contains("36000,\"car\",1,1,1,10.000000"));
    }

    fn read_json(path: &Path) -> serde_json::Value {
        let raw = fs::read_to_string(path).unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    fn expected_person(person_id: &str, legs: &[(usize, &str)]) -> PersonExpectedTravel {
        PersonExpectedTravel {
            person_id: person_id.to_owned(),
            legs: legs
                .iter()
                .map(|(leg_index, mode)| ExpectedLeg {
                    leg_index: *leg_index,
                    mode: (*mode).to_owned(),
                    departure_seconds: None,
                    expected_travel_seconds: None,
                })
                .collect(),
        }
    }
}
