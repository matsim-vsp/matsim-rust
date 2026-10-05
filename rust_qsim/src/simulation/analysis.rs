//! Final-iteration link coverage reporting.

use crate::simulation::config::{Analysis, CompressionType, LinkLabels};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network, Node};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
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
    if let Some(boundary) = &settings.urban_boundary
        && (boundary.len() < 3
            || boundary
                .iter()
                .flatten()
                .any(|coordinate| !coordinate.is_finite()))
    {
        return Err(AnalysisError(
            "analysis.urban_boundary must contain at least three finite coordinates".into(),
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
    let classifications = classify_links(&ordered_links, network, settings);
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
    )?;
    write_classification(&staging, &ordered_links, &classifications)?;
    let grouped_coverage = write_group_coverage(
        &staging,
        &ordered_links,
        &classifications,
        &counts,
        settings.interval_seconds,
        simulation_end_time,
    )?;
    write_network_map(&staging, &ordered_links, network, &classifications)?;
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
            name: "group_eligible_links",
            unit: "links",
            aggregation_key: "dimension,category,hour_start_seconds",
        },
        Metric {
            name: "group_used_links",
            unit: "links",
            aggregation_key: "dimension,category,hour_start_seconds",
        },
        Metric {
            name: "group_unused_links",
            unit: "links",
            aggregation_key: "dimension,category,hour_start_seconds",
        },
        Metric {
            name: "group_used_link_percent",
            unit: "percent",
            aggregation_key: "dimension,category,hour_start_seconds",
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
    write_report(&staging, iteration, ordered_links.len(), &grouped_coverage)?;
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

type ClassifiedGroups = BTreeMap<String, LinkLabels>;
type GroupCoverage = (String, String, u64, usize, usize, usize, f64);

fn classify_links(links: &[&Link], network: &Network, settings: &Analysis) -> ClassifiedGroups {
    links
        .iter()
        .map(|link| {
            let supplied = settings.link_labels.get(link.id.external());
            let urban_area = if let Some(boundary) = &settings.urban_boundary {
                let from = network.nodes_with_ids().get(&link.from);
                let to = network.nodes_with_ids().get(&link.to);
                match (from, to) {
                    (Some(from), Some(to)) => {
                        Some(classify_link_to_boundary(from, to, boundary).to_owned())
                    }
                    _ => Some("unknown".to_owned()),
                }
            } else {
                label(supplied.and_then(|labels| labels.urban_area.as_deref()))
            };
            (
                link.id.external().to_owned(),
                LinkLabels {
                    urban_area,
                    road_type: label(supplied.and_then(|labels| labels.road_type.as_deref())),
                    road_size: label(supplied.and_then(|labels| labels.road_size.as_deref())),
                },
            )
        })
        .collect()
}

fn label(value: Option<&str>) -> Option<String> {
    Some(
        value
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("unknown")
            .to_owned(),
    )
}

fn classify_link_to_boundary(from: &Node, to: &Node, polygon: &[[f64; 2]]) -> &'static str {
    let from_inside = point_in_polygon(from.coord.x, from.coord.y, polygon);
    let to_inside = point_in_polygon(to.coord.x, to.coord.y, polygon);
    if from_inside && to_inside {
        "inner"
    } else if from_inside || to_inside || segment_crosses_polygon(from, to, polygon) {
        "cross_boundary"
    } else {
        "outer"
    }
}

fn point_in_polygon(x: f64, y: f64, polygon: &[[f64; 2]]) -> bool {
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let [x1, y1] = polygon[previous];
        let [x2, y2] = polygon[current];
        if point_on_segment(x, y, x1, y1, x2, y2) {
            return true;
        }
        if (y1 > y) != (y2 > y) && x < (x2 - x1) * (y - y1) / (y2 - y1) + x1 {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn segment_crosses_polygon(from: &Node, to: &Node, polygon: &[[f64; 2]]) -> bool {
    (0..polygon.len()).any(|index| {
        let [x1, y1] = polygon[index];
        let [x2, y2] = polygon[(index + 1) % polygon.len()];
        segments_intersect(
            from.coord.x,
            from.coord.y,
            to.coord.x,
            to.coord.y,
            x1,
            y1,
            x2,
            y2,
        )
    })
}

fn point_on_segment(x: f64, y: f64, x1: f64, y1: f64, x2: f64, y2: f64) -> bool {
    let cross = (x - x1) * (y2 - y1) - (y - y1) * (x2 - x1);
    cross == 0.0 && x >= x1.min(x2) && x <= x1.max(x2) && y >= y1.min(y2) && y <= y1.max(y2)
}

fn segments_intersect(
    ax: f64,
    ay: f64,
    bx: f64,
    by: f64,
    cx: f64,
    cy: f64,
    dx: f64,
    dy: f64,
) -> bool {
    fn orientation(ax: f64, ay: f64, bx: f64, by: f64, cx: f64, cy: f64) -> f64 {
        (bx - ax) * (cy - ay) - (by - ay) * (cx - ax)
    }
    let a = orientation(ax, ay, bx, by, cx, cy);
    let b = orientation(ax, ay, bx, by, dx, dy);
    let c = orientation(cx, cy, dx, dy, ax, ay);
    let d = orientation(cx, cy, dx, dy, bx, by);
    (a == 0.0 && point_on_segment(cx, cy, ax, ay, bx, by))
        || (b == 0.0 && point_on_segment(dx, dy, ax, ay, bx, by))
        || (c == 0.0 && point_on_segment(ax, ay, cx, cy, dx, dy))
        || (d == 0.0 && point_on_segment(bx, by, cx, cy, dx, dy))
        || ((a > 0.0) != (b > 0.0) && (c > 0.0) != (d > 0.0))
}

fn write_classification(
    path: &Path,
    links: &[&Link],
    classifications: &ClassifiedGroups,
) -> Result<(), AnalysisError> {
    let mut file =
        BufWriter::new(File::create(path.join("link_classification.csv")).map_err(io_error)?);
    writeln!(file, "link_id,urban_area,road_type,road_size").map_err(io_error)?;
    for link in links {
        let labels = &classifications[link.id.external()];
        writeln!(
            file,
            "{},{},{},{}",
            csv(link.id.external()),
            csv(labels.urban_area.as_deref().unwrap_or("unknown")),
            csv(labels.road_type.as_deref().unwrap_or("unknown")),
            csv(labels.road_size.as_deref().unwrap_or("unknown")),
        )
        .map_err(io_error)?;
    }
    Ok(())
}

fn write_group_coverage(
    path: &Path,
    links: &[&Link],
    classifications: &ClassifiedGroups,
    counts: &LinkVolumesByHour,
    interval: u32,
    simulation_end_time: u32,
) -> Result<Vec<GroupCoverage>, AnalysisError> {
    let mut eligible = BTreeMap::<(String, String), usize>::new();
    for link in links {
        let labels = &classifications[link.id.external()];
        for (dimension, category) in [
            (
                "urban_area",
                labels.urban_area.as_deref().unwrap_or("unknown"),
            ),
            (
                "road_type",
                labels.road_type.as_deref().unwrap_or("unknown"),
            ),
            (
                "road_size",
                labels.road_size.as_deref().unwrap_or("unknown"),
            ),
        ] {
            *eligible
                .entry((dimension.to_owned(), category.to_owned()))
                .or_default() += 1;
        }
    }
    let mut hours: BTreeSet<u64> = counts.keys().map(|key| key.hour_start_seconds).collect();
    hours.extend((0..u64::from(simulation_end_time)).step_by(interval as usize));
    hours.insert(0);
    let mut rows = Vec::new();
    for hour in hours {
        let mut used = BTreeMap::<(String, String), usize>::new();
        for link in links {
            if counts
                .get(&LinkHour {
                    hour_start_seconds: hour,
                    link_id: link.id.external().to_owned(),
                })
                .is_some_and(|volumes| volumes.entries + volumes.exits > 0)
            {
                let labels = &classifications[link.id.external()];
                for (dimension, category) in [
                    (
                        "urban_area",
                        labels.urban_area.as_deref().unwrap_or("unknown"),
                    ),
                    (
                        "road_type",
                        labels.road_type.as_deref().unwrap_or("unknown"),
                    ),
                    (
                        "road_size",
                        labels.road_size.as_deref().unwrap_or("unknown"),
                    ),
                ] {
                    *used
                        .entry((dimension.to_owned(), category.to_owned()))
                        .or_default() += 1;
                }
            }
        }
        for ((dimension, category), total) in &eligible {
            let used = used
                .get(&(dimension.clone(), category.clone()))
                .copied()
                .unwrap_or_default();
            let percent = used as f64 * 100.0 / *total as f64;
            rows.push((
                dimension.clone(),
                category.clone(),
                hour,
                *total,
                used,
                total - used,
                percent,
            ));
        }
    }
    let mut file = BufWriter::new(File::create(path.join("group_coverage.csv")).map_err(io_error)?);
    writeln!(
        file,
        "dimension,category,hour_start_seconds,eligible_links,used_links,unused_links,used_percent"
    )
    .map_err(io_error)?;
    for (dimension, category, hour, total, used, unused, percent) in &rows {
        writeln!(
            file,
            "{},{},{hour},{total},{used},{unused},{percent:.6}",
            csv(dimension),
            csv(category)
        )
        .map_err(io_error)?;
    }
    Ok(rows)
}

fn write_network_map(
    path: &Path,
    links: &[&Link],
    network: &Network,
    classifications: &ClassifiedGroups,
) -> Result<(), AnalysisError> {
    let nodes = network.nodes();
    let min_x = nodes
        .iter()
        .map(|node| node.coord.x)
        .fold(f64::INFINITY, f64::min);
    let max_x = nodes
        .iter()
        .map(|node| node.coord.x)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = nodes
        .iter()
        .map(|node| node.coord.y)
        .fold(f64::INFINITY, f64::min);
    let max_y = nodes
        .iter()
        .map(|node| node.coord.y)
        .fold(f64::NEG_INFINITY, f64::max);
    let width = (max_x - min_x).max(1.0);
    let height = (max_y - min_y).max(1.0);
    let project = |node: &Node| {
        let x = 20.0 + (node.coord.x - min_x) / width * 760.0;
        let y = 580.0 - (node.coord.y - min_y) / height * 560.0;
        (x, y)
    };
    let mut file = BufWriter::new(File::create(path.join("network_map.svg")).map_err(io_error)?);
    writeln!(file, "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 800 600\" role=\"img\" aria-label=\"Classified network map\"><rect width=\"800\" height=\"600\" fill=\"white\"/>").map_err(io_error)?;
    for link in links {
        let (Some(from), Some(to)) = (
            network.nodes_with_ids().get(&link.from),
            network.nodes_with_ids().get(&link.to),
        ) else {
            continue;
        };
        let (x1, y1) = project(from);
        let (x2, y2) = project(to);
        let labels = &classifications[link.id.external()];
        let road_type = labels.road_type.as_deref().unwrap_or("unknown");
        let color = match road_type {
            "expressway" => "#d94801",
            "unknown" => "#8a8f98",
            _ => "#1769aa",
        };
        let title = format!(
            "{} | {} | {} | {}",
            link.id.external(),
            labels.urban_area.as_deref().unwrap_or("unknown"),
            road_type,
            labels.road_size.as_deref().unwrap_or("unknown"),
        );
        writeln!(file, "<line x1=\"{x1:.2}\" y1=\"{y1:.2}\" x2=\"{x2:.2}\" y2=\"{y2:.2}\" stroke=\"{color}\" stroke-width=\"3\"><title>{}</title></line>", xml_escape(&title)).map_err(io_error)?;
    }
    writeln!(file, "</svg>").map_err(io_error)
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn write_tables(
    path: &Path,
    links: &[&Link],
    counts: &LinkVolumesByHour,
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
    Ok(())
}

fn write_report(
    path: &Path,
    iteration: u32,
    links: usize,
    grouped_coverage: &[GroupCoverage],
) -> Result<(), AnalysisError> {
    let coverage = fs::read_to_string(path.join("coverage.csv")).map_err(io_error)?;
    let coverage = json_for_script(&coverage.lines().collect::<Vec<_>>())?;
    let modules = fs::read_to_string(path.join("module_status.json")).map_err(io_error)?;
    let metrics = fs::read(path.join("metric_catalog.json")).map_err(io_error)?;
    let metrics: serde_json::Value =
        serde_json::from_slice(&metrics).map_err(|error| AnalysisError(error.to_string()))?;
    let metrics = json_for_script(&metrics)?;
    let hourly = fs::read_to_string(path.join("link_hourly.csv")).map_err(io_error)?;
    let hourly = json_for_script(&hourly.lines().collect::<Vec<_>>())?;
    let grouped_coverage = json_for_script(grouped_coverage)?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>body{{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}}table{{border-collapse:collapse;margin-bottom:2rem}}td,th{{border:1px solid #ccd;padding:.5rem}}a{{color:#075ea8}}img{{max-width:100%;border:1px solid #ccd}}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links.</p><h2>Classified network map</h2><img src=\"network_map.svg\" alt=\"Network links colored by road type; hover for all link classifications\"><h2>Coverage by group</h2><p>Urban area, road type, and road size are grouped independently. Missing labels are retained as unknown; geographic boundary crossings are explicit.</p><div id=\"groups\"></div><h2>Hourly volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end. Tables and module status are embedded for offline viewing.</p><h3>Per-link hourly entry and exit vehicles</h3><div id=\"hourly\"></div><h3>Hourly coverage</h3><div id=\"coverage\"></div><h2>Available metrics</h2><div id=\"metrics\"></div><h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_classification.csv\">link classifications (CSV)</a>, <a href=\"group_coverage.csv\">group coverage (CSV)</a>, <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>const h={hourly};const c={coverage};const g={grouped_coverage};const a={metrics};const m={modules};function table(root,headers,rows){{const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)}});const body=t.createTBody();rows.forEach(row=>{{const tr=body.insertRow();row.forEach(x=>{{const cell=tr.insertCell();cell.textContent=x}})}});root.appendChild(t)}}table(document.querySelector('#groups'),['Dimension','Group','Hour start (s)','Eligible','Used','Unused','Used (%)'],g);table(document.querySelector('#hourly'),h[0].split(','),h.slice(1).map(x=>x.split(',')));table(document.querySelector('#coverage'),c[0].split(','),c.slice(1).map(x=>x.split(',')));table(document.querySelector('#metrics'),['Metric','Unit','Aggregation key'],a.map(x=>[x.name,x.unit,x.aggregation_key]));table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))</script></body></html>"
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
}

fn json_for_script(value: &(impl Serialize + ?Sized)) -> Result<String, AnalysisError> {
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
