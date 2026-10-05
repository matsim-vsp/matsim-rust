//! Final-iteration link coverage and capacity reporting.

pub mod capacity;

use crate::simulation::config::{Analysis, CompressionType};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
use capacity::{
    FlowSide, IntervalVolumes, LinkUtilization, VC_BIN_COUNT, VcHistogram, vc_bin_bounds,
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
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
    /// Simulated fraction of the population the volumes were scaled up from.
    sample_size: f64,
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
    /// Simulated fraction of the population; observed volumes are scaled up by
    /// its reciprocal before being compared with network capacity.
    pub sample_size: f64,
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

type LinkVolumesByHour = BTreeMap<LinkHour, IntervalVolumes>;

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
    if !run_metadata.sample_size.is_finite() || run_metadata.sample_size <= 0.0 {
        return Err(AnalysisError(format!(
            "sample size must be a positive finite number to scale volumes, got {}",
            run_metadata.sample_size
        )));
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
            run_metadata.garage,
            &mut counts,
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
        settings.interval_seconds,
        simulation_end_time,
        run_metadata.sample_size,
    )?;
    let manifest = Manifest {
        status: "complete",
        iteration,
        interval_seconds: settings.interval_seconds,
        partitions: (0..partitions).collect(),
        input_format: ext,
        eligible_links: ordered_links.len(),
        random_seed: run_metadata.random_seed,
        sample_size: run_metadata.sample_size,
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
            name: "entry_pce_vehicles",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_pce_vehicles",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "entry_vc",
            unit: "ratio",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_vc",
            unit: "ratio",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "entry_vc_histogram",
            unit: "links",
            aggregation_key: "interval_start_seconds,bin_index",
        },
        Metric {
            name: "exit_vc_histogram",
            unit: "links",
            aggregation_key: "interval_start_seconds,bin_index",
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
            module: "link_capacity",
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
            status: "unavailable",
            reason: Some("Observed leg and journey metrics are not implemented yet"),
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
    garage: &Garage,
    counts: &mut LinkVolumesByHour,
) {
    let (link, vehicle, entry) =
        if let Some(event) = event.as_any().downcast_ref::<LinkEnterEvent>() {
            (&event.link, &event.vehicle, true)
        } else if let Some(event) = event.as_any().downcast_ref::<LinkLeaveEvent>() {
            (&event.link, &event.vehicle, false)
        } else if let Some(event) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
            (&event.link, &event.vehicle, true)
        } else if let Some(event) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
            (&event.link, &event.vehicle, false)
        } else {
            return;
        };
    let id = link.external();
    if !ids.contains(id) {
        return;
    }
    let hour = time.as_nanos() / 1_000_000_000 / u64::from(interval) * u64::from(interval);
    // Vehicles that never appear in the garage, e.g. transit or DRT units, leave the
    // PCE total for the interval unusable instead of silently counting as zero.
    let pce = garage.vehicles.get(vehicle).map(|vehicle| vehicle.pce);
    let side = if entry {
        FlowSide::Entry
    } else {
        FlowSide::Exit
    };
    counts
        .entry(LinkHour {
            hour_start_seconds: hour,
            link_id: id.to_owned(),
        })
        .or_default()
        .record(side, pce);
}

fn write_tables(
    path: &Path,
    links: &[&Link],
    counts: &LinkVolumesByHour,
    interval: u32,
    simulation_end_time: u32,
    sample_size: f64,
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
    let interval_hours = f64::from(interval) / 3600.0;
    // Every table below covers the same intervals, including those without any events.
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
    let mut capacities: BTreeMap<u64, IntervalHistograms> = BTreeMap::new();
    for hour in &hours {
        let hour = *hour;
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
        let histograms = capacities.entry(hour).or_default();
        for link in links {
            let volumes = counts
                .get(&LinkHour {
                    hour_start_seconds: hour,
                    link_id: link.id.external().to_owned(),
                })
                .copied()
                .unwrap_or_default();
            let utilization =
                LinkUtilization::new(link, hour, interval_hours, sample_size, &volumes);
            histograms.entry.observe(&utilization, FlowSide::Entry);
            histograms.exit.observe(&utilization, FlowSide::Exit);
        }
    }
    write_capacity_table(path, links, counts, &hours, interval_hours, sample_size)?;
    write_histograms(path, &capacities)?;
    Ok(())
}

