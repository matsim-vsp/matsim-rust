//! Final-iteration link coverage reporting.

use crate::simulation::config::{Analysis, CompressionType, LinkLabels};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id;
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network, Node};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::warn;

/// Published report of the latest completed iteration.
const ANALYSIS_DIR: &str = "analysis";
/// Report of the most recent failed attempt; never holds a completed index.
const FAILURE_DIR: &str = "analysis-failure";
const MANIFEST_FILE: &str = "manifest.json";
const RUN_METADATA_FILE: &str = "run_metadata.json";
const MODULE_STATUS_FILE: &str = "module_status.json";
const METRIC_CATALOG_FILE: &str = "metric_catalog.json";
const STAGING_DIR: &str = ".analysis-staging";
const FAILURE_STAGING_DIR: &str = ".analysis-failure-staging";
const BACKUP_DIR: &str = ".analysis-backup";
const FAILURE_BACKUP_DIR: &str = ".analysis-failure-backup";
const ID_STORE_FILE: &str = id::OUTPUT_FILE_NAME;

const STATUS_COMPLETE: &str = "complete";
const STATUS_FAILED: &str = "failed";
const STATUS_UNAVAILABLE: &str = "unavailable";

/// Module whose inputs must be readable for any report to be published.
const REQUIRED_MODULE: &str = "link_coverage";

/// Modules that stay unavailable until their inputs or implementations exist. They are reported
/// as such instead of failing the report.
const OPTIONAL_MODULES: &[(&str, &str)] = &[
    (
        "link_speed",
        "Traversal timing metrics are not implemented yet",
    ),
    (
        "agent_travel",
        "Observed leg and journey metrics are not implemented yet",
    ),
    (
        "validation",
        "No observed validation datasets are configured",
    ),
    ("cross_run_comparison", "No comparison runs are configured"),
    (
        "transit_and_research",
        "Optional module inputs are not configured",
    ),
];

const REPORT_STYLE: &str = "body{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;margin-bottom:2rem}td,th{border:1px solid #ccd;padding:.5rem}a{color:#075ea8}pre{background:#f4f6f9;border:1px solid #ccd;padding:1rem;overflow:auto}";

const MODULE_TABLE_SCRIPT: &str = "function table(root,headers,rows){const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)});const body=t.createTBody();rows.forEach(row=>{const tr=body.insertRow();row.forEach(x=>{const cell=tr.insertCell();cell.textContent=x})});root.replaceChildren(t)}table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))";

/// Complete report shell. Substituted in one pass by [`substitute_template`], so a link
/// label that happens to read like a token cannot corrupt the payloads.
const REPORT_TEMPLATE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>MATSim analysis</title><style>__REPORT_STYLE__label{margin-right:1rem}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration __ITERATION__; __LINKS__ eligible directed links in __INTERVAL__-second intervals.</p><h2>Final-run network coverage map</h2><p>Green links were used at least once in the final iteration; gray links were unused. Dashed links are expressways. Hover over a link for its classifications.</p><div id="map-container">__NETWORK_MAP__</div><h2>Coverage by group</h2><p>Urban area, road type, and road size are grouped independently. Missing labels are retained as unknown; geographic boundary crossings are explicit.</p><div id="groups"></div><h2>Hourly link metrics</h2><p>Filter on any combination of classifications to compare link volumes by group.</p><div id="filters"></div><div id="hourly"></div><h2>Hourly network coverage</h2><div id="coverage"></div><h2>Available metrics</h2><div id="metrics"></div><h2>Module status</h2><div id="modules"></div><p>Machine-readable data: <a href="network_map.svg">coverage map (SVG)</a>, <a href="link_classification.csv">link classifications (CSV)</a>, <a href="group_coverage.csv">group coverage (CSV)</a>, <a href="link_hourly.csv">link volumes (CSV)</a>, <a href="coverage.csv">coverage (CSV)</a>, <a href="run_metadata.json">expected travel and vehicle/PCE metadata (JSON)</a>, <a href="manifest.json">run manifest</a>, <a href="metric_catalog.json">metric catalog</a>.</p><script>const d=__LINK_HOURLY__;const c=__COVERAGE__;const a=__METRICS__;const m=__MODULES__;const D=__DIMENSIONS__;__MODULE_TABLE_SCRIPT__;table(document.querySelector('#coverage'),['hour_start_seconds','eligible_links','used_links','unused_links','used_percent'],c.slice(1).map(x=>x.split(',')));table(document.querySelector('#metrics'),['Metric','Unit','Aggregation key'],a.map(x=>[x.name,x.unit,x.aggregation_key]));const selectors=[];D.forEach(([key,title])=>{const label=document.createElement('label');label.textContent=title+' ';const select=document.createElement('select');select.append(new Option('All',''));[...new Set(d.map(x=>x[key]))].sort().forEach(value=>select.append(new Option(value,value)));label.append(select);document.querySelector('#filters').append(label);select.addEventListener('change',renderHourly);selectors.push([key,select])});function selectedRows(){return d.filter(row=>selectors.every(([key,select])=>select.value===''||row[key]===select.value))}function renderHourly(){const rows=selectedRows();table(document.querySelector('#hourly'),['link_id','hour_start_seconds','entry_vehicles','exit_vehicles','urban_area','road_type','road_size'],rows.map(row=>[row.link_id,row.hour_start_seconds,row.entry_vehicles,row.exit_vehicles,row.urban_area,row.road_type,row.road_size]));renderGroups(rows);updateMap()}function renderGroups(rows){const groups=new Map();rows.forEach(row=>D.map(([dimension])=>[dimension,row[dimension]]).forEach(([dimension,category])=>{const key=JSON.stringify([dimension,category,row.hour_start_seconds]);let group=groups.get(key);if(!group){group={dimension,category,hour:row.hour_start_seconds,eligible:0,used:0};groups.set(key,group)}group.eligible++;if(row.entry_vehicles+row.exit_vehicles>0)group.used++}));const values=[...groups.values()].map(group=>[group.dimension,group.category,group.hour,group.eligible,group.used,group.eligible-group.used,(group.used*100/group.eligible).toFixed(6)]);table(document.querySelector('#groups'),['Dimension','Group','Hour start (s)','Eligible','Used','Unused','Used (%)'],values)}function updateMap(){document.querySelectorAll('#network-map line').forEach(line=>{line.style.display=selectors.every(([key,select])=>select.value===''||line.getAttribute('data-'+key.replace('_','-'))===select.value)?'':'none'})}renderHourly()</script></body></html>"#;

