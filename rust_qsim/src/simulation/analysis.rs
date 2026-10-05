//! Final-iteration link coverage and link speed reporting.

mod link_speed;

use crate::simulation::config::{Analysis, CompressionType};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, VehicleEntersTrafficEvent,
    VehicleLeavesTrafficEvent,
};
use crate::simulation::id;
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use link_speed::LinkSpeedCollector;
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

/// Optional modules whose implementation is present in this report.
const SUPPORTED_MODULES: &[&str] = &["link_speed"];

/// Modules that stay unavailable until their inputs or implementations exist. They are reported
/// as such instead of failing the report.
const OPTIONAL_MODULES: &[(&str, &str)] = &[
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
    };

    // Required inputs are validated before anything is staged, so an unreadable recording is
    // reported as a failed attempt instead of replacing a previously published report.
    let replayed = match replay_partitions(
        output_dir,
        iteration,
        partitions,
        compression,
        settings.interval_seconds,
        &ordered_links,
    ) {
        Ok(replayed) => replayed,
        Err(error) => return Err(record_failure(output_dir, &manifest, error)),
    };

    publish_complete(
        output_dir,
        &manifest,
        &ordered_links,
        &replayed,
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

/// What one replay of the final-iteration event partitions produced.
struct Replayed<'a> {
    counts: LinkVolumesByHour,
    speeds: LinkSpeedCollector<'a>,
}

fn replay_partitions<'a>(
    output_dir: &Path,
    iteration: u32,
    partitions: u32,
    compression: CompressionType,
    interval: u32,
    ordered_links: &'a [&'a Link],
) -> Result<Replayed<'a>, AnalysisError> {
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
    let mut speeds = LinkSpeedCollector::new(interval, ordered_links);
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
        accumulate(event.as_ref(), time, interval, &ids, &mut counts);
        speeds.observe(event.as_ref(), time);
        heads[rank] = readers[rank].next_event()?;
    }
    speeds.finish();
    Ok(Replayed { counts, speeds })
}

fn publish_complete<'a>(
    output_dir: &Path,
    manifest: &Manifest,
    ordered_links: &[&Link],
    replayed: &Replayed<'a>,
    interval: u32,
    simulation_end_time: u32,
    run_metadata: &AnalysisRunMetadata,
) -> Result<PathBuf, AnalysisError> {
    let staging = output_dir.join(STAGING_DIR);
    reset_staging(&staging)?;
    // A speed observation starts with a link entry in the same interval, so the entry counts
    // already cover every interval that a speed can be reported for.
    let hours = report_hours(&replayed.counts, interval, simulation_end_time);
    write_tables(&staging, ordered_links, &replayed.counts, &hours)?;
    replayed.speeds.write_tables(&staging, &hours)?;
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
    statuses.extend(SUPPORTED_MODULES.iter().map(|module| ModuleStatus {
        module,
        required: false,
        status: STATUS_COMPLETE,
        reason: None,
    }));
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

/// Every exported measure with its unit and the columns it is grouped by.
fn metrics() -> Vec<Metric<'static>> {
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
    // The traversal records are counted by the module that writes the table, so it owns their
    // names.
    .chain(
        link_speed::SpeedDiagnostics::METRICS
            .iter()
            .map(|name| Metric {
                name,
                unit: "records",
                aggregation_key: "report",
            }),
    )
    .collect()
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

/// Start of the analysis interval that contains `nanos`; intervals include their start and
/// exclude their end.
fn hour_start_seconds(nanos: u64, interval: u32) -> u64 {
    nanos / 1_000_000_000 / u64::from(interval) * u64::from(interval)
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

/// Every reported interval: those that hold an observation plus those up to the end of the day.
fn report_hours(counts: &LinkVolumesByHour, interval: u32, simulation_end_time: u32) -> Vec<u64> {
    let mut hours: BTreeSet<u64> = counts.keys().map(|key| key.hour_start_seconds).collect();
    hours.extend((0..u64::from(simulation_end_time)).step_by(interval as usize));
    hours.insert(0);
    hours.into_iter().collect()
}

fn write_tables(
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
        title: "Per-link interval entry and exit vehicles",
        file: "link_hourly.csv",
    },
    ReportTable {
        name: "coverage",
        title: "Interval coverage",
        file: "coverage.csv",
    },
];

const SPEED_TABLES: &[ReportTable] = &[
    ReportTable {
        name: "linkSpeeds",
        title: "Per-link interval speed",
        file: "link_speed_hourly.csv",
    },
    ReportTable {
        name: "speedSummary",
        title: "Across-link interval speed summary",
        file: "link_speed_summary.csv",
    },
    ReportTable {
        name: "speedHistogram",
        title: "Interval link speed histogram",
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

fn write_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &[ModuleStatus],
) -> Result<(), AnalysisError> {
    let volumes = embed_tables(path, VOLUME_TABLES)?;
    let speeds = embed_tables(path, SPEED_TABLES)?;
    let modules = json_for_script(statuses)?;
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
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MATSim analysis</title><style>{REPORT_STYLE}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration {iteration}; {links} eligible directed links in {interval}-second intervals.</p><h2>Interval volumes and coverage</h2><p>Zero-volume links are retained in every interval. Intervals include their start and exclude their end. Both result tables and module status are embedded for offline viewing.</p>{volume_sections}<h2>Interval link speeds</h2><p>Speeds are reconstructed from full-link traversals and assigned to the interval in which the vehicle entered the link. The representative speed divides the total travelled distance by the total travel time; the arithmetic vehicle-speed mean and population standard deviation describe the single traversals. A link without a full-link traversal has no speed: QSim inserts a vehicle at the end of the first link of a leg, so the first link of a network leg never covers its whole length and is reported as a partial traversal instead. The traversal records table lists every record that cannot produce a full-link speed, such as those partial traversals, traversals that never finished, and records without a positive duration.</p>{speed_sections}<h2>Module status</h2><div id=\"modules\"></div><p>Machine-readable data: <a href=\"link_hourly.csv\">link volumes (CSV)</a>, <a href=\"coverage.csv\">coverage (CSV)</a>, <a href=\"link_speed_hourly.csv\">link speeds (CSV)</a>, <a href=\"link_speed_summary.csv\">interval speed summary (CSV)</a>, <a href=\"link_speed_histogram.csv\">speed histogram (CSV)</a>, <a href=\"link_speed_diagnostics.csv\">speed traversal records (CSV)</a>, <a href=\"run_metadata.json\">expected travel and vehicle/PCE metadata (JSON)</a>, <a href=\"manifest.json\">run manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>{declarations}{MODULE_TABLE_SCRIPT}{renders}</script></body></html>",
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

/// Opens one of the exported CSV tables for writing.
fn table_writer(path: &Path, name: &str) -> Result<BufWriter<File>, AnalysisError> {
    Ok(BufWriter::new(
        File::create(path.join(name)).map_err(io_error)?,
    ))
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
