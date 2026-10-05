//! Final-iteration link coverage and link speed reporting.

mod link_speed;

use crate::simulation::config::{Analysis, CompressionType};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use link_speed::LinkSpeedCollector;
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
    let mut speeds = LinkSpeedCollector::new(settings.interval_seconds, &ordered_links);
    loop {
        // Rank order breaks simultaneous timestamps consistently, so the replay order is stable.
        // The link counts commute, and the speed collector matches every enter with the leave of
        // the same link, so both stay independent of that order.
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
        speeds.observe(event.as_ref(), time);
        heads[rank] = readers[rank].next_event()?;
    }
    speeds.finish();

    // Intervals include their start and exclude their end; the report keeps every interval that
    // holds observations plus every interval up to the end of the simulated day. A link speed
    // observation starts with a link entry in the same interval, so the entry counts already cover
    // every interval that a speed can be reported for.
    let mut hours: BTreeSet<u64> = counts.keys().map(|key| key.hour_start_seconds).collect();
    hours.extend((0..u64::from(simulation_end_time)).step_by(settings.interval_seconds as usize));
    hours.insert(0);
    let hours: Vec<u64> = hours.into_iter().collect();

    let staging = output_dir.join(".analysis-staging");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_error)?;
    }
    fs::create_dir_all(&staging).map_err(io_error)?;
    write_volume_tables(&staging, &ordered_links, &counts, &hours)?;
    speeds.write_tables(&staging, &hours)?;
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
    // Every exported measure with its unit and the columns it is grouped by.
    let metrics: Vec<Metric> = [
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
            name: "link_speed_traversals",
            unit: "traversals",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "link_total_distance",
            unit: "m",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "link_total_duration",
            unit: "s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "link_representative_speed",
            unit: "m/s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "link_vehicle_speed_mean",
            unit: "m/s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "link_vehicle_speed_population_std",
            unit: "m/s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "links_with_speed",
            unit: "links",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "hourly_link_speed_traversals",
            unit: "traversals",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "hourly_mean_link_speed",
            unit: "m/s",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "hourly_link_speed_population_std",
            unit: "m/s",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "speed_histogram_link_count",
            unit: "links",
            aggregation_key: "hour_start_seconds,bin_index",
        },
        Metric {
            name: "speed_histogram_observation_count",
            unit: "traversals",
            aggregation_key: "hour_start_seconds,bin_index",
        },
    ]
    .into_iter()
    // The traversal records are counted by the module that writes the table, so it owns their names.
    .chain(
        link_speed::SpeedDiagnostics::METRICS
            .iter()
            .map(|name| Metric {
                name,
                unit: "records",
                aggregation_key: "report",
            }),
    )
    .collect();
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
            status: "complete",
            reason: None,
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

/// One recorded visit of a link. QSim reports the first and the last link of a network leg
/// through the departure and the arrival instead of a plain link enter and leave, and both of
/// those events carry the position along the link at which the visit starts or ends.
enum LinkVisit<'a> {
    Enter {
        vehicle: &'a Id<InternalVehicle>,
        link: &'a Id<Link>,
        entry_position: f64,
    },
    Leave {
        vehicle: &'a Id<InternalVehicle>,
        link: &'a Id<Link>,
        exit_position: f64,
    },
}

/// Classifies the events that describe a link visit, so that link volumes and link speeds agree
/// on which events are a link entry and which one is a link exit.
fn link_visit(event: &dyn EventTrait) -> Option<LinkVisit<'_>> {
    let event = event.as_any();
    if let Some(event) = event.downcast_ref::<LinkEnterEvent>() {
        Some(LinkVisit::Enter {
            vehicle: &event.vehicle,
            link: &event.link,
            entry_position: 0.0,
        })
    } else if let Some(event) = event.downcast_ref::<VehicleEntersTrafficEvent>() {
        Some(LinkVisit::Enter {
            vehicle: &event.vehicle,
            link: &event.link,
            entry_position: event.relative_position,
        })
    } else if let Some(event) = event.downcast_ref::<LinkLeaveEvent>() {
        Some(LinkVisit::Leave {
            vehicle: &event.vehicle,
            link: &event.link,
            exit_position: 1.0,
        })
    } else if let Some(event) = event.downcast_ref::<VehicleLeavesTrafficEvent>() {
        Some(LinkVisit::Leave {
            vehicle: &event.vehicle,
            link: &event.link,
            exit_position: event.relative_position,
        })
    } else {
        None
    }
}