#[derive(Debug)]
pub struct AnalysisError(String);

impl AnalysisError {
    pub fn new(message: impl Into<String>) -> Self {
        AnalysisError(message.into())
    }
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for AnalysisError {}

/// Reported category for a link whose label is absent or only whitespace.
const UNKNOWN: &str = "unknown";
/// Road-type label the coverage map renders as a dashed expressway.
const EXPRESSWAY: &str = "expressway";

#[derive(Serialize)]
struct Metric<'a> {
    name: &'a str,
    unit: &'a str,
    aggregation_key: &'a str,
}

/// Provenance of one published report attempt. Written into `manifest.json` and read back by
/// [`reanalyze_completed_run`] so a rerun reuses the recorded final iteration and run settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    /// `complete` for a published report, `failed` for a recorded failed attempt.
    status: String,
    /// Present only when `status` is `failed`.
    failure: Option<String>,
    iteration: u32,
    interval_seconds: u32,
    simulation_end_time: u32,
    partitions: Vec<u32>,
    input_format: String,
    eligible_links: usize,
    random_seed: u64,
    network_input: Option<String>,
    population_input: Option<String>,
    software_version: String,
    /// Classification inputs, recorded so [`reanalyze_completed_run`] rebuilds the same
    /// report. Absent in a manifest written before classifications existed, which reads
    /// back as "nothing labelled" rather than failing the rerun.
    #[serde(default)]
    link_labels: BTreeMap<String, LinkLabels>,
    #[serde(default)]
    urban_boundary: Option<Vec<[f64; 2]>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonExpectedTravel {
    person_id: String,
    legs: Vec<ExpectedLeg>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExpectedLeg {
    leg_index: usize,
    mode: String,
    departure_seconds: Option<f64>,
    expected_travel_seconds: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VehiclePce {
    vehicle_id: String,
    vehicle_type_id: String,
    pce: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VehicleTypePce {
    vehicle_type_id: String,
    pce: f64,
}

/// Input files a run was configured with. Recorded for provenance and for standalone reruns.
#[derive(Debug, Clone, Copy, Default)]
pub struct AnalysisInputPaths<'a> {
    pub network: Option<&'a Path>,
    /// Output network written next to the events; the eligible-link set a rerun must use.
    pub network_file: Option<&'a Path>,
    pub population: Option<&'a Path>,
    pub vehicles: Option<&'a Path>,
}

/// Compact run inputs an analysis pass needs but cannot recover from the event files. Serialized
/// to `run_metadata.json` so a standalone rerun reproduces the automatic run's metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisRunMetadata {
    random_seed: u64,
    network_input: Option<String>,
    network_file: Option<String>,
    population_input: Option<String>,
    vehicles_input: Option<String>,
    expected_travel: Vec<PersonExpectedTravel>,
    vehicles: Vec<VehiclePce>,
    vehicle_types: Vec<VehicleTypePce>,
}

impl AnalysisRunMetadata {
    /// Snapshot the live run's metadata without cloning the scenario: only the compact plan
    /// expectations and the vehicle/PCE catalog cross into the report. The expectations are
    /// taken by value because the run has finished and never reads them again.
    pub fn from_run(
        random_seed: u64,
        garage: &Garage,
        expected_travel: Vec<PersonExpectedTravel>,
        inputs: AnalysisInputPaths<'_>,
    ) -> Self {
        let mut vehicles: Vec<_> = garage
            .vehicles
            .values()
            .map(|vehicle| VehiclePce {
                vehicle_id: vehicle.id.external().to_owned(),
                vehicle_type_id: vehicle.vehicle_type.external().to_owned(),
                pce: vehicle.pce,
            })
            .collect();
        vehicles.sort_by(|a, b| a.vehicle_id.cmp(&b.vehicle_id));
        let mut vehicle_types: Vec<_> = garage
            .vehicle_types
            .values()
            .map(|vehicle_type| VehicleTypePce {
                vehicle_type_id: vehicle_type.id.external().to_owned(),
                pce: vehicle_type.pce,
            })
            .collect();
        vehicle_types.sort_by(|a, b| a.vehicle_type_id.cmp(&b.vehicle_type_id));
        AnalysisRunMetadata {
            random_seed,
            network_input: inputs.network.map(|path| path.display().to_string()),
            network_file: inputs.network_file.map(|path| path.display().to_string()),
            population_input: inputs.population.map(|path| path.display().to_string()),
            vehicles_input: inputs.vehicles.map(|path| path.display().to_string()),
            expected_travel,
            vehicles,
            vehicle_types,
        }
    }
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

#[derive(Serialize)]
struct ModuleStatus {
    module: &'static str,
    /// A failing required module fails the whole report; optional ones only report unavailability.
    required: bool,
    status: &'static str,
    reason: Option<String>,
}

/// Outcome of the required module. Optional modules are never failing; they are unavailable.
enum RequiredOutcome {
    Complete,
    Failed(String),
}

/// Replay every final-iteration partition and publish deterministic coverage tables and HTML.
///
/// A required-module failure records a failed report next to the last completed one and returns
/// the error, so a failed attempt never publishes a completed index over working output.
pub fn analyze_final_iteration(
    output_dir: &Path,
    iteration: u32,
    partitions: u32,
    compression: CompressionType,
    simulation_end_time: u32,
    run_metadata: &AnalysisRunMetadata,
    network: &Network,
    settings: &Analysis,
) -> Result<PathBuf, AnalysisError> {
    if !settings.enabled {
        return Err(AnalysisError::new("analysis is disabled"));
    }
    if settings.interval_seconds == 0 {
        return Err(AnalysisError::new(
            "analysis.interval_seconds must be greater than zero",
        ));
    }
    if let Some(boundary) = &settings.urban_boundary
        && (boundary.len() < 3
            || boundary
                .iter()
                .flatten()
                .any(|coordinate| !coordinate.is_finite()))
    {
        return Err(AnalysisError::new(
            "analysis.urban_boundary must contain at least three finite coordinates",
        ));
    }

    let ordered_links = sorted_links(network);
    let manifest = Manifest {
        status: STATUS_COMPLETE.to_owned(),
        failure: None,
        iteration,
        interval_seconds: settings.interval_seconds,
        simulation_end_time,
        partitions: (0..partitions).collect(),
        input_format: compression.extension().to_owned(),
        eligible_links: ordered_links.len(),
        random_seed: run_metadata.random_seed,
        network_input: run_metadata.network_input.clone(),
        population_input: run_metadata.population_input.clone(),
        software_version: env!("CARGO_PKG_VERSION").to_owned(),
        link_labels: settings.link_labels.clone(),
        urban_boundary: settings.urban_boundary.clone(),
    };

    // Required inputs are validated before anything is staged, so an unreadable recording is
    // reported as a failed attempt instead of replacing a previously published report.
    let counts = match replay_partitions(
        output_dir,
        iteration,
        partitions,
        compression,
        settings.interval_seconds,
        &ordered_links,
    ) {
        Ok(counts) => counts,
        Err(error) => return Err(record_failure(output_dir, &manifest, error)),
    };

    publish_complete(
        output_dir,
        &manifest,
        &ordered_links,
        &counts,
        settings.interval_seconds,
        simulation_end_time,
        run_metadata,
        network,
        settings,
    )
}

/// Regenerate the final-iteration report of a completed run from its recorded outputs.
///
/// Only the analysis outputs are rewritten; event files, plans, network and ID store are read but
/// left untouched. `interval_seconds` overrides the recorded interval width so analysis settings
/// can change without rerunning QSim; `None` reuses the width the recorded report used.
pub fn reanalyze_completed_run(
    output_dir: &Path,
    interval_seconds: Option<u32>,
) -> Result<PathBuf, AnalysisError> {
    // A crash between the two renames of a previous publish strands the last good report in the
    // backup. Reclaim it before anything else, so even a run that cannot be reanalyzed keeps its
    // report rather than leaving it next to a broken one.
    if let Err(error) = reclaim_backup(&output_dir.join(ANALYSIS_DIR), &output_dir.join(BACKUP_DIR))
    {
        warn!("Could not reclaim the previous analysis report: {error}");
    }
    let manifest_path = output_dir.join(ANALYSIS_DIR).join(MANIFEST_FILE);
    if !manifest_path.is_file() {
        return Err(AnalysisError(format!(
            "no recorded analysis manifest at {}: run the simulation with output.analysis.enabled before requesting a reanalysis",
            manifest_path.display()
        )));
    }
    let recorded: Manifest = read_json(&manifest_path)?;
    if recorded.partitions.is_empty() {
        return Err(record_failure(
            output_dir,
            &recorded,
            AnalysisError::new("recorded manifest does not list any event partition"),
        ));
    }
    let compression = match CompressionType::from_extension(&recorded.input_format) {
        Some(compression) => compression,
        None => {
            return Err(record_failure(
                output_dir,
                &recorded,
                AnalysisError(format!(
                    "unsupported recorded input format: {}",
                    recorded.input_format
                )),
            ));
        }
    };

    let run_metadata: AnalysisRunMetadata =
        match read_json(&output_dir.join(ANALYSIS_DIR).join(RUN_METADATA_FILE)) {
            Ok(metadata) => metadata,
            Err(error) => return Err(record_failure(output_dir, &recorded, error)),
        };
    let Some(network_file) = run_metadata.network_file.as_ref() else {
        return Err(record_failure(
            output_dir,
            &recorded,
            AnalysisError::new("recorded run metadata does not name an output network file"),
        ));
    };
    let network_path = output_dir.join(network_file);
    if !network_path.is_file() {
        return Err(record_failure(
            output_dir,
            &recorded,
            AnalysisError(format!(
                "missing recorded output network: {}",
                network_path.display()
            )),
        ));
    }
    // Restoring the run's ID mapping keeps rerun link identifiers consistent with the recorded
    // events; runs that stored no ID store simply build one from the output network.
    let id_store = output_dir.join(ID_STORE_FILE);
    if id_store.is_file() {
        id::load_from_file(&id_store);
    }
    let network = Network::from_file_as_is(&network_path);
    let settings = Analysis {
        enabled: true,
        interval_seconds: interval_seconds.unwrap_or(recorded.interval_seconds),
        link_labels: recorded.link_labels.clone(),
        urban_boundary: recorded.urban_boundary.clone(),
    };

    analyze_final_iteration(
        output_dir,
        recorded.iteration,
        recorded.partitions.len() as u32,
        compression,
        recorded.simulation_end_time,
        &run_metadata,
        &network,
        &settings,
    )
}

/// Record a failed attempt and hand the original error back to the caller. The module error is
/// always the one returned; a secondary failure to write the diagnostics is logged, not dropped.
fn record_failure(output_dir: &Path, recorded: &Manifest, error: AnalysisError) -> AnalysisError {
    if let Err(write) = publish_failure(output_dir, recorded, &error) {
        warn!("Could not record the analysis failure report: {write}");
    }
    error
}

fn sorted_links(network: &Network) -> Vec<&Link> {
    let mut ordered_links: Vec<_> = network.links().into_iter().collect();
    ordered_links.sort_by(|a, b| a.id.external().cmp(b.id.external()));
    ordered_links
}

fn replay_partitions(
    output_dir: &Path,
    iteration: u32,
    partitions: u32,
    compression: CompressionType,
    interval: u32,
    ordered_links: &[&Link],
) -> Result<LinkVolumesByHour, AnalysisError> {
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
        accumulate(event.as_ref(), time, interval, &ids, &mut counts);
        heads[rank] = readers[rank].next_event()?;
    }
    Ok(counts)
}

fn publish_complete(
    output_dir: &Path,
    manifest: &Manifest,
    ordered_links: &[&Link],
    counts: &LinkVolumesByHour,
    interval: u32,
    simulation_end_time: u32,
    run_metadata: &AnalysisRunMetadata,
    network: &Network,
    settings: &Analysis,
) -> Result<PathBuf, AnalysisError> {
    let staging = output_dir.join(STAGING_DIR);
    reset_staging(&staging)?;
    let classifications = classify_links(ordered_links, network, settings);
    let link_hourly = link_hourly_metrics(
        ordered_links,
        &classifications,
        counts,
        interval,
        simulation_end_time,
    );
    write_tables(
        &staging,
        ordered_links,
        counts,
        interval,
        simulation_end_time,
        &link_hourly,
    )?;
    write_classification(&staging, ordered_links, &classifications)?;
    write_group_coverage(
        &staging,
        ordered_links,
        &classifications,
        counts,
        interval,
        simulation_end_time,
    )?;
    write_network_map(&staging, ordered_links, network, &classifications, counts)?;
    write_json(&staging.join(RUN_METADATA_FILE), run_metadata)?;
    write_json(&staging.join(METRIC_CATALOG_FILE), &metrics())?;
    let statuses = module_statuses(&RequiredOutcome::Complete);
    write_json(&staging.join(MODULE_STATUS_FILE), &statuses)?;
    write_json(&staging.join(MANIFEST_FILE), manifest)?;
    write_report(&staging, manifest, &statuses, &link_hourly)?;
    let published = publish(
        &staging,
        &output_dir.join(ANALYSIS_DIR),
        &output_dir.join(BACKUP_DIR),
    )?;
    // A complete report supersedes any failure recorded by an earlier attempt.
    let failure_dir = output_dir.join(FAILURE_DIR);
    if failure_dir.exists() {
        fs::remove_dir_all(&failure_dir).map_err(io_error)?;
    }
    Ok(published.join("index.html"))
}

/// Publish the diagnostics of a failed attempt. The completed report in [`ANALYSIS_DIR`] is never
/// touched, so a failed rerun cannot be mistaken for a completed index.
fn publish_failure(
    output_dir: &Path,
    manifest: &Manifest,
    error: &AnalysisError,
) -> Result<(), AnalysisError> {
    let mut failed = manifest.clone();
    failed.status = STATUS_FAILED.to_owned();
    failed.failure = Some(error.to_string());
    let statuses = module_statuses(&RequiredOutcome::Failed(error.to_string()));
    let staging = output_dir.join(FAILURE_STAGING_DIR);
    reset_staging(&staging)?;
    write_json(&staging.join(MANIFEST_FILE), &failed)?;
    write_json(&staging.join(MODULE_STATUS_FILE), &statuses)?;
    fs::write(staging.join("failure.txt"), format!("{error}\n")).map_err(io_error)?;
    write_failure_report(&staging, &failed, &statuses)?;
    publish(
        &staging,
        &output_dir.join(FAILURE_DIR),
        &output_dir.join(FAILURE_BACKUP_DIR),
    )?;
    Ok(())
}

fn module_statuses(outcome: &RequiredOutcome) -> Vec<ModuleStatus> {
    let (status, reason) = match outcome {
        RequiredOutcome::Complete => (STATUS_COMPLETE, None),
        RequiredOutcome::Failed(reason) => (STATUS_FAILED, Some(reason.clone())),
    };
    let mut statuses = vec![ModuleStatus {
        module: REQUIRED_MODULE,
        required: true,
        status,
        reason,
    }];
    statuses.extend(
        OPTIONAL_MODULES
            .iter()
            .map(|(module, reason)| ModuleStatus {
                module,
                required: false,
                status: STATUS_UNAVAILABLE,
                reason: Some((*reason).to_owned()),
            }),
    );
    statuses
}

fn metrics() -> [Metric<'static>; 9] {
    [
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
    ]
}

/// Swap a fully staged directory into place.
fn publish(staging: &Path, published: &Path, backup: &Path) -> Result<PathBuf, AnalysisError> {
    reclaim_backup(published, backup)?;
    let had_published = published.exists();
    if had_published {
        fs::rename(published, backup).map_err(io_error)?;
    }
    if let Err(error) = fs::rename(staging, published) {
        if had_published {
            if let Err(restore) = fs::rename(backup, published) {
                warn!(
                    "Could not restore the previous report from {}: {restore}",
                    backup.display()
                );
            }
        }
        return Err(io_error(error));
    }
    if had_published {
        fs::remove_dir_all(backup).map_err(io_error)?;
    }
    Ok(published.to_path_buf())
}

/// Resolve a backup left behind by an interrupted publish: restore it when its published
/// counterpart is gone, and drop it when the publish did land. Without this an interrupted publish
/// would strand the last good report in the backup.
fn reclaim_backup(published: &Path, backup: &Path) -> Result<(), AnalysisError> {
    if backup.exists() {
        if published.exists() {
            fs::remove_dir_all(backup).map_err(io_error)?;
        } else {
            fs::rename(backup, published).map_err(io_error)?;
        }
    }
    Ok(())
}

/// A staging directory left behind by an interrupted run is never a usable report.
fn reset_staging(staging: &Path) -> Result<(), AnalysisError> {
    if staging.exists() {
        fs::remove_dir_all(staging).map_err(io_error)?;
    }
    fs::create_dir_all(staging).map_err(io_error)
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, AnalysisError> {
    let bytes = fs::read(path)
        .map_err(|error| AnalysisError(format!("cannot read {}: {error}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| AnalysisError(format!("cannot parse {}: {error}", path.display())))
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), AnalysisError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|e| AnalysisError(e.to_string()))?;
    fs::write(path, bytes).map_err(io_error)
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

/// A link's three classification dimensions, always populated.
///
/// `classify_links` resolves blank and missing labels to `unknown` once, so the
/// exporters and the report never repeat that fallback and cannot disagree about
/// which group a link belongs to.
#[derive(Serialize)]
struct ClassifiedLink {
    urban_area: String,
    road_type: String,
    road_size: String,
}

impl ClassifiedLink {
    /// Dimension key and value pairs, in `FILTER_DIMENSIONS` order.
    fn dimensions(&self) -> [(&'static str, &str); FILTER_DIMENSIONS.len()] {
        [
            ("urban_area", self.urban_area.as_str()),
            ("road_type", self.road_type.as_str()),
            ("road_size", self.road_size.as_str()),
        ]
    }
}

/// Classified dimensions for every eligible link, keyed by external link ID.
type LinkClassifications = BTreeMap<String, ClassifiedLink>;

/// The report's classification dimensions, with their column and filter headings.
///
/// This is the single list the CSV exporters group by and the HTML filters
/// offer, so a dimension cannot be exported without also being filterable. The
/// report test asserts the same keys appear in every emitted artefact.
const FILTER_DIMENSIONS: [(&str, &str); 3] = [
    ("urban_area", "Urban area"),
    ("road_type", "Road type"),
    ("road_size", "Road size"),
];

#[derive(Serialize)]
struct LinkHourlyMetric {
    link_id: String,
    hour_start_seconds: u64,
    entry_vehicles: u64,
    exit_vehicles: u64,
    urban_area: String,
    road_type: String,
    road_size: String,
}

/// Start second of every reported interval: the buckets that carry traffic, the
/// empty ones up to the simulation end, and zero.
fn interval_starts(
    counts: &LinkVolumesByHour,
    interval: u32,
    simulation_end_time: u32,
) -> BTreeSet<u64> {
    let mut hours: BTreeSet<_> = counts.keys().map(|key| key.hour_start_seconds).collect();
    hours.extend((0..u64::from(simulation_end_time)).step_by(interval as usize));
    hours.insert(0);
    hours
}

fn classify_links(links: &[&Link], network: &Network, settings: &Analysis) -> LinkClassifications {
    links
        .iter()
        .map(|link| {
            let supplied = settings.link_labels.get(link.id.external());
            let urban_area = match &settings.urban_boundary {
                Some(boundary) => {
                    let from = network.nodes_with_ids().get(&link.from);
                    let to = network.nodes_with_ids().get(&link.to);
                    match (from, to) {
                        (Some(from), Some(to)) => {
                            classify_link_to_boundary(from, to, boundary).to_owned()
                        }
                        _ => UNKNOWN.to_owned(),
                    }
                }
                None => label(supplied.and_then(|labels| labels.urban_area.as_deref())),
            };
            (
                link.id.external().to_owned(),
                ClassifiedLink {
                    urban_area,
                    road_type: label(supplied.and_then(|labels| labels.road_type.as_deref())),
                    road_size: label(supplied.and_then(|labels| labels.road_size.as_deref())),
                },
            )
        })
        .collect()
}

/// A supplied label, or `unknown` when it is absent or only whitespace. Resolved
/// once here so that grouping, the exports and the report filters agree.
fn label(value: Option<&str>) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(UNKNOWN)
        .to_owned()
}

fn classify_link_to_boundary(from: &Node, to: &Node, polygon: &[[f64; 2]]) -> &'static str {
    let from_inside = point_in_polygon([from.coord.x, from.coord.y], polygon);
    let to_inside = point_in_polygon([to.coord.x, to.coord.y], polygon);
    if from_inside && to_inside {
        "inner"
    } else if from_inside || to_inside || segment_crosses_polygon(from, to, polygon) {
        "cross_boundary"
    } else {
        "outer"
    }
}

/// Relative tolerance for calling a point collinear with a segment.
///
/// Boundary assignment is report metadata, so a node that lands on the polygon
/// edge only up to floating-point rounding still counts as on it. The bound is
/// scaled by the coordinates involved, which keeps it far below any real link
/// length in both metres and degrees while absorbing the rounding of a
/// coordinate that was itself computed from an arithmetic expression.
const BOUNDARY_TOLERANCE: f64 = 1e-9;

fn orientation(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Distance tolerance for `point` against segment `a`-`b`, scaled to the
/// coordinates involved so it means the same thing in any unit.
fn boundary_tolerance(a: [f64; 2], b: [f64; 2], point: [f64; 2]) -> f64 {
    BOUNDARY_TOLERANCE
        * [a[0], a[1], b[0], b[1], point[0], point[1]]
            .iter()
            .map(|coordinate| coordinate.abs())
            .fold(1.0_f64, f64::max)
}

fn collinear(a: [f64; 2], b: [f64; 2], point: [f64; 2]) -> bool {
    let tolerance = boundary_tolerance(a, b, point);
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    if dx == 0.0 && dy == 0.0 {
        // A degenerate segment only contains its own endpoints.
        return (point[0] - a[0]).abs() <= tolerance && (point[1] - a[1]).abs() <= tolerance;
    }
    // Divide the cross-product area by the segment length to compare a distance
    // rather than an area, so the same tolerance reads the same in any unit.
    orientation(a, b, point).abs() / dx.hypot(dy) <= tolerance
}

fn point_in_polygon(point: [f64; 2], polygon: &[[f64; 2]]) -> bool {
    // Crossing-number test: a horizontal ray from the point toggles `inside` once
    // per edge it passes through, and only edges that straddle the point's y can
    // pass through that ray. The `first[1] != second[1]` gap implied by the
    // straddle test is what keeps the x-intercept division below well defined.
    let mut inside = false;
    let mut previous = polygon.len() - 1;
    for current in 0..polygon.len() {
        let first = polygon[previous];
        let second = polygon[current];
        if point_on_segment(point, first, second) {
            return true;
        }
        if (first[1] > point[1]) != (second[1] > point[1])
            && point[0]
                < (second[0] - first[0]) * (point[1] - first[1]) / (second[1] - first[1]) + first[0]
        {
            inside = !inside;
        }
        previous = current;
    }
    inside
}

fn segment_crosses_polygon(from: &Node, to: &Node, polygon: &[[f64; 2]]) -> bool {
    (0..polygon.len()).any(|index| {
        segments_intersect(
            [from.coord.x, from.coord.y],
            [to.coord.x, to.coord.y],
            polygon[index],
            polygon[(index + 1) % polygon.len()],
        )
    })
}

fn point_on_segment(point: [f64; 2], first: [f64; 2], second: [f64; 2]) -> bool {
    // Collinearity alone is not enough: an infinite line has to be clipped to the
    // segment's bounding box before the point counts as lying on it. The clamp
    // uses the same tolerance, because a point that misses the edge by a rounding
    // step misses the bounding box by that same step.
    let tolerance = boundary_tolerance(first, second, point);
    collinear(first, second, point)
        && point[0] >= first[0].min(second[0]) - tolerance
        && point[0] <= first[0].max(second[0]) + tolerance
        && point[1] >= first[1].min(second[1]) - tolerance
        && point[1] <= first[1].max(second[1]) + tolerance
}

fn segments_intersect(a: [f64; 2], b: [f64; 2], c: [f64; 2], d: [f64; 2]) -> bool {
    // Either an endpoint of one segment lies on the other (touching or collinear
    // overlap), or the two segments strictly straddle each other's supporting
    // line. Both are needed: collinear overlap leaves the sign test below
    // unsatisfied, and a strict crossing satisfies none of the endpoint tests.
    let ab_c = orientation(a, b, c);
    let ab_d = orientation(a, b, d);
    let cd_a = orientation(c, d, a);
    let cd_b = orientation(c, d, b);
    point_on_segment(c, a, b)
        || point_on_segment(d, a, b)
        || point_on_segment(a, c, d)
        || point_on_segment(b, c, d)
        || ((ab_c > 0.0) != (ab_d > 0.0) && (cd_a > 0.0) != (cd_b > 0.0))
}

fn write_classification(
    path: &Path,
    links: &[&Link],
    classifications: &LinkClassifications,
) -> Result<(), AnalysisError> {
    let mut file =
        BufWriter::new(File::create(path.join("link_classification.csv")).map_err(io_error)?);
    writeln!(
        file,
        "link_id,{}",
        FILTER_DIMENSIONS
            .iter()
            .map(|(key, _)| csv(key))
            .collect::<Vec<_>>()
            .join(",")
    )
    .map_err(io_error)?;
    for link in links {
        let classified = &classifications[link.id.external()];
        writeln!(
            file,
            "{},{}",
            csv(link.id.external()),
            classified
                .dimensions()
                .iter()
                .map(|(_, value)| csv(value))
                .collect::<Vec<_>>()
                .join(",")
        )
        .map_err(io_error)?;
    }
    Ok(())
}

fn write_group_coverage(
    path: &Path,
    links: &[&Link],
    classifications: &LinkClassifications,
    counts: &LinkVolumesByHour,
    interval: u32,
    simulation_end_time: u32,
) -> Result<(), AnalysisError> {
    let mut eligible = BTreeMap::<(String, String), usize>::new();
    for link in links {
        let classified = &classifications[link.id.external()];
        for (dimension, category) in classified.dimensions() {
            *eligible
                .entry((dimension.to_owned(), category.to_owned()))
                .or_default() += 1;
        }
    }
    let mut file = BufWriter::new(File::create(path.join("group_coverage.csv")).map_err(io_error)?);
    writeln!(
        file,
        "dimension,category,hour_start_seconds,eligible_links,used_links,unused_links,used_percent"
    )
    .map_err(io_error)?;
    for hour in interval_starts(counts, interval, simulation_end_time) {
        let mut used = BTreeMap::<(String, String), usize>::new();
        for link in links {
            if counts
                .get(&LinkHour {
                    hour_start_seconds: hour,
                    link_id: link.id.external().to_owned(),
                })
                .is_some_and(|volumes| volumes.entries + volumes.exits > 0)
            {
                let classified = &classifications[link.id.external()];
                for (dimension, category) in classified.dimensions() {
                    *used
                        .entry((dimension.to_owned(), category.to_owned()))
                        .or_default() += 1;
                }
            }
        }
        // Eligible denominators stay fixed per category; only the used count varies by hour.
        for ((dimension, category), total) in &eligible {
            let used = used
                .get(&(dimension.clone(), category.clone()))
                .copied()
                .unwrap_or_default();
            let percent = used as f64 * 100.0 / *total as f64;
            writeln!(
                file,
                "{},{},{hour},{total},{used},{},{percent:.6}",
                csv(dimension),
                csv(category),
                total - used,
            )
            .map_err(io_error)?;
        }
    }
    Ok(())
}

fn write_network_map(
    path: &Path,
    links: &[&Link],
    network: &Network,
    classifications: &LinkClassifications,
    counts: &LinkVolumesByHour,
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
    let used_links: BTreeSet<_> = counts
        .iter()
        .filter(|(_, volumes)| volumes.entries + volumes.exits > 0)
        .map(|(key, _)| key.link_id.as_str())
        .collect();
    let mut file = BufWriter::new(File::create(path.join("network_map.svg")).map_err(io_error)?);
    writeln!(file, "<svg id=\"network-map\" xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 800 600\" role=\"img\" aria-label=\"Classified network map\" style=\"width:100%;height:auto;max-height:600px\"><rect width=\"800\" height=\"600\" fill=\"white\"/>").map_err(io_error)?;
    for link in links {
        let (Some(from), Some(to)) = (
            network.nodes_with_ids().get(&link.from),
            network.nodes_with_ids().get(&link.to),
        ) else {
            continue;
        };
        let (x1, y1) = project(from);
        let (x2, y2) = project(to);
        // A non-finite node coordinate projects to `NaN`, which browsers drop,
        // leaving a silently absent link. Skip it rather than emit broken markup.
        if ![x1, y1, x2, y2].into_iter().all(f64::is_finite) {
            continue;
        }
        let classified = &classifications[link.id.external()];
        let used = used_links.contains(link.id.external());
        let color = if used { "#287a3d" } else { "#c8ccd0" };
        let road_style = if classified.road_type == EXPRESSWAY {
            " stroke-dasharray=\"8 3\""
        } else {
            ""
        };
        let title = format!(
            "{} | {} | {} | {} | {}",
            link.id.external(),
            classified.urban_area,
            classified.road_type,
            classified.road_size,
            if used { "used" } else { "unused" },
        );
        // One `data-` attribute per dimension, so the report's filters can hide
        // links without re-parsing the title text.
        let data_attributes = FILTER_DIMENSIONS
            .iter()
            .zip(classified.dimensions())
            .map(|((key, _), (_, value))| {
                format!("data-{}=\"{}\"", key.replace('_', "-"), xml_escape(value))
            })
            .collect::<Vec<_>>()
            .join(" ");
        writeln!(file, "<line x1=\"{x1:.2}\" y1=\"{y1:.2}\" x2=\"{x2:.2}\" y2=\"{y2:.2}\" {data_attributes} stroke=\"{color}\" stroke-width=\"3\"{road_style}><title>{}</title></line>", xml_escape(&title)).map_err(io_error)?;
    }
    writeln!(file, "</svg>").map_err(io_error)
}

fn link_hourly_metrics(
    links: &[&Link],
    classifications: &LinkClassifications,
    counts: &LinkVolumesByHour,
    interval: u32,
    simulation_end_time: u32,
) -> Vec<LinkHourlyMetric> {
    interval_starts(counts, interval, simulation_end_time)
        .into_iter()
        .flat_map(|hour| {
            links.iter().map(move |link| {
                let classified = &classifications[link.id.external()];
                let volumes = counts
                    .get(&LinkHour {
                        hour_start_seconds: hour,
                        link_id: link.id.external().to_owned(),
                    })
                    .copied()
                    .unwrap_or_default();
                LinkHourlyMetric {
                    link_id: link.id.external().to_owned(),
                    hour_start_seconds: hour,
                    entry_vehicles: volumes.entries,
                    exit_vehicles: volumes.exits,
                    urban_area: classified.urban_area.clone(),
                    road_type: classified.road_type.clone(),
                    road_size: classified.road_size.clone(),
                }
            })
        })
        .collect()
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
    link_hourly: &[LinkHourlyMetric],
) -> Result<(), AnalysisError> {
    let mut hourly = BufWriter::new(File::create(path.join("link_hourly.csv")).map_err(io_error)?);
    writeln!(
        hourly,
        "link_id,hour_start_seconds,entry_vehicles,exit_vehicles"
    )
    .map_err(io_error)?;
    for row in link_hourly {
        writeln!(
            hourly,
            "{},{},{},{}",
            csv(&row.link_id),
            row.hour_start_seconds,
            row.entry_vehicles,
            row.exit_vehicles,
        )
        .map_err(io_error)?;
    }
    let mut coverage = BufWriter::new(File::create(path.join("coverage.csv")).map_err(io_error)?);
    writeln!(
        coverage,
        "hour_start_seconds,eligible_links,used_links,unused_links,used_percent"
    )
    .map_err(io_error)?;
    for hour in interval_starts(counts, interval, simulation_end_time) {
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
    manifest: &Manifest,
    statuses: &[ModuleStatus],
    link_hourly: &[LinkHourlyMetric],
) -> Result<(), AnalysisError> {
    let coverage = fs::read_to_string(path.join("coverage.csv")).map_err(io_error)?;
    let coverage = json_for_script(&coverage.lines().collect::<Vec<_>>())?;
    let modules = json_for_script(statuses)?;
    let metrics = json_for_script(&metrics())?;
    let hourly = json_for_script(link_hourly)?;
    // The filter list comes from the same constant the CSV exporters group by, so a
    // dimension cannot be exported without also being offered as a filter.
    let dimensions = json_for_script(&FILTER_DIMENSIONS)?;
    let network_map = fs::read_to_string(path.join("network_map.svg")).map_err(io_error)?;
    let html = substitute_template(
        REPORT_TEMPLATE,
        &[
            ("__ITERATION__", &manifest.iteration.to_string()),
            ("__LINKS__", &manifest.eligible_links.to_string()),
            ("__INTERVAL__", &manifest.interval_seconds.to_string()),
            ("__REPORT_STYLE__", REPORT_STYLE),
            ("__MODULE_TABLE_SCRIPT__", MODULE_TABLE_SCRIPT),
            ("__NETWORK_MAP__", &network_map),
            ("__DIMENSIONS__", &dimensions),
            ("__LINK_HOURLY__", &hourly),
            ("__COVERAGE__", &coverage),
            ("__METRICS__", &metrics),
            ("__MODULES__", &modules),
        ],
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
}

/// Fill `template` by scanning it once, left to right.
///
/// A chained `str::replace` would rescan text it had already substituted, so a
/// replacement value that happens to contain another token (a link label reading
/// `__METRICS__`, say) would be rewritten by a later step and corrupt the
/// payload. Advancing past the token after one substitution leaves inserted text
/// untouched. Byte indexing is safe here because the cursor only ever lands on a
/// `char` boundary.
fn substitute_template(template: &str, replacements: &[(&str, &str)]) -> String {
    let mut output = String::with_capacity(template.len());
    let mut cursor = 0;
    while cursor < template.len() {
        if let Some((token, value)) = replacements
            .iter()
            .find(|(token, _)| template[cursor..].starts_with(token))
        {
            output.push_str(value);
            cursor += token.len();
        } else {
            let character = template[cursor..]
                .chars()
                .next()
                .expect("cursor is within the template");
            output.push(character);
            cursor += character.len_utf8();
        }
    }
    output
}

fn write_failure_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &[ModuleStatus],
) -> Result<(), AnalysisError> {
    let modules = json_for_script(statuses)?;
    let reason = escape_html(manifest.failure.as_deref().unwrap_or_default());
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis failed</title><style>{REPORT_STYLE}</style></head><body><h1>Analysis failed</h1><p>The required <code>{REQUIRED_MODULE}</code> module did not complete for final iteration {iteration}, so no completed report was published. The previously published report in <code>{ANALYSIS_DIR}</code> is unchanged.</p><h2>Failure</h2><pre>{reason}</pre><h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"manifest.json\">failure manifest</a>, <a href=\"module_status.json\">module status</a>, <a href=\"failure.txt\">error text</a>.</p><script>const m={modules};{MODULE_TABLE_SCRIPT}</script></body></html>",
        iteration = manifest.iteration,
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

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn io_error(error: std::io::Error) -> AnalysisError {
    AnalysisError(error.to_string())
}
