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
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct AnalysisError(String);

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AnalysisError {}

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

#[derive(Clone)]
struct ObservedLeg {
    person_id: String,
    leg_index: usize,
    mode: String,
    departure_seconds: f64,
    departure_hour: u64,
    arrival_seconds: Option<f64>,
    status: &'static str,
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
    let mut departure_counts = BTreeMap::<String, usize>::new();
    let mut pending = BTreeMap::<String, VecDeque<usize>>::new();
    let mut observed_legs = Vec::<ObservedLeg>::new();
    loop {
        // Rank order breaks simultaneous timestamps consistently; these link counts commute.
        let Some((rank, time)) = heads
            .iter()
            .enumerate()
            .filter_map(|(rank, event)| event.as_ref().map(|(time, _)| (rank, *time)))
            .min_by_key(|(_, time)| *time)
        else {
            break;
        };
        let (_, event) = heads[rank].take().expect("selected reader head exists");
        accumulate(
            event.as_ref(),
            time,
            settings.interval_seconds,
            &ids,
            &mut counts,
        );
        accumulate_agent_event(
            event.as_ref(),
            time,
            settings.interval_seconds,
            &expected,
            &mut departure_counts,
            &mut pending,
            &mut observed_legs,
        );
        heads[rank] = readers[rank].next_event()?;
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
        &observed_legs,
        &expected,
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

fn accumulate_agent_event(
    event: &dyn EventTrait,
    time: SimTime,
    interval: u32,
    expected: &BTreeMap<String, Vec<(usize, String)>>,
    departure_counts: &mut BTreeMap<String, usize>,
    pending: &mut BTreeMap<String, VecDeque<usize>>,
    legs: &mut Vec<ObservedLeg>,
) {
    let seconds = time.as_nanos() as f64 / 1_000_000_000.0;
    if let Some(event) = event.as_any().downcast_ref::<PersonDepartureEvent>() {
        let person = event.person.external().to_owned();
        let offset = departure_counts.entry(person.clone()).or_default();
        let leg_index = expected
            .get(&person)
            .and_then(|person_legs| person_legs.get(*offset))
            .map_or(*offset, |(leg_index, _)| *leg_index);
        let mode = event.leg_mode.external().to_owned();
        *offset += 1;
        let leg_id = legs.len();
        legs.push(ObservedLeg {
            person_id: person.clone(),
            leg_index,
            mode,
            departure_seconds: seconds,
            departure_hour: time.as_nanos() / 1_000_000_000 / u64::from(interval)
                * u64::from(interval),
            arrival_seconds: None,
            status: "incomplete",
        });
        pending.entry(person).or_default().push_back(leg_id);
    } else if let Some(event) = event.as_any().downcast_ref::<PersonArrivalEvent>() {
        let person = event.person.external();
        if let Some(leg_id) = pending.get_mut(person).and_then(VecDeque::pop_front) {
            legs[leg_id].arrival_seconds = Some(seconds);
            legs[leg_id].status = "completed";
        }
    } else if let Some(event) = event.as_any().downcast_ref::<PersonStuckEvent>() {
        let person = event.person.external();
        if let Some(leg_id) = pending.get_mut(person).and_then(VecDeque::pop_front) {
            legs[leg_id].status = "stuck";
        }
    }
}

fn write_tables(
    path: &Path,
    links: &[&Link],
    counts: &LinkVolumesByHour,
    observed_legs: &[ObservedLeg],
    expected: &BTreeMap<String, Vec<(usize, String)>>,
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
    let mut person_totals = BTreeMap::<String, (f64, usize)>::new();
    let mut person_activity = BTreeMap::<String, (usize, usize, bool)>::new();
    let mut observed_plan_legs = BTreeSet::new();
    for leg in observed_legs {
        observed_plan_legs.insert((leg.person_id.clone(), leg.leg_index));
        let duration = leg
            .arrival_seconds
            .map(|arrival| arrival - leg.departure_seconds);
        writeln!(
            legs,
            "{},{},{},{:.6},{},{},{},{}",
            csv(&leg.person_id),
            leg.leg_index,
            csv(&leg.mode),
            leg.departure_seconds,
            leg.departure_hour,
            leg.arrival_seconds
                .map(|v| format!("{v:.6}"))
                .unwrap_or_default(),
            duration.map(|v| format!("{v:.6}")).unwrap_or_default(),
            leg.status
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
        activity.0 += 1;
        activity.1 += usize::from(duration.is_some());
        activity.2 |= leg.status == "stuck";
        if let Some(duration) = duration {
            aggregate.duration_sum += duration;
            aggregate.completed += 1;
            let total = person_totals.entry(leg.person_id.clone()).or_default();
            total.0 += duration;
            total.1 += 1;
        }
    }
    for (person, expected_legs) in expected {
        for (leg_index, mode) in expected_legs {
            if !observed_plan_legs.contains(&(person.clone(), *leg_index)) {
                writeln!(
                    legs,
                    "{},{},{},,,,,not_departed",
                    csv(person),
                    leg_index,
                    csv(mode)
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
    for person in person_ids {
        let expected_legs = expected.get(&person).map_or(&[][..], Vec::as_slice);
        let activity = person_activity.get(&person).copied().unwrap_or_default();
        let (departed, completed, stuck) = activity;
        let sum = person_totals.get(&person).map_or(0.0, |value| value.0);
        let status = if expected_legs.is_empty() && departed == 0 {
            "no_travel"
        } else if departed == expected_legs.len() && completed == expected_legs.len() && !stuck {
            "complete"
        } else if stuck {
            "stuck"
        } else {
            "incomplete"
        };
        let mean = if completed == 0 {
            String::new()
        } else {
            format!("{:.6}", sum / completed as f64)
        };
        writeln!(
            daily,
            "{},{},{},{},{:.6},{},{}",
            csv(&person),
            expected_legs.len(),
            departed,
            completed,
            sum,
            mean,
            status
        )
        .map_err(io_error)?;
        if matches!(status, "complete" | "no_travel") {
            complete_all.push(sum);
            if departed > 0 {
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
    let coverage = fs::read_to_string(path.join("coverage.csv")).map_err(io_error)?;
    let coverage = json_for_script(&coverage.lines().collect::<Vec<_>>())?;
    let modules = fs::read_to_string(path.join("module_status.json")).map_err(io_error)?;
    let hourly = fs::read_to_string(path.join("link_hourly.csv")).map_err(io_error)?;
    let hourly = json_for_script(&hourly.lines().collect::<Vec<_>>())?;
    let leg_hourly = fs::read_to_string(path.join("leg_hourly.csv")).map_err(io_error)?;
    let leg_hourly = json_for_script(&leg_hourly.lines().collect::<Vec<_>>())?;
    let daily = fs::read_to_string(path.join("daily_summary.csv")).map_err(io_error)?;
    let daily = json_for_script(&daily.lines().collect::<Vec<_>>())?;
    let persons = fs::read_to_string(path.join("person_daily.csv")).map_err(io_error)?;
    let persons = json_for_script(&persons.lines().collect::<Vec<_>>())?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>body{{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}}table{{border-collapse:collapse;margin-bottom:2rem}}td,th{{border:1px solid #ccd;padding:.5rem}}a{{color:#075ea8}}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links.</p><h2>Hourly volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end.</p><h3>Per-link hourly entry and exit vehicles</h3><div id=\"hourly\"></div><h3>Hourly coverage</h3><div id=\"coverage\"></div><h2>Agent travel</h2><p>Leg completion uses observed departure and arrival events. Incomplete persons retain completed-leg duration totals; missing arrivals are excluded from duration means. Verified non-travelers have an expected plan with no legs.</p><h3>Departures and duration by hour/mode</h3><div id=\"leg-hourly\"></div><h3>Daily cohort means</h3><div id=\"daily\"></div><h3>Person daily totals and status</h3><div id=\"persons\"></div><p><a href=\"legs.csv\">Observed and planned legs</a></p><h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>const h={hourly};const c={coverage};const a={leg_hourly};const d={daily};const p={persons};const m={modules};function table(root,headers,rows){{const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)}});const body=t.createTBody();rows.forEach(row=>{{const tr=body.insertRow();row.forEach(x=>{{const cell=tr.insertCell();cell.textContent=x}})}});root.appendChild(t)}}function parseCsv(line){{const fields=[];let field='',quoted=false;for(let i=0;i<line.length;i++){{const ch=line[i];if(ch.charCodeAt(0)===34){{if(quoted&&line.charCodeAt(i+1)===34){{field+=String.fromCharCode(34);i++}}else{{quoted=!quoted}}}}else if(ch===','&&!quoted){{fields.push(field);field=''}}else{{field+=ch}}}}fields.push(field);return fields}}function csvTable(id,rows){{table(document.querySelector(id),parseCsv(rows[0]),rows.slice(1).map(parseCsv))}}csvTable('#hourly',h);csvTable('#coverage',c);csvTable('#leg-hourly',a);csvTable('#daily',d);csvTable('#persons',p);table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))</script></body></html>"
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
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
    use crate::simulation::events::{PersonArrivalEvent, PersonDepartureEvent, PersonStuckEvent};
    use crate::simulation::id::Id;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn records_repeated_departures_midnight_stuck_and_missing_arrivals() {
        let expected = BTreeMap::from([
            (
                "p".to_owned(),
                vec![(0, "car".to_owned()), (2, "walk".to_owned())],
            ),
            ("stuck".to_owned(), vec![(0, "car".to_owned())]),
        ]);
        let mut counts = BTreeMap::new();
        let mut pending = BTreeMap::new();
        let mut legs = Vec::new();
        let departure = |person: &str, mode: &str| PersonDepartureEvent {
            time: SimTime::from_secs(0),
            person: Id::create(person),
            link: Id::create("l"),
            leg_mode: Id::create(mode),
            routing_mode: Id::create(mode),
            attributes: InternalAttributes::default(),
        };
        let arrival = |person: &str, secs| PersonArrivalEvent {
            time: SimTime::from_secs(secs),
            person: Id::create(person),
            link: Id::create("l"),
            leg_mode: Id::create("car"),
            attributes: InternalAttributes::default(),
        };
        let first = departure("p", "car");
        accumulate_agent_event(
            &first,
            SimTime::from_secs(86_399),
            3600,
            &expected,
            &mut counts,
            &mut pending,
            &mut legs,
        );
        let first_arrival = arrival("p", 86_400);
        accumulate_agent_event(
            &first_arrival,
            SimTime::from_secs(86_400),
            3600,
            &expected,
            &mut counts,
            &mut pending,
            &mut legs,
        );
        let second = departure("p", "walk");
        accumulate_agent_event(
            &second,
            SimTime::from_secs(86_401),
            3600,
            &expected,
            &mut counts,
            &mut pending,
            &mut legs,
        );
        let stuck_departure = departure("stuck", "car");
        accumulate_agent_event(
            &stuck_departure,
            SimTime::from_secs(86_402),
            3600,
            &expected,
            &mut counts,
            &mut pending,
            &mut legs,
        );
        let stuck_event = PersonStuckEvent {
            time: SimTime::from_secs(86_500),
            person: Id::create("stuck"),
            link: None,
            leg_mode: None,
            reason: None,
            attributes: InternalAttributes::default(),
        };
        accumulate_agent_event(
            &stuck_event,
            SimTime::from_secs(86_500),
            3600,
            &expected,
            &mut counts,
            &mut pending,
            &mut legs,
        );

        assert_eq!(legs.len(), 3);
        assert_eq!(legs[0].status, "completed");
        assert_eq!(legs[0].departure_hour, 82_800);
        assert_eq!(legs[0].arrival_seconds, Some(86_400.0));
        assert_eq!(legs[1].status, "incomplete");
        assert_eq!(legs[1].leg_index, 2);
        assert_eq!(legs[2].status, "stuck");
        assert_eq!(legs[2].arrival_seconds, None);
    }

    #[test]
    fn incomplete_days_keep_partial_sums_out_of_complete_cohort_means() {
        let dir = tempfile::tempdir().unwrap();
        let events = dir.path().join("ITERS/it.0/events");
        fs::create_dir_all(&events).unwrap();
        fs::write(
            events.join("events.0.xml"),
            r#"<events>
                <event time="100" type="departure" person="partial" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="120" type="departure" person="partial" link="l" legMode="walk" computationalRoutingMode="walk" />
                <event time="130" type="departure" person="stuck" link="l" legMode="car" computationalRoutingMode="car" />
                <event time="86390" type="departure" person="traveler" link="l" legMode="car" computationalRoutingMode="car" />
            </events>"#,
        )
        .unwrap();
        fs::write(
            events.join("events.1.xml"),
            r#"<events>
                <event time="110" type="arrival" person="partial" link="l" legMode="car" />
                <event time="140" type="stuckAndAbort" person="stuck" />
                <event time="86400" type="arrival" person="traveler" link="l" legMode="car" />
            </events>"#,
        )
        .unwrap();
        let expected_travel = vec![
            expected_person("traveler", &[(0, "car")]),
            expected_person("partial", &[(0, "car"), (1, "walk")]),
            expected_person("stuck", &[(0, "car")]),
            expected_person("nontraveler", &[]),
            expected_person("missing", &[(0, "bike")]),
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
        assert!(persons.contains("\"partial\",2,2,1,10.000000,10.000000,incomplete"));
        assert!(persons.contains("\"stuck\",1,1,0,0.000000,,stuck"));
        assert!(persons.contains("\"nontraveler\",0,0,0,0.000000,,no_travel"));
        assert!(persons.contains("\"missing\",1,0,0,0.000000,,incomplete"));
        let legs = fs::read_to_string(output.join("legs.csv")).unwrap();
        assert!(legs.contains("\"missing\",0,\"bike\",,,,,not_departed"));
        let summary = fs::read_to_string(output.join("daily_summary.csv")).unwrap();
        assert!(summary.contains("all_complete_persons,2,5.000000"));
        assert!(summary.contains("travelers,1,10.000000"));
        let hourly = fs::read_to_string(output.join("leg_hourly.csv")).unwrap();
        assert!(hourly.contains("0,\"car\",2,2,1,10.000000"));
        assert!(hourly.contains("0,\"walk\",1,1,0,"));
        assert!(hourly.contains("82800,\"car\",1,1,1,10.000000"));
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