/// Per-link hourly PCE volumes, effective capacity and V/C ratios.
///
/// Raw vehicle counts, the observed PCE volume and the volume expanded to the
/// unsampled network are separate columns, and a link's raw network capacity is
/// never multiplied by its lane count.
fn write_capacity_table(
    path: &Path,
    links: &[&Link],
    counts: &LinkVolumesByHour,
    hours: &BTreeSet<u64>,
    interval_hours: f64,
    sample_size: f64,
) -> Result<(), AnalysisError> {
    let mut table = BufWriter::new(File::create(path.join("link_capacity.csv")).map_err(io_error)?);
    writeln!(
        table,
        "link_id,interval_start_seconds,capacity_pce_per_hour,effective_capacity_pce,permlanes,interval_hours,sample_size,entry_vehicles,exit_vehicles,entry_pce,exit_pce,entry_unresolved_pce,exit_unresolved_pce,entry_pce_scaled,exit_pce_scaled,entry_flow_pce_per_hour,exit_flow_pce_per_hour,entry_vc,exit_vc,entry_vc_status,exit_vc_status"
    )
    .map_err(io_error)?;
    for hour in hours {
        for link in links {
            let volumes = counts
                .get(&LinkHour {
                    hour_start_seconds: *hour,
                    link_id: link.id.external().to_owned(),
                })
                .copied()
                .unwrap_or_default();
            let utilization =
                LinkUtilization::new(link, *hour, interval_hours, sample_size, &volumes);
            let (entry, exit) = (&utilization.entry, &utilization.exit);
            writeln!(
                table,
                "{},{},{:.6},{},{:.6},{:.6},{:.6},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                csv(utilization.link_id),
                *hour,
                utilization.capacity_pce_per_hour,
                // The V/C denominator. Blank exactly when the capacity is unusable.
                number_opt(entry.effective_capacity_pce),
                utilization.permlanes,
                interval_hours,
                sample_size,
                utilization.entry_vehicles,
                utilization.exit_vehicles,
                // Observed PCE volumes depend only on the vehicles, so a link with
                // an unusable capacity still reports what it carried.
                number_opt(volumes.pce(FlowSide::Entry)),
                number_opt(volumes.pce(FlowSide::Exit)),
                volumes.entry_unresolved_pce,
                volumes.exit_unresolved_pce,
                number_opt(entry.expanded_pce),
                number_opt(exit.expanded_pce),
                number_opt(entry.flow_pce_per_hour),
                number_opt(exit.flow_pce_per_hour),
                number_opt(entry.ratio),
                number_opt(exit.ratio),
                entry.status.label(),
                exit.status.label(),
            )
            .map_err(io_error)?;
        }
    }
    Ok(())
}

/// Fixed-bin V/C distributions per interval, one row per bin per side.
fn write_histograms(
    path: &Path,
    capacities: &BTreeMap<u64, IntervalHistograms>,
) -> Result<(), AnalysisError> {
    let mut table = BufWriter::new(File::create(path.join("vc_histogram.csv")).map_err(io_error)?);
    writeln!(
        table,
        "interval_start_seconds,metric,bin_index,bin_lower,bin_upper,links,observations,unused_links,unavailable_links"
    )
    .map_err(io_error)?;
    for (hour, histograms) in capacities {
        for (metric, histogram) in [
            (FlowSide::Entry, &histograms.entry),
            (FlowSide::Exit, &histograms.exit),
        ] {
            for bin in 0..VC_BIN_COUNT {
                let (lower, upper) = vc_bin_bounds(bin);
                writeln!(
                    table,
                    "{hour},{},{bin},{lower:.3},{},{},{},{},{}",
                    metric.label(),
                    upper
                        .map(|upper| format!("{upper:.3}"))
                        .unwrap_or_else(|| "inf".to_owned()),
                    histogram.bins[bin],
                    histogram.observations,
                    histogram.unused_links,
                    histogram.unavailable_links,
                )
                .map_err(io_error)?;
            }
        }
    }
    Ok(())
}

