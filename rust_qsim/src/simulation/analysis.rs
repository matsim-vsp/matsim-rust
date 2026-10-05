//! Final-iteration link coverage and capacity reporting.

pub mod capacity;

use crate::simulation::config::{Analysis, CompressionType};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id;
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;
use capacity::{
    FlowSide, IntervalVolumes, LinkUtilization, VC_BIN_COUNT, VcHistogram, covered_interval_hours,
    vc_bin_bounds,
};
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

const MODULE_TABLE_SCRIPT: &str = "function table(root,headers,rows){const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)});const body=t.createTBody();rows.forEach(row=>{const tr=body.insertRow();row.forEach(x=>{const cell=tr.insertCell();cell.textContent=x})});root.appendChild(t)}table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))";

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
    /// Simulated fraction of the population the volumes were scaled up from.
    sample_size: f64,
    network_input: Option<String>,
    population_input: Option<String>,
    software_version: String,
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
    /// Simulated fraction of the population. Observed volumes are scaled up by its reciprocal
    /// before being compared with network capacity, so a rerun has to scale the same way.
    sample_size: f64,
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
        sample_size: f64,
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
            sample_size,
            network_input: inputs.network.map(|path| path.display().to_string()),
            network_file: inputs.network_file.map(|path| path.display().to_string()),
            population_input: inputs.population.map(|path| path.display().to_string()),
            vehicles_input: inputs.vehicles.map(|path| path.display().to_string()),
            expected_travel,
            vehicles,
            vehicle_types,
        }
    }

    /// Simulated fraction of the population the observed volumes were scaled up from.
    pub fn sample_size(&self) -> f64 {
        self.sample_size
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

/// Observed volumes per link and interval, keyed by interval start and then by
/// link id.
///
/// The nested map lets the table writers look a link up by `&str` instead of
/// building an owned key for every link and interval, which is the bulk of the
/// work once the tables are written.
type LinkVolumesByHour = BTreeMap<u64, BTreeMap<String, IntervalVolumes>>;

/// The observed volumes of one link in one interval, defaulting to no traffic.
fn volumes_of(
    counts: &LinkVolumesByHour,
    interval_start_seconds: u64,
    link_id: &str,
) -> IntervalVolumes {
    counts
        .get(&interval_start_seconds)
        .and_then(|links| links.get(link_id))
        .copied()
        .unwrap_or_default()
}

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
    // Observed volumes are scaled up by the reciprocal of the sample size, so a run
    // without a usable fraction cannot produce a report at all.
    let sample_size = run_metadata.sample_size();
    if !sample_size.is_finite() || sample_size <= 0.0 {
        return Err(AnalysisError::new(format!(
            "sample size must be a positive finite number to scale volumes, got {sample_size}"
        )));
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
        // Recorded so the report says which fraction the volumes were scaled up from.
        sample_size,
        network_input: run_metadata.network_input.clone(),
        population_input: run_metadata.population_input.clone(),
        software_version: env!("CARGO_PKG_VERSION").to_owned(),
    };

    // The recorded vehicle catalog is the only PCE source, so a standalone rerun weights
    // vehicles exactly like the run it reproduces.
    let pce_by_vehicle: BTreeMap<&str, f64> = run_metadata
        .vehicles
        .iter()
        .map(|vehicle| (vehicle.vehicle_id.as_str(), vehicle.pce))
        .collect();
    // Required inputs are validated before anything is staged, so an unreadable recording is
    // reported as a failed attempt instead of replacing a previously published report.
    let counts = match replay_partitions(
        output_dir,
        iteration,
        partitions,
        compression,
        settings.interval_seconds,
        &ordered_links,
        &pce_by_vehicle,
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
    pce_by_vehicle: &BTreeMap<&str, f64>,
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
        accumulate(
            event.as_ref(),
            time,
            interval,
            &ids,
            pce_by_vehicle,
            &mut counts,
        );
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
) -> Result<PathBuf, AnalysisError> {
    let staging = output_dir.join(STAGING_DIR);
    reset_staging(&staging)?;
    write_tables(
        &staging,
        ordered_links,
        counts,
        interval,
        simulation_end_time,
        // The same accessor the validation used, so the scale that was checked and
        // the scale that is written can never disagree.
        run_metadata.sample_size(),
    )?;
    write_json(&staging.join(RUN_METADATA_FILE), run_metadata)?;
    write_json(&staging.join(METRIC_CATALOG_FILE), &metrics())?;
    let statuses = module_statuses(&RequiredOutcome::Complete);
    write_json(&staging.join(MODULE_STATUS_FILE), &statuses)?;
    write_json(&staging.join(MANIFEST_FILE), manifest)?;
    write_report(&staging, manifest, &statuses)?;
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

/// Every metric the report exports, named after the column that carries it, so a consumer of the
/// catalog can look a metric up in the table that describes it.
fn metrics() -> Vec<Metric<'static>> {
    vec![
        Metric {
            name: "entry_vehicles",
            unit: "vehicles",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_vehicles",
            unit: "vehicles",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "eligible_links",
            unit: "links",
            aggregation_key: "interval_start_seconds",
        },
        Metric {
            name: "used_links",
            unit: "links",
            aggregation_key: "interval_start_seconds",
        },
        // unused_links is a column of both coverage.csv and vc_histogram.csv; the
        // histogram groups it by metric, the coverage table does not.
        Metric {
            name: "unused_links",
            unit: "links",
            aggregation_key: "interval_start_seconds",
        },
        Metric {
            name: "used_percent",
            unit: "percent",
            aggregation_key: "interval_start_seconds",
        },
        // Every name below matches a column header of link_capacity.csv or
        // vc_histogram.csv, so a consumer of the catalog can look each metric up by
        // name in the table that describes it.
        Metric {
            name: "capacity_pce_per_hour",
            unit: "pce_per_hour",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "effective_capacity_pce",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "entry_pce",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_pce",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "entry_pce_scaled",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_pce_scaled",
            unit: "pce",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "entry_flow_pce_per_hour",
            unit: "pce_per_hour",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_flow_pce_per_hour",
            unit: "pce_per_hour",
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
            name: "entry_unresolved_pce",
            unit: "vehicles",
            aggregation_key: "link_id,interval_start_seconds",
        },
        Metric {
            name: "exit_unresolved_pce",
            unit: "vehicles",
            aggregation_key: "link_id,interval_start_seconds",
        },
        // The histogram rows carry metric=entry_vc or metric=exit_vc.
        Metric {
            name: "links",
            unit: "links",
            aggregation_key: "interval_start_seconds,metric,bin_index",
        },
        Metric {
            name: "observations",
            unit: "links",
            aggregation_key: "interval_start_seconds,metric",
        },
        Metric {
            name: "unavailable_links",
            unit: "links",
            aggregation_key: "interval_start_seconds,metric",
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
    pce_by_vehicle: &BTreeMap<&str, f64>,
    counts: &mut LinkVolumesByHour,
) {
    let (link, vehicle, side) = if let Some(event) = event.as_any().downcast_ref::<LinkEnterEvent>()
    {
        (&event.link, &event.vehicle, FlowSide::Entry)
    } else if let Some(event) = event.as_any().downcast_ref::<LinkLeaveEvent>() {
        (&event.link, &event.vehicle, FlowSide::Exit)
    } else if let Some(event) = event.as_any().downcast_ref::<VehicleEntersTrafficEvent>() {
        (&event.link, &event.vehicle, FlowSide::Entry)
    } else if let Some(event) = event.as_any().downcast_ref::<VehicleLeavesTrafficEvent>() {
        (&event.link, &event.vehicle, FlowSide::Exit)
    } else {
        return;
    };
    let id = link.external();
    if !ids.contains(id) {
        return;
    }
    let hour = time.as_nanos() / 1_000_000_000 / u64::from(interval) * u64::from(interval);
    // Vehicles that are not in the recorded catalog, e.g. transit or DRT units, leave the
    // PCE total for the interval unusable instead of silently counting as zero.
    let pce = pce_by_vehicle.get(vehicle.external()).copied();
    counts
        .entry(hour)
        .or_default()
        .entry(id.to_owned())
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
    let mut coverage = BufWriter::new(File::create(path.join("coverage.csv")).map_err(io_error)?);
    writeln!(
        coverage,
        "hour_start_seconds,eligible_links,used_links,unused_links,used_percent"
    )
    .map_err(io_error)?;
    let mut capacity =
        BufWriter::new(File::create(path.join("link_capacity.csv")).map_err(io_error)?);
    writeln!(
        capacity,
        "link_id,interval_start_seconds,capacity_pce_per_hour,effective_capacity_pce,permlanes,interval_hours,sample_size,entry_vehicles,exit_vehicles,entry_pce,exit_pce,entry_unresolved_pce,exit_unresolved_pce,entry_pce_scaled,exit_pce_scaled,entry_flow_pce_per_hour,exit_flow_pce_per_hour,entry_vc,exit_vc,entry_vc_status,exit_vc_status"
    )
    .map_err(io_error)?;

    let mut hours: BTreeSet<u64> = counts.keys().copied().collect();
    hours.extend((0..u64::from(simulation_end_time)).step_by(interval as usize));
    hours.insert(0);

    // One pass over the intervals and the links feeds every per-link table, so a
    // link's volumes are looked up and its utilization derived exactly once. The
    // interval width is derived per interval, because the final one can be shorter
    // than the configured interval.
    let mut histograms: BTreeMap<u64, IntervalHistograms> = BTreeMap::new();
    for hour in &hours {
        let hour = *hour;
        let interval_hours = covered_interval_hours(hour, interval, simulation_end_time);
        let interval_histograms = histograms.entry(hour).or_default();
        let mut used = 0usize;
        for link in links {
            let link_id = link.id.external();
            let volumes = volumes_of(counts, hour, link_id);
            used += usize::from(volumes.entries + volumes.exits > 0);
            writeln!(
                hourly,
                "{},{hour},{},{}",
                csv(link_id),
                volumes.entries,
                volumes.exits,
            )
            .map_err(io_error)?;
            let utilization = LinkUtilization::new(link, interval_hours, sample_size, &volumes);
            write_capacity_row(
                &mut capacity,
                hour,
                &utilization,
                &volumes,
                interval_hours,
                sample_size,
            )?;
            interval_histograms
                .entry
                .observe(&utilization, FlowSide::Entry);
            interval_histograms
                .exit
                .observe(&utilization, FlowSide::Exit);
        }
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
    write_histograms(path, &histograms)?;
    Ok(())
}

/// Write one `link_capacity.csv` row: PCE volumes, effective capacity and V/C.
///
/// Raw vehicle counts, the observed PCE volume and the volume expanded to the
/// unsampled network are separate columns, and a link's raw network capacity is
/// never multiplied by its lane count.
#[allow(clippy::too_many_arguments)]
fn write_capacity_row(
    table: &mut BufWriter<File>,
    interval_start_seconds: u64,
    utilization: &LinkUtilization<'_>,
    volumes: &IntervalVolumes,
    interval_hours: f64,
    sample_size: f64,
) -> Result<(), AnalysisError> {
    let (entry, exit) = (&utilization.entry, &utilization.exit);
    writeln!(
        table,
        "{},{},{:.6},{},{:.6},{:.6},{:.6},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
        csv(utilization.link_id),
        interval_start_seconds,
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
    .map_err(io_error)
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
                    metric.metric_name(),
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

/// A quantity that could not be computed is exported as an empty cell.
fn number_opt(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| format!("{value:.6}"))
}

fn write_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &[ModuleStatus],
) -> Result<(), AnalysisError> {
    let coverage = fs::read_to_string(path.join("coverage.csv")).map_err(io_error)?;
    let coverage = json_for_script(&coverage.lines().collect::<Vec<_>>())?;
    let modules = json_for_script(statuses)?;
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
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>{REPORT_STYLE}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links in {interval}-second intervals.</p><h2>Interval volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end. Both result tables and module status are embedded for offline viewing.</p><h3>Per-link interval entry and exit vehicles</h3><div id=\"hourly\"></div><h3>Interval coverage</h3><div id=\"coverage\"></div><h2>PCE volumes and capacity utilization</h2><p>Volumes are passenger-car-equivalent weighted, matching how the link flow cap is charged, and are scaled up by the simulated sample fraction to describe the full population. Raw vehicle counts, observed PCE volumes and scaled PCE volumes are exported separately. The V/C denominator is the link's own network capacity multiplied by the length of the interval the simulation covered; lanes are never applied again, and a value on a bin edge belongs to the higher bin. A link that carried no vehicles is counted as unused whatever its capacity says, while missing PCE or an invalid capacity leaves the ratio blank and is reported per link.</p><h3>Per-link PCE volumes, capacity and V/C</h3><div id=\"capacity\"></div><h3>V/C distribution</h3><p id=\"histogram-metric-label\">Entry V/C (default view)</p><div id=\"histogram\"></div><button id=\"histogram-toggle\" type=\"button\">Show exit V/C</button><h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"link_capacity.csv\">PCE volumes, capacity and V/C (CSV)</a>, <a href=\"vc_histogram.csv\">V/C distribution (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>const h={hourly};const c={coverage};const p={capacity};const d={histogram_rows};const m={modules};function table(root,headers,rows){{const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)}});const body=t.createTBody();rows.forEach(row=>{{const tr=body.insertRow();row.forEach(x=>{{const cell=tr.insertCell();cell.textContent=x}})}});root.appendChild(t)}}table(document.querySelector('#hourly'),h[0].split(','),h.slice(1).map(x=>x.split(',')));table(document.querySelector('#coverage'),c[0].split(','),c.slice(1).map(x=>x.split(',')));table(document.querySelector('#capacity'),p[0].split(','),p.slice(1).map(x=>x.split(',')));const metricColumn=d[0].indexOf('metric');let metric='entry_vc';function histogram(){{const root=document.querySelector('#histogram');root.replaceChildren();table(root,d[0],d.slice(1).filter(x=>x[metricColumn]===metric));document.querySelector('#histogram-metric-label').textContent=metric==='entry_vc'?'Entry V/C (default view)':'Exit V/C';document.querySelector('#histogram-toggle').textContent=metric==='entry_vc'?'Show exit V/C':'Show entry V/C';}}histogram();document.querySelector('#histogram-toggle').addEventListener('click',()=>{{metric=metric==='entry_vc'?'exit_vc':'entry_vc';histogram()}});{MODULE_TABLE_SCRIPT}</script></body></html>",
        iteration = manifest.iteration,
        links = manifest.eligible_links,
        interval = manifest.interval_seconds,
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
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