fn accumulate(
    event: &dyn EventTrait,
    time: SimTime,
    interval: u32,
    ids: &BTreeSet<String>,
    counts: &mut LinkVolumesByHour,
) {
    let Some(visit) = link_visit(event) else {
        return;
    };
    let (link, entry) = match &visit {
        LinkVisit::Enter { link, .. } => (link, true),
        LinkVisit::Leave { link, .. } => (link, false),
    };
    let id = link.external();
    if !ids.contains(id) {
        return;
    }
    let hour = hour_start_seconds(time.as_nanos(), interval);
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

/// Start of the analysis interval that contains `nanos`; intervals include their start and
/// exclude their end.
fn hour_start_seconds(nanos: u64, interval: u32) -> u64 {
    nanos / 1_000_000_000 / u64::from(interval) * u64::from(interval)
}

fn write_volume_tables(
    path: &Path,
    links: &[&Link],
    counts: &LinkVolumesByHour,
    hours: &[u64],
) -> Result<(), AnalysisError> {
    let mut hourly = table_writer(path, "link_hourly.csv")?;
    writeln!(
        hourly,
        "link_id,hour_start_seconds,entry_vehicles,exit_vehicles"
    )
    .map_err(io_error)?;
    for hour in hours {
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
    let mut coverage = table_writer(path, "coverage.csv")?;
    writeln!(
        coverage,
        "hour_start_seconds,eligible_links,used_links,unused_links,used_percent"
    )
    .map_err(io_error)?;
    for hour in hours {
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
    }
    Ok(())
}

/// One exported CSV file rendered as a table of the report.
struct ReportTable {
    /// JavaScript variable and DOM id of the rendered table.
    name: &'static str,
    title: &'static str,
    file: &'static str,
}

const VOLUME_TABLES: &[ReportTable] = &[
    ReportTable {
        name: "volumes",
        title: "Per-link hourly entry and exit vehicles",
        file: "link_hourly.csv",
    },
    ReportTable {
        name: "coverage",
        title: "Hourly coverage",
        file: "coverage.csv",
    },
];

const SPEED_TABLES: &[ReportTable] = &[
    ReportTable {
        name: "linkSpeeds",
        title: "Per-link hourly speed",
        file: "link_speed_hourly.csv",
    },
    ReportTable {
        name: "speedSummary",
        title: "Across-link hourly speed summary",
        file: "link_speed_summary.csv",
    },
    ReportTable {
        name: "speedHistogram",
        title: "Hourly link speed histogram",
        file: "link_speed_histogram.csv",
    },
    ReportTable {
        name: "speedRecords",
        title: "Link speed traversal records",
        file: "link_speed_diagnostics.csv",
    },
];

/// A table of the report together with its embedded CSV lines.
struct EmbeddedTable<'a> {
    table: &'a ReportTable,
    data: String,
}

impl<'a> EmbeddedTable<'a> {
    fn read(path: &Path, table: &'a ReportTable) -> Result<Self, AnalysisError> {
        let content = fs::read_to_string(path.join(table.file)).map_err(io_error)?;
        let lines: Vec<_> = content.lines().collect();
        Ok(Self {
            table,
            data: json_for_script(&lines)?,
        })
    }

    /// The first embedded line holds the column names, the remaining lines the rows.
    fn declaration(&self) -> String {
        format!(
            "const {name}={data};",
            name = self.table.name,
            data = self.data
        )
    }

    fn section(&self) -> String {
        format!(
            "<h3>{title}</h3><div id=\"{name}\"></div>",
            title = self.table.title,
            name = self.table.name
        )
    }

    fn render(&self) -> String {
        let name = self.table.name;
        format!(
            "table(document.querySelector('#{name}'),{name}[0].split(','),{name}.slice(1).map(x=>x.split(',')));"
        )
    }
}

fn embed_tables<'a>(
    path: &Path,
    tables: &'a [ReportTable],
) -> Result<Vec<EmbeddedTable<'a>>, AnalysisError> {
    tables
        .iter()
        .map(|table| EmbeddedTable::read(path, table))
        .collect()
}

/// The headings and mount points of all tables of one report section.
fn sections(tables: &[EmbeddedTable<'_>]) -> String {
    tables
        .iter()
        .map(EmbeddedTable::section)
        .collect::<String>()
}

fn write_report(path: &Path, iteration: u32, links: usize) -> Result<(), AnalysisError> {
    let volumes = embed_tables(path, VOLUME_TABLES)?;
    let speeds = embed_tables(path, SPEED_TABLES)?;
    let modules = fs::read_to_string(path.join("module_status.json")).map_err(io_error)?;
    // The module status is the one table that is not exported as CSV, so it is embedded as its
    // JSON records instead of as lines of a table.
    let declarations = volumes
        .iter()
        .chain(&speeds)
        .map(EmbeddedTable::declaration)
        .chain(std::iter::once(format!("const m={modules};")))
        .collect::<Vec<_>>()
        .join("");
    let volume_sections = sections(&volumes);
    let speed_sections = sections(&speeds);
    let renders = volumes
        .iter()
        .chain(&speeds)
        .map(EmbeddedTable::render)
        .collect::<Vec<_>>()
        .join("");
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>body{{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}}table{{border-collapse:collapse;margin-bottom:2rem}}td,th{{border:1px solid #ccd;padding:.5rem}}a{{color:#075ea8}}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links.</p><h2>Hourly volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end. Both result tables and module status are embedded for offline viewing.</p>{volume_sections}<h2>Hourly link speeds</h2><p>Speeds are reconstructed from full-link traversals and assigned to the hour in which the vehicle entered the link. The representative speed divides the total travelled distance by the total travel time; the arithmetic vehicle-speed mean and population standard deviation describe the single traversals. A link without a full-link traversal has no speed: QSim inserts a vehicle at the end of the first link of a leg, so the first link of a network leg never covers its whole length and is reported as a partial traversal instead. The traversal records table lists every record that cannot produce a full-link speed, such as those partial traversals, traversals that never finished, and records without a positive duration.</p>{speed_sections}<h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"link_speed_hourly.csv\">link speeds (CSV)</a>, <a href=\"link_speed_summary.csv\">hourly speed summary (CSV)</a>, <a href=\"link_speed_histogram.csv\">speed histogram (CSV)</a>, <a href=\"link_speed_diagnostics.csv\">speed traversal records (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>{declarations}function table(root,headers,rows){{const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)}});const body=t.createTBody();rows.forEach(row=>{{const tr=body.insertRow();row.forEach(x=>{{const cell=tr.insertCell();cell.textContent=x}})}});root.appendChild(t)}}{renders}table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))</script></body></html>"
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
}

/// Opens one of the exported CSV tables for writing.
fn table_writer(path: &Path, name: &str) -> Result<BufWriter<File>, AnalysisError> {
    Ok(BufWriter::new(
        File::create(path.join(name)).map_err(io_error)?,
    ))
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