/// Both V/C distributions of one reported interval.
#[derive(Default)]
struct IntervalHistograms {
    entry: VcHistogram,
    exit: VcHistogram,
}

/// An unavailable quantity is exported blank, a non-finite one as `nan`.
fn number_opt(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| format!("{value:.6}"))
}

fn write_report(path: &Path, iteration: u32, links: usize) -> Result<(), AnalysisError> {
    let coverage = fs::read_to_string(path.join("coverage.csv")).map_err(io_error)?;
    let coverage = json_for_script(&coverage.lines().collect::<Vec<_>>())?;
    let modules = fs::read_to_string(path.join("module_status.json")).map_err(io_error)?;
    let hourly = fs::read_to_string(path.join("link_hourly.csv")).map_err(io_error)?;
    let hourly = json_for_script(&hourly.lines().collect::<Vec<_>>())?;
    let capacity = fs::read_to_string(path.join("link_capacity.csv")).map_err(io_error)?;
    let capacity = json_for_script(&capacity.lines().collect::<Vec<_>>())?;
    let histogram = fs::read_to_string(path.join("vc_histogram.csv")).map_err(io_error)?;
    // Split into columns so the report can filter by metric and label the bins itself.
    let histogram_rows = json_for_script(
        &histogram
            .lines()
            .map(|row| row.split(',').collect::<Vec<_>>())
            .collect::<Vec<_>>(),
    )?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>body{{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}}table{{border-collapse:collapse;margin-bottom:2rem}}td,th{{border:1px solid #ccd;padding:.5rem}}a{{color:#075ea8}}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links.</p><h2>Hourly volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end. Both result tables and module status are embedded for offline viewing.</p><h3>Per-link hourly entry and exit vehicles</h3><div id=\"hourly\"></div><h3>Hourly coverage</h3><div id=\"coverage\"></div><h2>PCE volumes and capacity utilization</h2><p>Volumes are passenger-car-equivalent weighted, matching how the link flow cap is charged, and are scaled up by the simulated sample fraction to describe the full population. Raw vehicle counts, observed PCE volumes and scaled PCE volumes are exported separately. The V/C denominator is the link's own network capacity multiplied by the interval length; lanes are never applied again, and a value on a bin edge belongs to the higher bin. Unused links carry no vehicles and are counted apart from lightly used ones, while missing PCE or an invalid capacity leaves the ratio blank and is reported per link.</p><h3>Per-link hourly PCE volumes, capacity and V/C</h3><div id=\"capacity\"></div><h3>Hourly V/C distribution</h3><p id=\"histogram-metric-label\">Entry V/C (default view)</p><div id=\"histogram\"></div><button id=\"histogram-toggle\" type=\"button\">Show exit V/C</button><h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"link_capacity.csv\">PCE volumes, capacity and V/C (CSV)</a>, <a href=\"vc_histogram.csv\">V/C distribution (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>const h={hourly};const c={coverage};const p={capacity};const d={histogram_rows};const m={modules};function table(root,headers,rows){{const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)}});const body=t.createTBody();rows.forEach(row=>{{const tr=body.insertRow();row.forEach(x=>{{const cell=tr.insertCell();cell.textContent=x}})}});root.appendChild(t)}}table(document.querySelector('#hourly'),h[0].split(','),h.slice(1).map(x=>x.split(',')));table(document.querySelector('#coverage'),c[0].split(','),c.slice(1).map(x=>x.split(',')));table(document.querySelector('#capacity'),p[0].split(','),p.slice(1).map(x=>x.split(',')));const metricColumn=d[0].indexOf('metric');let metric='entry_vc';function histogram(){{const root=document.querySelector('#histogram');root.replaceChildren();table(root,d[0],d.slice(1).filter(x=>x[metricColumn]===metric));document.querySelector('#histogram-metric-label').textContent=metric==='entry_vc'?'Entry V/C (default view)':'Exit V/C';document.querySelector('#histogram-toggle').textContent=metric==='entry_vc'?'Show exit V/C':'Show entry V/C';}}histogram();document.querySelector('#histogram-toggle').addEventListener('click',()=>{{metric=metric==='entry_vc'?'exit_vc':'entry_vc';histogram()}});table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))</script></body></html>"
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
