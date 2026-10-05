//! Final-iteration link coverage, capacity, speed, distance, delay and agent travel reporting.

mod accessibility;
mod agent_profile;
pub mod capacity;
mod cross_run;
mod demographic;
mod ensemble;
mod link_speed;
mod service;
pub use cross_run::compare_completed_runs;
pub use demographic::PersonDemographic;
pub use ensemble::analyze_run_ensemble;
mod survey;
mod validation;

mod transit;
pub use transit::TransitMetadata;

mod network_distance;

use crate::simulation::config::{
    Accessibility, Analysis, CompressionType, LinkLabels, ServiceInputs,
};
use crate::simulation::events::{
    EventTrait, LinkEnterEvent, LinkLeaveEvent, PersonArrivalEvent, PersonDepartureEvent,
    PersonStuckEvent, VehicleEntersTrafficEvent, VehicleLeavesTrafficEvent,
};
use crate::simulation::id;
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_events::{ProtoEventsReader, event_from_proto};
use crate::simulation::io::xml::events::XmlEventsReader;
use crate::simulation::scenario::network::{Link, Network, Node};
use crate::simulation::scenario::population::{InternalPlanElement, Population};
use crate::simulation::scenario::transit::TransitSchedule;
use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
use crate::simulation::time::SimTime;
use agent_profile::AgentProfileCollector;
use capacity::{
    FlowSide, IntervalVolumes, LinkUtilization, VC_BIN_COUNT, VcHistogram, covered_interval_hours,
    vc_bin_bounds,
};
use link_speed::LinkSpeedCollector;
use network_distance::NetworkDistanceCollector;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use tracing::warn;
use transit::TransitCollector;

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

/// Modules the report covers beyond the required one, in report order. `None` marks a module this
/// build computes, so it follows the run's outcome; `Some` carries the reason the module stays
/// unavailable until its inputs or implementation exist.
const OPTIONAL_MODULES: &[(&str, Option<&str>)] = &[
    // Both link metrics are computed from the same replay, so both follow the run's outcome.
    ("link_speed", None),
    ("network_distance_time", None),
    ("agent_travel", None),
    ("transit_performance", None),
    ("transit_validation", None),
    ("validation", None),
    ("cross_run_comparison", None),
    // Follows the run's outcome when its three supplied inputs are configured; otherwise
    // unavailable, and `module_statuses` supplies the reason.
    ("accessibility", None),
    (
        "transit_and_research",
        Some("Optional module inputs are not configured"),
    ),
    (demographic::MODULE, None),
    ("service_performance", None),
    (
        "transit_and_research",
        Some("Optional module inputs are not configured"),
    ),
];

const REPORT_STYLE: &str = "body{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;margin-bottom:2rem}td,th{border:1px solid #ccd;padding:.5rem}a{color:#075ea8}pre{background:#f4f6f9;border:1px solid #ccd;padding:1rem;overflow:auto}";

const MODULE_TABLE_SCRIPT: &str = "function table(root,headers,rows){const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)});const body=t.createTBody();rows.forEach(row=>{const tr=body.insertRow();row.forEach(x=>{const cell=tr.insertCell();cell.textContent=x})});root.replaceChildren(t)}table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))";

/// Complete report shell. Substituted in one pass by [`substitute_template`], so a link
/// label that happens to read like a token cannot corrupt the payloads.

const REPORT_TEMPLATE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>MATSim analysis</title><style>__REPORT_STYLE__label{margin-right:1rem}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration __ITERATION__; __LINKS__ eligible directed links in __INTERVAL__-second intervals.</p><h2>Final-run network coverage map</h2><p>Green links were used at least once in the final iteration; gray links were unused. Dashed links are expressways. Hover over a link for its classifications.</p><div id="map-container">__NETWORK_MAP__</div><h2>Observed validation</h2><p>Count observations are expanded by the reciprocal of the simulated sample fraction. Only exact link, period, class and metric matches are compared; __VALIDATION_NOTE__ A blank relative error means the observed reference is zero.</p><h3>Validation summary</h3><div id="validation-summary"></div><h3>Matched observations</h3><div id="validation-matches"></div>__VALIDATION_PLOTS__<p><a href="validation_summary.csv">Summary CSV</a> · <a href="validation_matches.csv">Matched observations CSV</a> · <a href="validation_unmatched.csv">Unmatched observations CSV</a></p><h2>Coverage by group</h2><p>Urban area, road type, and road size are grouped independently. Missing labels are retained as unknown; geographic boundary crossings are explicit.</p><div id="groups"></div><h2>Hourly link metrics</h2><p>Filter on any combination of classifications to compare link volumes by group.</p><div id="filters"></div><div id="hourly"></div><h2>Hourly network coverage</h2><div id="coverage"></div><h2>PCE volumes and capacity utilization</h2><p>Volumes are passenger-car-equivalent weighted, matching how the link flow cap is charged, and are scaled up by the simulated sample fraction to describe the full population. Raw vehicle counts, observed PCE volumes and scaled PCE volumes are exported separately. The V/C denominator is the link's own network capacity multiplied by the length of the interval the simulation covered; lanes are never applied again, and a value on a bin edge belongs to the higher bin. A link that carried no vehicles is counted as unused whatever its capacity says, while missing PCE or an invalid capacity leaves the ratio blank and is reported per link.</p><h3>Per-link PCE volumes, capacity and V/C</h3><div id="capacity"></div><h3>V/C distribution</h3><p id="histogram-metric-label">Entry V/C (default view)</p><div id="histogram"></div><button id="histogram-toggle" type="button">Show exit V/C</button><h2>Interval link speeds</h2><p>__SPEED_NOTE__</p>__SPEED_SECTIONS__<h2>Network distance, time and congestion</h2><p>Vehicle distance uses the observed fraction of each link. Partial and unfinished traversals are reported in diagnostics. A traversal crossing an interval boundary is assigned whole to its entry interval, so no within-link path is inferred. Relative delay is signed; clipped excess delay, when configured, sums positive link delay capped per link and interval. Passenger distance and time are unavailable because link-level passenger occupancy is not recorded. <a href="network_distance_time_diagnostics.csv">Traversal diagnostics (CSV)</a>.</p><h3>Network totals and peak-hour profile</h3><p id="peak-delay"></p><div id="network-summary"></div><h3>Per-link distance, time and relative speed</h3><div id="network-link-metrics"></div><h3>Traversal exclusions</h3><div id="network-distance-diagnostics"></div><h2>Available metrics</h2><div id="metrics"></div><h2>Agent travel</h2><p>Leg completion uses observed departure and arrival events. Incomplete persons retain completed-leg duration totals; missing arrivals are excluded from duration means. Verified non-travelers have an expected plan with no legs. Journeys run between substantive activities; stage activities such as transit transfers stay within the journey. Main mode follows the MATSim analysis hierarchy. Distances sum planned route distances, including prepared teleported routes, and report when any component is unavailable.</p><h3>En-route agent profile</h3><p>Counts use observed person departures, arrivals, and stuck events across all travel modes. Person-seconds are allocated by event timestamps; no within-link position or occupancy is inferred.</p><div id="en-route-agents"></div><h3>Departures and duration by interval and mode</h3><div id="leg-hourly"></div><h3>Journey mode share by hour, purpose, and distance</h3><div id="journey-shares"></div><h3>Journey duration and distance distributions</h3><div id="journey-summary"></div><h3>Journey components and completion</h3><div id="journeys"></div><h3>Daily cohort means</h3><div id="daily"></div><h3>Person daily totals and status</h3><div id="persons"></div><h3>Observed and planned legs</h3><p>__LEGS_NOTE__</p><div id="legs"></div>__SURVEY_SECTION____TRANSIT_SECTION____CROSS_RUN_SECTION____ACCESSIBILITY_SECTION____SERVICE_SECTION__<h2>Demographic outcomes and equity</h2><p>__DEMOGRAPHIC_NOTE__</p><h3>Group sizes and travel burdens</h3><div id="group-burdens"></div><h3>Person groups</h3><div id="person-demographics"></div><h3>Other modules' group outcomes</h3><div id="group-module-outcomes"></div><h3>Equity comparison</h3><div id="equity-comparison"></div><h2>Module status</h2><div id="modules"></div><p>Machine-readable data: <a href="network_map.svg">coverage map (SVG)</a>, <a href="link_classification.csv">link classifications (CSV)</a>, <a href="group_coverage.csv">group coverage (CSV)</a>, <a href="link_hourly.csv">link volumes (CSV)</a>, <a href="link_capacity.csv">PCE volumes, capacity and V/C (CSV)</a>, <a href="vc_histogram.csv">V/C distribution (CSV)</a>, <a href="coverage.csv">coverage (CSV)</a>, <a href="link_speed_hourly.csv">link speeds (CSV)</a>, <a href="link_speed_summary.csv">interval speed summary (CSV)</a>, <a href="link_speed_histogram.csv">speed histogram (CSV)</a>, <a href="link_speed_diagnostics.csv">speed traversal records (CSV)</a>, <a href="leg_hourly.csv">legs by interval and mode (CSV)</a>, <a href="journeys.csv">journey components and completion (CSV)</a>, <a href="journey_mode_share.csv">journey mode shares (CSV)</a>, <a href="journey_summary.csv">journey distributions (CSV)</a>, <a href="person_daily.csv">person daily totals (CSV)</a>, <a href="daily_summary.csv">daily cohort means (CSV)</a>, <a href="legs.csv">legs (CSV)</a>, <a href="journey_survey_comparison.csv">journey survey (CSV)</a>, <a href="service_summary.csv">service summary (CSV)</a>, <a href="service_requests.csv">service requests (CSV)</a>, <a href="service_vehicles.csv">service vehicles (CSV)</a>, <a href="service_occupancy.csv">service occupancy (CSV)</a>, <a href="service_constraints.csv">service constraints (CSV)</a>, <a href="service_availability.csv">service availability (CSV)</a>, <a href="service_diagnostics.csv">service diagnostics (CSV)</a>, <a href="transit_trips.csv">transit trips (CSV)</a>, <a href="transit_stop_hourly.csv">transit boardings and alightings (CSV)</a>, <a href="transit_line_summary.csv">transit line summary (CSV)</a>, <a href="transit_occupancy.csv">transit occupancy (CSV)</a>, <a href="transit_journeys.csv">transit journeys (CSV)</a>, <a href="transit_outcomes.csv">transit outcomes (CSV)</a>, <a href="transit_availability.csv">transit availability (CSV)</a>, <a href="transit_validation_summary.csv">transit observed demand (CSV)</a>, <a href="run_metadata.json">expected travel and vehicle/PCE metadata (JSON)</a>, <a href="manifest.json">run manifest</a>, <a href="metric_catalog.json">metric catalog</a>.</p><script>const d=__LINK_HOURLY__;const c=__COVERAGE__;const a=__METRICS__;const cap=__LINK_CAPACITY__;const bins=__VC_HISTOGRAM__;const m=__MODULES__;const D=__DIMENSIONS__;const lh=__LEG_HOURLY__;const dy=__DAILY__;const pd=__PERSONS__;const lg=__LEGS__;const gb=__GROUP_BURDENS__;const pg=__PERSON_DEMOGRAPHICS__;const gm=__GROUP_MODULE_OUTCOMES__;const eq=__EQUITY_COMPARISON__;const js=__JOURNEY_SHARES__;const jy=__JOURNEY_SUMMARY__;const jn=__JOURNEYS__;const jsurvey=__JOURNEY_SURVEY__;__SPEED_DECLARATIONS____MODULE_TABLE_SCRIPT__;__CSV_TABLE_SCRIPT__;csvTable('#validation-summary',__VALIDATION_SUMMARY__);csvTable('#validation-matches',__VALIDATION_MATCHES__);__TRANSIT_RENDER____CROSS_RUN_RENDER____SERVICE_RENDER____ACCESSIBILITY_SCRIPTS__table(document.querySelector('#coverage'),['hour_start_seconds','eligible_links','used_links','unused_links','used_percent'],c.slice(1).map(x=>x.split(',')));__SPEED_RENDERS__table(document.querySelector('#capacity'),cap[0].split(','),cap.slice(1).map(x=>x.split(',')));const metricColumn=bins[0].indexOf('metric');let metric='entry_vc';function histogram(){const root=document.querySelector('#histogram');root.replaceChildren();table(root,bins[0],bins.slice(1).filter(x=>x[metricColumn]===metric));document.querySelector('#histogram-metric-label').textContent=metric==='entry_vc'?'Entry V/C (default view)':'Exit V/C';document.querySelector('#histogram-toggle').textContent=metric==='entry_vc'?'Show exit V/C':'Show entry V/C';}histogram();document.querySelector('#histogram-toggle').addEventListener('click',()=>{metric=metric==='entry_vc'?'exit_vc':'entry_vc';histogram()});table(document.querySelector('#metrics'),['Metric','Unit','Aggregation key'],a.map(x=>[x.name,x.unit,x.aggregation_key]));csvTable('#leg-hourly',lh);csvTable('#journey-shares',js);csvTable('#journey-survey',jsurvey);csvTable('#journey-summary',jy);csvTable('#journeys',jn);csvTable('#daily',dy);csvTable('#persons',pd);csvTable('#legs',lg);csvTable('#group-burdens',gb);csvTable('#person-demographics',pg);csvTable('#group-module-outcomes',gm);csvTable('#equity-comparison',eq);const selectors=[];D.forEach(([key,title])=>{const label=document.createElement('label');label.textContent=title+' ';const select=document.createElement('select');select.append(new Option('All',''));[...new Set(d.map(x=>x[key]))].sort().forEach(value=>select.append(new Option(value,value)));label.append(select);document.querySelector('#filters').append(label);select.addEventListener('change',renderHourly);selectors.push([key,select])});function selectedRows(){return d.filter(row=>selectors.every(([key,select])=>select.value===''||row[key]===select.value))}function renderHourly(){const rows=selectedRows();table(document.querySelector('#hourly'),['link_id','hour_start_seconds','entry_vehicles','exit_vehicles','urban_area','road_type','road_size'],rows.map(row=>[row.link_id,row.hour_start_seconds,row.entry_vehicles,row.exit_vehicles,row.urban_area,row.road_type,row.road_size]));renderGroups(rows);updateMap()}function renderGroups(rows){const groups=new Map();rows.forEach(row=>D.map(([dimension])=>[dimension,row[dimension]]).forEach(([dimension,category])=>{const key=JSON.stringify([dimension,category,row.hour_start_seconds]);let group=groups.get(key);if(!group){group={dimension,category,hour:row.hour_start_seconds,eligible:0,used:0};groups.set(key,group)}group.eligible++;if(row.entry_vehicles+row.exit_vehicles>0)group.used++}));const values=[...groups.values()].map(group=>[group.dimension,group.category,group.hour,group.eligible,group.used,group.eligible-group.used,(group.used*100/group.eligible).toFixed(6)]);table(document.querySelector('#groups'),['Dimension','Group','Hour start (s)','Eligible','Used','Unused','Used (%)'],values)}function updateMap(){document.querySelectorAll('#network-map line').forEach(line=>{line.style.display=selectors.every(([key,select])=>select.value===''||line.getAttribute('data-'+key.replace('_','-'))===select.value)?'':'none'})}renderHourly()__NETWORK_ANALYSIS_SCRIPT__</script></body></html>"#;

/// Renders the agent travel tables. They quote person identifiers, so the header and every row
/// are split with a quote-aware parser instead of `String.split(',')`.
const CSV_TABLE_SCRIPT: &str = "function parseCsv(line){const fields=[];let field='',quoted=false;for(let i=0;i<line.length;i++){const ch=line[i];if(ch.charCodeAt(0)===34){if(quoted&&line.charCodeAt(i+1)===34){field+=String.fromCharCode(34);i++}else{quoted=!quoted}}else if(ch===','&&!quoted){fields.push(field);field=''}else{field+=ch}}fields.push(field);return fields}function csvTable(id,rows){table(document.querySelector(id),parseCsv(rows[0]),rows.slice(1).map(parseCsv))}";

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
const PERSON_DEMOGRAPHIC_PREVIEW_ROWS: usize = 200;
/// Road-type label the coverage map renders as a dashed expressway.
const EXPRESSWAY: &str = "expressway";
/// Leg rows embedded in the local report before it defers to the full `legs.csv`.
const LEGS_PREVIEW_ROWS: usize = 200;
/// Rows of each accessibility table embedded in the local report. The CSVs hold every row.
const ACCESSIBILITY_PREVIEW_ROWS: usize = 500;
/// Dimensions every accessibility row is grouped by, as the catalog declares them.
const ACCESSIBILITY_AGGREGATION_KEY: &str =
    "origin_zone,category,mode,departure_period_start_seconds,threshold_seconds";
/// The same dimensions without the origin, which is how a summary row aggregates.
const ACCESSIBILITY_SUMMARY_KEY: &str =
    "category,mode,departure_period_start_seconds,threshold_seconds";

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
    /// Classification inputs, recorded so [`reanalyze_completed_run`] rebuilds the same
    /// report. Absent in a manifest written before classifications existed, which reads
    /// back as "nothing labelled" rather than failing the rerun.
    #[serde(default)]
    link_labels: BTreeMap<String, LinkLabels>,
    #[serde(default)]
    urban_boundary: Option<Vec<[f64; 2]>>,
    #[serde(default)]
    observed_data: Option<String>,
    #[serde(default)]
    journey_survey: Option<String>,
    #[serde(default)]
    comparison_runs: Vec<String>,
    /// Service records, recorded so a standalone rerun analyses the same supplied inputs.
    #[serde(default)]
    service: Option<ServiceInputs>,
    #[serde(default)]
    transit_observed_data: Option<String>,
    #[serde(default)]
    person_group_attributes: Vec<String>,
    #[serde(default)]
    person_weight_attribute: Option<String>,
    #[serde(default)]
    person_cost_attribute: Option<String>,

    excess_delay_clip_seconds: Option<f64>,
    /// Accessibility inputs, recorded so [`reanalyze_completed_run`] rebuilds the same
    /// report. Absent in a manifest written before the module existed, which reads back as
    /// "nothing configured" rather than failing the rerun.
    #[serde(default)]
    accessibility: Accessibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonExpectedTravel {
    person_id: String,
    /// Coordinate of the person's first non-stage activity, or `None` for a plan without one.
    /// Accessibility places a person in the zone nearest this point, so the coordinate has to
    /// be recorded here rather than recovered later: the analysis pass never sees the
    /// population, only this metadata.
    #[serde(default)]
    home_coord: Option<[f64; 2]>,
    legs: Vec<ExpectedLeg>,
    #[serde(default)]
    journeys: Vec<ExpectedJourney>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExpectedLeg {
    leg_index: usize,
    mode: String,
    departure_seconds: Option<f64>,
    expected_travel_seconds: Option<f64>,
    distance_meters: Option<f64>,
    /// The plan routes this leg through the transit schedule.
    #[serde(default)]
    transit: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ExpectedJourney {
    journey_index: usize,
    origin: String,
    destination: String,
    origin_link: String,
    destination_link: String,
    purpose: String,
    leg_indices: Vec<usize>,
    component_modes: Vec<String>,
    distance_meters: Option<f64>,
    distance_provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug)]
struct JourneyRow {
    person_id: String,
    journey_index: usize,
    departure_seconds: Option<f64>,
    departure_hour: Option<u64>,
    origin: String,
    destination: String,
    origin_link: String,
    destination_link: String,
    purpose: String,
    main_mode: String,
    component_modes: String,
    component_leg_indices: String,
    duration_seconds: Option<f64>,
    completion: &'static str,
    distance_meters: Option<f64>,
    distance_provenance: String,
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
    #[serde(default)]
    pub person_demographics: Vec<PersonDemographic>,
    vehicles: Vec<VehiclePce>,
    vehicle_types: Vec<VehicleTypePce>,
    /// Schedule and vehicle capacities for transit analysis. Absent for a run without a transit
    /// schedule and for metadata written before transit analysis existed.
    #[serde(default)]
    transit: Option<TransitMetadata>,
}

#[derive(Serialize)]
struct CrossRunManifest {
    status: &'static str,
    module: &'static str,
    runs: Vec<CrossRunReference>,
}

#[derive(Serialize)]
struct CrossRunReference {
    run_dir: String,
    iteration: u32,
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
            person_demographics: Vec::new(),
            vehicles,
            vehicle_types,
            transit: None,
        }
    }

    /// Attach the demographics captured from the live population.
    pub fn with_person_demographics(mut self, demographics: Vec<PersonDemographic>) -> Self {
        self.person_demographics = demographics;
        self
    }

    /// Record the transit schedule, with the capacity each departure's vehicle declares.
    pub fn with_transit(mut self, schedule: &TransitSchedule, garage: &Garage) -> Self {
        self.transit = (!schedule.lines().is_empty())
            .then(|| TransitMetadata::from_schedule(schedule, garage));
        self
    }

    /// Simulated fraction of the population the observed volumes were scaled up from.
    pub fn sample_size(&self) -> f64 {
        self.sample_size
    }
}

/// Capture grouping attributes, weights and costs while the population is available.
pub fn capture_person_demographics(
    population: &Population,
    settings: &Analysis,
) -> Vec<PersonDemographic> {
    demographic::capture(population, settings)
}

/// Capture compact plan expectations immediately before the final iteration's mobsim.
pub fn capture_expected_travel(population: &Population) -> Vec<PersonExpectedTravel> {
    let mut persons: Vec<_> = population.persons.values().collect();
    persons.sort_by(|a, b| a.id().external().cmp(b.id().external()));
    persons
        .into_iter()
        .filter_map(|person| {
            let plan = person.selected_plan()?;
            let mut legs_by_index = BTreeMap::new();
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
                    let distance_meters = leg
                        .route
                        .as_ref()
                        .and_then(|route| route.as_generic().distance())
                        .filter(|distance| distance.is_finite() && *distance >= 0.0);
                    let expected_leg = ExpectedLeg {
                        leg_index: element_index,
                        mode: leg.mode.external().to_owned(),
                        departure_seconds: leg.dep_time.map(|time| time.as_nanos() as f64 / 1e9),
                        expected_travel_seconds: expected.map(|time| time.as_secs_f64()),
                        distance_meters,
                        transit: leg
                            .route
                            .as_ref()
                            .is_some_and(|route| route.as_pt().is_some()),
                    };
                    legs_by_index.insert(element_index, expected_leg.clone());
                    Some(expected_leg)
                })
                .collect();
            let activities: Vec<_> = plan
                .elements
                .iter()
                .enumerate()
                .filter_map(|(index, element)| match element {
                    InternalPlanElement::Activity(activity) if !activity.is_interaction() => {
                        Some((index, activity))
                    }
                    _ => None,
                })
                .collect();
            let journeys = activities
                .windows(2)
                .enumerate()
                .filter_map(|(journey_index, pair)| {
                    let (origin_index, origin) = pair[0];
                    let (destination_index, destination) = pair[1];
                    let expected_legs: Vec<_> = legs_by_index
                        .range((origin_index + 1)..destination_index)
                        .map(|(_, leg)| leg)
                        .collect();
                    if expected_legs.is_empty() {
                        return None;
                    }
                    let distances: Vec<_> = expected_legs
                        .iter()
                        .filter_map(|leg| leg.distance_meters)
                        .collect();
                    let total_distance: f64 = distances.iter().sum();
                    let all_distances_available = distances.len() == expected_legs.len();
                    let distance_meters = (all_distances_available && total_distance.is_finite())
                        .then_some(total_distance);
                    let distance_provenance = if distance_meters.is_some() {
                        "planned_route"
                    } else if distances.is_empty() || all_distances_available {
                        "unavailable"
                    } else {
                        "partial_planned_route"
                    };
                    Some(ExpectedJourney {
                        journey_index,
                        origin: origin.act_type.external().to_owned(),
                        destination: destination.act_type.external().to_owned(),
                        origin_link: origin.link_id.external().to_owned(),
                        destination_link: destination.link_id.external().to_owned(),
                        purpose: destination.act_type.external().to_owned(),
                        leg_indices: expected_legs.iter().map(|leg| leg.leg_index).collect(),
                        component_modes: expected_legs.iter().map(|leg| leg.mode.clone()).collect(),
                        distance_meters,
                        distance_provenance: distance_provenance.to_owned(),
                    })
                })
                .collect();
            Some(PersonExpectedTravel {
                person_id: person.id().external().to_owned(),
                home_coord: activities
                    .first()
                    .and_then(|(_, activity)| activity.coord.as_ref())
                    .map(|coordinate| [coordinate.x, coordinate.y])
                    .filter(|[x, y]| x.is_finite() && y.is_finite()),
                legs,
                journeys,
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
type LinkVolumesByClass = BTreeMap<String, LinkVolumesByHour>;

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
    if settings
        .excess_delay_clip_seconds
        .is_some_and(|value| !value.is_finite() || value < 0.0)
    {
        return Err(AnalysisError::new(
            "analysis.excess_delay_clip_seconds must be a non-negative finite number",
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
        // Recorded so the report says which fraction the volumes were scaled up from.
        sample_size,
        network_input: run_metadata.network_input.clone(),
        population_input: run_metadata.population_input.clone(),
        software_version: env!("CARGO_PKG_VERSION").to_owned(),
        link_labels: settings.link_labels.clone(),
        urban_boundary: settings.urban_boundary.clone(),

        observed_data: settings
            .observed_data
            .as_ref()
            .map(|path| path.display().to_string()),
        journey_survey: settings
            .journey_survey
            .as_ref()
            .map(|path| path.display().to_string()),
        comparison_runs: settings
            .comparison_runs
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        service: settings.service.clone(),
        transit_observed_data: settings
            .transit_observed_data
            .as_ref()
            .map(|path| path.display().to_string()),
        person_group_attributes: settings.person_group_attributes.clone(),
        person_weight_attribute: settings.person_weight_attribute.clone(),
        person_cost_attribute: settings.person_cost_attribute.clone(),

        excess_delay_clip_seconds: settings.excess_delay_clip_seconds,
        accessibility: settings.accessibility.clone(),
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
        run_metadata,
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

        observed_data: recorded.observed_data.as_ref().map(PathBuf::from),
        journey_survey: recorded.journey_survey.as_ref().map(PathBuf::from),
        comparison_runs: recorded.comparison_runs.iter().map(PathBuf::from).collect(),
        service: recorded.service.clone(),
        transit_observed_data: recorded.transit_observed_data.as_ref().map(PathBuf::from),
        person_group_attributes: recorded.person_group_attributes.clone(),
        person_weight_attribute: recorded.person_weight_attribute.clone(),
        person_cost_attribute: recorded.person_cost_attribute.clone(),

        excess_delay_clip_seconds: recorded.excess_delay_clip_seconds,
        accessibility: recorded.accessibility.clone(),
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

/// Compare journey-mode shares from each supplied run's latest completed iteration report.
///
/// This consumes saved report tables, so comparison never replays or reruns the supplied
/// simulations. The output is a self-contained local report under output_dir.
pub fn compare_latest_run_reports(
    output_dir: &Path,
    run_dirs: &[PathBuf],
) -> Result<PathBuf, AnalysisError> {
    if run_dirs.is_empty() {
        return Err(AnalysisError::new(
            "cross-run comparison requires at least one completed run",
        ));
    }
    let staging = output_dir.join(".cross-run-comparison-staging");
    reset_staging(&staging)?;
    let mut combined = csv::Writer::from_path(staging.join("journey_mode_share.csv"))
        .map_err(|error| AnalysisError(error.to_string()))?;
    combined
        .write_record([
            "run_dir",
            "departure_hour_seconds",
            "purpose",
            "distance_class",
            "main_mode",
            "journeys",
            "share",
        ])
        .map_err(|error| AnalysisError(error.to_string()))?;
    let mut references = Vec::new();
    for run_dir in run_dirs {
        let report_dir = run_dir.join(ANALYSIS_DIR);
        let manifest: Manifest = read_json(&report_dir.join(MANIFEST_FILE))?;
        if manifest.status != STATUS_COMPLETE {
            return Err(AnalysisError(format!(
                "run {} has no completed latest-iteration analysis report",
                run_dir.display()
            )));
        }
        let latest_iteration = latest_output_iteration(run_dir)?;
        if latest_iteration != manifest.iteration {
            return Err(AnalysisError(format!(
                "run {} report covers iteration {}, but its latest output is iteration {latest_iteration}",
                run_dir.display(),
                manifest.iteration
            )));
        }
        let table_path = report_dir.join("journey_mode_share.csv");
        let mut table = csv::Reader::from_path(&table_path).map_err(|error| {
            AnalysisError(format!("cannot read {}: {error}", table_path.display()))
        })?;
        for row in table.records() {
            let row = row.map_err(|error| AnalysisError(error.to_string()))?;
            let mut combined_row = vec![run_dir.display().to_string()];
            combined_row.extend(row.iter().map(str::to_owned));
            combined
                .write_record(combined_row)
                .map_err(|error| AnalysisError(error.to_string()))?;
        }
        references.push(CrossRunReference {
            run_dir: run_dir.display().to_string(),
            iteration: manifest.iteration,
        });
    }
    combined
        .flush()
        .map_err(|error| AnalysisError(error.to_string()))?;
    write_json(
        &staging.join("manifest.json"),
        &CrossRunManifest {
            status: STATUS_COMPLETE,
            module: "cross_run_comparison",
            runs: references,
        },
    )?;
    write_json(
        &staging.join(METRIC_CATALOG_FILE),
        &[
            Metric {
                name: "journeys",
                unit: "journeys",
                aggregation_key: "run_dir,departure_hour_seconds,purpose,distance_class,main_mode",
            },
            Metric {
                name: "share",
                unit: "proportion",
                aggregation_key: "run_dir,departure_hour_seconds,purpose,distance_class,main_mode",
            },
        ],
    )?;
    write_json(
        &staging.join(MODULE_STATUS_FILE),
        &[ModuleStatus {
            module: "cross_run_comparison",
            required: true,
            status: STATUS_COMPLETE,
            reason: None,
        }],
    )?;
    write_cross_run_report(&staging)?;
    publish(
        &staging,
        &output_dir.join("cross_run_comparison"),
        &output_dir.join(".cross-run-comparison-backup"),
    )
    .map(|path| path.join("index.html"))
}

fn latest_output_iteration(run_dir: &Path) -> Result<u32, AnalysisError> {
    let iterations = run_dir.join("ITERS");
    let entries = fs::read_dir(&iterations)
        .map_err(|error| AnalysisError(format!("cannot read {}: {error}", iterations.display())))?;
    entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_name()
                .to_str()?
                .strip_prefix("it.")?
                .parse()
                .ok()
        })
        .max()
        .ok_or_else(|| {
            AnalysisError(format!(
                "no output iterations found in {}",
                run_dir.display()
            ))
        })
}

fn write_cross_run_report(path: &Path) -> Result<(), AnalysisError> {
    let rows = csv_for_script(&path.join("journey_mode_share.csv"))?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Cross-run journey comparison</title><style>{REPORT_STYLE}</style></head><body><h1>Cross-run journey mode shares</h1><p>Each row comes from the latest completed iteration report of its run. Shares are grouped by departure interval, purpose, distance class, and main mode.</p><div id=\"comparison\"></div><p><a href=\"journey_mode_share.csv\">CSV table</a> · <a href=\"manifest.json\">run iterations</a> · <a href=\"metric_catalog.json\">metric catalog</a></p><script>{CSV_TABLE_SCRIPT}csvTable('#comparison',{rows});</script></body></html>"
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
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

fn replay_partitions<'a>(
    output_dir: &Path,
    iteration: u32,
    partitions: u32,
    compression: CompressionType,
    interval: u32,
    ordered_links: &'a [&'a Link],
    run_metadata: &AnalysisRunMetadata,
) -> Result<ReplayedAnalysis<'a>, AnalysisError> {
    // The recorded vehicle catalog is the only PCE source, so a standalone rerun weights
    // vehicles exactly like the run it reproduces.
    let pce_by_vehicle: BTreeMap<&str, f64> = run_metadata
        .vehicles
        .iter()
        .map(|vehicle| (vehicle.vehicle_id.as_str(), vehicle.pce))
        .collect();
    let class_by_vehicle: BTreeMap<&str, &str> = run_metadata
        .vehicles
        .iter()
        .filter(|vehicle| vehicle.vehicle_type_id != "all")
        .map(|vehicle| {
            (
                vehicle.vehicle_id.as_str(),
                vehicle.vehicle_type_id.as_str(),
            )
        })
        .collect();
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
    let mut counts_by_class = LinkVolumesByClass::new();
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
    let expected_journeys: BTreeMap<_, _> = run_metadata
        .expected_travel
        .iter()
        .map(|person| (person.person_id.clone(), person.journeys.clone()))
        .collect();
    let mut agent_travel = AgentTravelAccumulator::new(interval, expected);

    let mut speeds = LinkSpeedCollector::new(interval, ordered_links, &class_by_vehicle);
    let mut network_distance = NetworkDistanceCollector::new(interval, ordered_links);
    let mut agent_profiles = AgentProfileCollector::new(interval);
    let mut transit = TransitCollector::new(
        run_metadata
            .expected_travel
            .iter()
            .flat_map(|person| &person.legs)
            .filter(|leg| leg.transit)
            .map(|leg| leg.mode.clone())
            .collect(),
    );

    loop {
        let Some(time) = heads
            .iter()
            .filter_map(|event| event.as_ref().map(|(time, _)| *time))
            .min()
        else {
            break;
        };
        // Link counts and link speeds commute; process all same-time agent events together so that
        // pairing an arrival with a same-time departure does not depend on which partition
        // delivered either event first. Within a batch, an arrival still matches the person's
        // open leg before any leg created in the same batch. Visiting the partitions in rank
        // order keeps the replay itself reproducible.
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
                    interval,
                    &ids,
                    &pce_by_vehicle,
                    &class_by_vehicle,
                    &mut counts,
                    &mut counts_by_class,
                );
                speeds.observe(event.as_ref(), time);
                network_distance.observe(event.as_ref(), time);
                simultaneous_events.push(event);
                heads[rank] = readers[rank].next_event()?;
            }
        }
        agent_travel.process_timestamp(&simultaneous_events, time);
        agent_profiles.process_timestamp(&simultaneous_events, time);
        transit.process_timestamp(&simultaneous_events, time);
    }
    transit.finish();
    speeds.finish();
    network_distance.finish();
    Ok(ReplayedAnalysis {
        counts,
        counts_by_class,
        agent_travel,
        expected_journeys,
        speeds,
        network_distance,
        agent_profiles,
        transit,
    })
}

/// Everything one replay of the final-iteration partitions yields. The speed collector borrows the
/// reported links, so the bundle carries the same lifetime as the network slice it was built over.
struct ReplayedAnalysis<'a> {
    counts: LinkVolumesByHour,
    counts_by_class: LinkVolumesByClass,
    agent_travel: AgentTravelAccumulator,
    expected_journeys: BTreeMap<String, Vec<ExpectedJourney>>,
    speeds: LinkSpeedCollector<'a>,
    network_distance: NetworkDistanceCollector<'a>,
    agent_profiles: AgentProfileCollector,
    transit: TransitCollector,
}

fn publish_complete(
    output_dir: &Path,
    manifest: &Manifest,
    ordered_links: &[&Link],
    replayed: &ReplayedAnalysis<'_>,
    interval: u32,
    simulation_end_time: u32,
    run_metadata: &AnalysisRunMetadata,
    network: &Network,
    settings: &Analysis,
) -> Result<PathBuf, AnalysisError> {
    let staging = output_dir.join(STAGING_DIR);
    reset_staging(&staging)?;
    let counts = &replayed.counts;
    let agent_travel = &replayed.agent_travel;
    let classifications = classify_links(ordered_links, network, settings);
    let link_hourly = link_hourly_metrics(
        ordered_links,
        &classifications,
        counts,
        interval,
        simulation_end_time,
    );
    // Volume, coverage, group and speed tables all span the same intervals, taken once from the
    // link counts. A speed observation starts with a link entry in the same interval, so the
    // entry counts already cover every interval a speed can be reported for, and sharing the
    // list keeps a row of one table aligned with the row of the other.
    let hours: Vec<u64> = interval_starts(counts, interval, simulation_end_time)
        .into_iter()
        .collect();
    write_tables(
        &staging,
        ordered_links,
        counts,
        &agent_travel.observed_legs,
        &agent_travel.expected,
        &agent_travel.stuck_people,
        &replayed.expected_journeys,
        interval,
        simulation_end_time,
        // The same accessor the validation used, so the scale that was checked and
        // the scale that is written can never disagree.
        run_metadata.sample_size(),
        &link_hourly,
    )?;
    replayed.speeds.write_tables(&staging, &hours)?;

    write_class_counts(
        &staging,
        ordered_links,
        &hours,
        &run_metadata.vehicle_types,
        &replayed.counts_by_class,
    )?;
    replayed.speeds.write_class_hourly_speeds(&staging)?;

    replayed
        .network_distance
        .write_tables(&staging, &hours, settings.excess_delay_clip_seconds)?;
    replayed
        .agent_profiles
        .write_table(&staging, interval, simulation_end_time)?;

    // The transit tables read the journeys and legs of the agent travel replay.
    let (transit_summary, transit_stops) = transit::write_tables(
        &staging,
        run_metadata.transit.as_ref(),
        &replayed.transit,
        &agent_travel.observed_legs,
        &replayed.expected_journeys,
        interval,
        run_metadata.sample_size(),
    )?;
    let transit_validation = settings.transit_observed_data.as_ref().map(|source| {
        let source = if source.is_absolute() {
            source.clone()
        } else {
            output_dir.join(source)
        };
        transit::write_observed(
            &staging,
            &source,
            run_metadata.transit.as_ref(),
            &transit_stops,
            interval,
            run_metadata.sample_size(),
        )
        .map_err(|error| error.to_string())
    });
    if transit_validation.as_ref().is_none_or(Result::is_err) {
        transit::write_empty_observed(&staging)?;
    }

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

    write_json(
        &staging.join(METRIC_CATALOG_FILE),
        &metrics(
            settings.excess_delay_clip_seconds.is_some(),
            settings.accessibility.is_configured(),
        ),
    )?;
    let vehicle_classes: Vec<_> = run_metadata
        .vehicle_types
        .iter()
        .filter(|vehicle_type| vehicle_type.vehicle_type_id != "all")
        .map(|vehicle_type| vehicle_type.vehicle_type_id.clone())
        .collect();
    let validation = settings.observed_data.as_ref().map(|source| {
        let source = if source.is_absolute() {
            source.clone()
        } else {
            output_dir.join(source)
        };
        validation::write(
            &staging,
            &source,
            interval,
            run_metadata.sample_size(),
            &vehicle_classes,
        )
        .map_err(|error| error.to_string())
    });
    if validation.as_ref().is_none_or(Result::is_err) {
        write_empty_validation(&staging)?;
    }
    let survey = settings.journey_survey.as_ref().map(|source| {
        let source = if source.is_absolute() {
            source.clone()
        } else {
            output_dir.join(source)
        };
        survey::write(&staging, &source).map_err(|error| error.to_string())
    });
    if survey.as_ref().is_none_or(Result::is_err) {
        survey::write_empty(&staging)?;
    }
    let comparison = (!settings.comparison_runs.is_empty()).then(|| {
        cross_run::write(output_dir, &staging, &settings.comparison_runs)
            .map_err(|error| error.to_string())
    });
    if comparison.as_ref().is_none_or(Result::is_err) {
        cross_run::write_empty(&staging)?;
    }
    // A partially configured accessibility input cannot produce a measure, so a bad setting
    // fails only this module; the rest of the report is unaffected.
    let accessibility = accessibility_result(output_dir, &staging, settings, run_metadata);
    if !matches!(accessibility, Some(Ok(()))) {
        // Replace whatever a half-written module left behind, so the report never mixes a
        // partial accessibility result with an unavailable one.
        accessibility::write_empty(
            &staging,
            accessibility
                .as_ref()
                .and_then(|result| result.as_ref().err().map(String::as_str)),
        )?;
    }
    let service = settings.service.as_ref().map(|inputs| {
        service::write(&staging, output_dir, inputs, network, ordered_links)
            .map_err(|error| error.to_string())
    });
    if service.as_ref().is_none_or(Result::is_err) {
        service::write_empty(&staging)?;
    }
    let demographics = if settings.person_group_attributes.is_empty()
        || run_metadata.person_demographics.is_empty()
    {
        None
    } else {
        Some(
            demographic::write(&staging, output_dir, settings, run_metadata)
                .map_err(|error| error.to_string()),
        )
    };
    if demographics.as_ref().is_none_or(Result::is_err) {
        demographic::write_empty(&staging)?;
    }
    let statuses = module_statuses(
        &RequiredOutcome::Complete,
        validation.as_ref(),
        comparison.as_ref(),
        accessibility.as_ref(),
        service.as_ref(),
        demographics.as_ref(),
        &TransitOutcome {
            has_service_records: transit_summary.has_service_records,
            validation: transit_validation.as_ref(),
        },
        survey.as_ref(),
    );

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

/// Outcome of the accessibility module.
///
/// `None` means no accessibility input is configured, which leaves the module unavailable.
/// A configured module either produces its tables or reports why it cannot; a partial set of
/// inputs and an unreadable file are both that one failure, and neither touches the required
/// module.
fn accessibility_result(
    output_dir: &Path,
    staging: &Path,
    settings: &Analysis,
    run_metadata: &AnalysisRunMetadata,
) -> Option<Result<(), String>> {
    if !settings.accessibility.is_configured() {
        return None;
    }
    Some(settings.accessibility.validate().and_then(|()| {
        accessibility::write(
            staging,
            output_dir,
            &settings.accessibility,
            &run_metadata.expected_travel,
            run_metadata.sample_size(),
        )
        .map_err(|error| error.to_string())
    }))
}

fn write_class_counts(
    path: &Path,
    links: &[&Link],
    hours: &[u64],
    classes: &[VehicleTypePce],
    counts: &LinkVolumesByClass,
) -> Result<(), AnalysisError> {
    let mut writer = table_writer(path, "link_hourly_by_class.csv")?;
    writeln!(
        writer,
        "vehicle_class,link_id,hour_start_seconds,entry_vehicles,exit_vehicles"
    )
    .map_err(io_error)?;
    for class in classes
        .iter()
        .filter(|class| class.vehicle_type_id != "all")
    {
        let class_counts = counts.get(&class.vehicle_type_id);
        for hour in hours {
            for link in links {
                let volumes = class_counts
                    .and_then(|counts| counts.get(hour))
                    .and_then(|links| links.get(link.id.external()))
                    .copied()
                    .unwrap_or_default();
                writeln!(
                    writer,
                    "{},{},{hour},{},{}",
                    csv(&class.vehicle_type_id),
                    csv(link.id.external()),
                    volumes.entries,
                    volumes.exits
                )
                .map_err(io_error)?;
            }
        }
    }
    Ok(())
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
    let statuses = module_statuses(
        &RequiredOutcome::Failed(error.to_string()),
        None,
        None,
        None,
        None,
        None,
        &TransitOutcome::default(),
        None,
    );
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

/// What the transit modules computed, for their status rows.
#[derive(Default)]
struct TransitOutcome<'a> {
    has_service_records: bool,
    validation: Option<&'a Result<(), String>>,
}

fn module_statuses(
    outcome: &RequiredOutcome,
    validation: Option<&Result<(), String>>,
    comparison: Option<&Result<(), String>>,
    accessibility: Option<&Result<(), String>>,
    service: Option<&Result<(), String>>,
    demographics: Option<&Result<(), String>>,
    transit: &TransitOutcome<'_>,
    survey: Option<&Result<(), String>>,
) -> Vec<ModuleStatus> {
    let (status, reason) = match outcome {
        RequiredOutcome::Complete => (STATUS_COMPLETE, None),
        RequiredOutcome::Failed(reason) => (STATUS_FAILED, Some(reason.clone())),
    };
    let mut statuses = vec![ModuleStatus {
        module: REQUIRED_MODULE,
        required: true,
        status,
        reason: reason.clone(),
    }];
    statuses.extend(OPTIONAL_MODULES.iter().map(|(module, unavailable)| {
        let computed_status = match *module {
            "transit_performance" => Some(if transit.has_service_records {
                (STATUS_COMPLETE, None)
            } else {
                (
                    STATUS_UNAVAILABLE,
                    Some("No transit service records in the final iteration".to_owned()),
                )
            }),
            "transit_validation" => match transit.validation {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No transit observation dataset is configured".to_owned()),
                )),
            },
            "validation" => match validation {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No observed validation dataset is configured".to_owned()),
                )),
            },
            "cross_run_comparison" => match comparison {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No comparison runs are configured".to_owned()),
                )),
            },
            "accessibility" => match accessibility {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No accessibility inputs are configured".to_owned()),
                )),
            },
            "service_performance" => match service {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No service records are configured".to_owned()),
                )),
            },
            demographic::MODULE => match demographics {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No person group attributes are configured".to_owned()),
                )),
            },
            "transit_and_research" => match survey {
                Some(Ok(())) => Some((STATUS_COMPLETE, None)),
                Some(Err(reason)) => Some((STATUS_FAILED, Some(reason.clone()))),
                None => Some((
                    STATUS_UNAVAILABLE,
                    Some("No journey survey dataset is configured".to_owned()),
                )),
            },
            _ => None,
        };
        ModuleStatus {
            module,
            required: false,
            // A computed module shares the run's outcome, so a failed run cannot report it complete.
            status: if let Some((status, _)) = &computed_status {
                status
            } else if unavailable.is_none() {
                status
            } else {
                STATUS_UNAVAILABLE
            },
            reason: computed_status.and_then(|(_, reason)| reason).or_else(|| {
                unavailable
                    .map(|reason| (*reason).to_owned())
                    .or_else(|| reason.clone())
            }),
        }
    }));
    statuses
}

fn write_empty_validation(path: &Path) -> Result<(), AnalysisError> {
    for (name, content) in [
        (
            "validation_summary.csv",
            "split,metric,vehicle_class,sample_size,bias,mae,rmse,geh_mean,geh_count,unmatched_observations,undefined_relative_errors\n",
        ),
        (
            "validation_matches.csv",
            "link_id,period_start_seconds,vehicle_class,metric,split,observed,simulated_sample,expansion_factor,simulated_expanded,residual,relative_error,observation_source,source_row\n",
        ),
        (
            "validation_unmatched.csv",
            "source_row,link_id,period_start_seconds,period_end_seconds,vehicle_class,split,metric,unit,value,source,reason\n",
        ),
        (
            "validation_scatter.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"600\"><text x=\"20\" y=\"30\">No observed validation data configured</text></svg>",
        ),
        (
            "validation_scatter_count_calibration.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"600\"><text x=\"20\" y=\"30\">No observed count data configured</text></svg>",
        ),
        (
            "validation_scatter_count_holdout.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"600\"><text x=\"20\" y=\"30\">No observed count data configured</text></svg>",
        ),
        (
            "validation_scatter_speed_calibration.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"600\"><text x=\"20\" y=\"30\">No observed speed data configured</text></svg>",
        ),
        (
            "validation_scatter_speed_holdout.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"600\"><text x=\"20\" y=\"30\">No observed speed data configured</text></svg>",
        ),
        (
            "validation_time_profiles.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"400\"><text x=\"20\" y=\"30\">No observed validation data configured</text></svg>",
        ),
        (
            "validation_time_profiles_calibration.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"400\"><text x=\"20\" y=\"30\">No calibration count data configured</text></svg>",
        ),
        (
            "validation_time_profiles_holdout.svg",
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"400\"><text x=\"20\" y=\"30\">No holdout count data configured</text></svg>",
        ),
    ] {
        fs::write(path.join(name), content).map_err(io_error)?;
    }
    fs::copy(
        path.join("network_map.svg"),
        path.join("validation_residual_map.svg"),
    )
    .map_err(io_error)?;
    fs::copy(
        path.join("network_map.svg"),
        path.join("validation_residual_map_calibration.svg"),
    )
    .map_err(io_error)?;
    fs::copy(
        path.join("network_map.svg"),
        path.join("validation_residual_map_holdout.svg"),
    )
    .map_err(io_error)?;
    Ok(())
}

/// Every metric the report exports, so a consumer of the catalog can look a metric up in the
/// table that describes it.
///
/// The `aggregation_key` of a metric names the columns that identify one of its rows, and those
/// columns are exported by the table the metric comes from. A name does not have to be a column
/// itself, because two tables can export the same column name for different metrics: coverage.csv
/// and group_coverage.csv both carry `used_links`, which the catalog distinguishes as
/// `used_links` and `group_used_links`.
fn metrics(include_clipped_delay: bool, include_accessibility: bool) -> Vec<Metric<'static>> {
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
        // Every name between here and the histogram rows is a column of link_capacity.csv, so a
        // consumer of the catalog can look each metric up by name in the table that describes it.
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
        Metric {
            name: "share",
            unit: "proportion",
            aggregation_key: "departure_hour_seconds,purpose,distance_class,main_mode",
        },
        Metric {
            name: "journeys",
            unit: "journeys",
            aggregation_key: "departure_hour_seconds,purpose,distance_class,main_mode",
        },
        Metric {
            name: "mean_duration_seconds",
            unit: "seconds",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "std_duration_seconds",
            unit: "seconds",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "median_duration_seconds",
            unit: "seconds",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "p90_duration_seconds",
            unit: "seconds",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "mean_distance_meters",
            unit: "meters",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "std_distance_meters",
            unit: "meters",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "median_distance_meters",
            unit: "meters",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "p90_distance_meters",
            unit: "meters",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "observed_journey_weight",
            unit: "weighted_journeys",
            aggregation_key: "split,metric,category",
        },
        Metric {
            name: "observed_journey_share",
            unit: "proportion",
            aggregation_key: "split,metric,category",
        },
        Metric {
            name: "simulated_journeys",
            unit: "journeys",
            aggregation_key: "split,metric,category",
        },
        Metric {
            name: "simulated_journey_share",
            unit: "proportion",
            aggregation_key: "split,metric,category",
        },
        Metric {
            name: "survey_uncertainty",
            unit: "weighted_journeys",
            aggregation_key: "split,metric,category",
        },
        Metric {
            name: "completed",
            unit: "journeys",
            aggregation_key: "main_mode,purpose",
        },
        Metric {
            name: "duration_seconds",
            unit: "seconds",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "distance_meters",
            unit: "meters",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "distance_class",
            unit: "category",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "distance_provenance",
            unit: "category",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "completion",
            unit: "category",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "departure_hour_seconds",
            unit: "seconds",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "purpose",
            unit: "category",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "origin",
            unit: "activity_type",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "destination",
            unit: "activity_type",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "origin_link",
            unit: "link_id",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "destination_link",
            unit: "link_id",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "main_mode",
            unit: "category",
            aggregation_key: "person_id,journey_index",
        },
        Metric {
            name: "component_modes",
            unit: "category_list",
            aggregation_key: "person_id,journey_index",
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
        // Link speeds follow the volumes they are reconstructed from, so the catalog lists them
        // next to the link metrics. The histogram bins are the grouped observation counts, and the
        // traversal records are counted by the module that writes the table, so it owns their names.
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
        Metric {
            name: "sample_size",
            unit: "observations",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "bias",
            unit: "metric units",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "mae",
            unit: "metric units",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "rmse",
            unit: "metric units",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "geh_mean",
            unit: "GEH",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "geh_count",
            unit: "observations",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "unmatched_observations",
            unit: "observations",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "undefined_relative_errors",
            unit: "observations",
            aggregation_key: "split,metric,vehicle_class",
        },
        Metric {
            name: "iteration",
            unit: "iterations",
            aggregation_key: "run,iteration",
        },
        Metric {
            name: "simulated_sample",
            unit: "run metric unit",
            aggregation_key: "run,iteration,metric,link_id,period_start_seconds,vehicle_class",
        },
        Metric {
            name: "sample_size",
            unit: "fraction",
            aggregation_key: "run,iteration",
        },
        Metric {
            name: "population_value",
            unit: "vehicles or m/s",
            aggregation_key: "run,iteration,metric,link_id,period_start_seconds,vehicle_class",
        },
        Metric {
            name: "period_end_seconds",
            unit: "seconds",
            aggregation_key: "run,iteration,link_id,period_start_seconds",
        },
    ]
    .into_iter()
    .chain(
        link_speed::SpeedDiagnostics::METRICS
            .iter()
            .map(|name| Metric {
                name,
                unit: "records",
                // link_speed_diagnostics.csv keys one row per report by its metric name.
                aggregation_key: "metric",
            }),
    )
    .chain([
        Metric {
            name: "vehicle_distance_meters",
            unit: "m",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "vehicle_time_seconds",
            unit: "s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "free_flow_relative_delay_seconds",
            unit: "s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "relative_speed_ratio",
            unit: "ratio",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "network_vehicle_distance_meters",
            unit: "m",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "network_vehicle_time_seconds",
            unit: "s",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "network_free_flow_relative_delay_seconds",
            unit: "s",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "network_relative_speed_ratio",
            unit: "ratio",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "en_route_departures",
            unit: "persons",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "en_route_arrivals",
            unit: "persons",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "en_route_stuck_events",
            unit: "persons",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "agents_at_interval_start",
            unit: "persons",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "peak_agents",
            unit: "persons",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "en_route_person_seconds",
            unit: "person_seconds",
            aggregation_key: "hour_start_seconds",
        },
        Metric {
            name: "passenger_distance_meters",
            unit: "m",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "passenger_time_seconds",
            unit: "s",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "passenger_data_status",
            unit: "category",
            aggregation_key: "link_id,hour_start_seconds",
        },
        Metric {
            name: "unfinished_traversals",
            unit: "traversals",
            aggregation_key: "metric",
        },
        Metric {
            name: "unmatched_leave_events",
            unit: "events",
            aggregation_key: "metric",
        },
        Metric {
            name: "invalid_positions",
            unit: "traversals",
            aggregation_key: "metric",
        },
        Metric {
            name: "invalid_link_lengths",
            unit: "traversals",
            aggregation_key: "metric",
        },
        Metric {
            name: "non_positive_durations",
            unit: "traversals",
            aggregation_key: "metric",
        },
        Metric {
            name: "invalid_reference_speeds",
            unit: "traversals",
            aggregation_key: "metric",
        },
        Metric {
            name: "requests",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "served",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "rejected",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "unserved",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "served_share",
            unit: "proportion",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "rejected_share",
            unit: "proportion",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "passengers_served",
            unit: "passengers",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "wait_mean_seconds",
            unit: "seconds",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "wait_std_seconds",
            unit: "seconds",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "wait_median_seconds",
            unit: "seconds",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "wait_p90_seconds",
            unit: "seconds",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "detour_mean_ratio",
            unit: "ratio",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "detour_std_ratio",
            unit: "ratio",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "detour_median_ratio",
            unit: "ratio",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "detour_p90_ratio",
            unit: "ratio",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "wait_limit_exceeded",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "inside_area",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "outside_area",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "area_unknown",
            unit: "requests",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "coverage_share",
            unit: "proportion",
            aggregation_key: "scope,group",
        },
        Metric {
            name: "service_seconds",
            unit: "seconds",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "busy_seconds",
            unit: "seconds",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "utilization",
            unit: "proportion",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "driven_meters",
            unit: "meters",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "occupied_meters",
            unit: "meters",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "empty_meters",
            unit: "meters",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "empty_share",
            unit: "proportion",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "passenger_meters",
            unit: "passenger-meters",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "mean_occupancy",
            unit: "passengers",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "load_factor",
            unit: "proportion",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "capacity_exceeded_tasks",
            unit: "tasks",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "requests_served",
            unit: "requests",
            aggregation_key: "scope,vehicle_id",
        },
        Metric {
            name: "load_vehicle_meters",
            unit: "meters",
            aggregation_key: "load_passengers",
        },
        Metric {
            name: "load_share",
            unit: "proportion",
            aggregation_key: "load_passengers",
        },
    ])
    .chain(
        transit::METRICS
            .iter()
            .map(|(name, unit, aggregation_key)| Metric {
                name,
                unit,
                aggregation_key,
            }),
    )
    .chain(
        demographic::METRICS
            .iter()
            .map(|(name, unit, aggregation_key)| Metric {
                name,
                unit,
                aggregation_key,
            }),
    )
    .chain(if include_clipped_delay {
        vec![
            Metric {
                name: "clipped_excess_delay_seconds",
                unit: "s",
                aggregation_key: "link_id,hour_start_seconds",
            },
            Metric {
                name: "network_clipped_excess_delay_seconds",
                unit: "s",
                aggregation_key: "hour_start_seconds",
            },
        ]
    } else {
        Vec::new()
    })
    .chain(
        include_accessibility
            .then(accessibility_metrics)
            .into_iter()
            .flatten(),
    )
    .collect()
}

/// Catalog entries of the accessibility module.
///
/// Registering the measure here is what makes it available to the comparison and equity
/// consumers: they read the catalog rather than the module, so a metric that is computed but
/// not registered would be invisible to them. The aggregation key names the origin, the
/// category, the mode, the departure period and the threshold, which are exactly the
/// dimensions a comparison needs to line two runs up on.
///
/// The catalog names the column, per the rule in `docs/architecture.md`, so the measure is
/// registered as `opportunities` — the column that carries it. The declared measure name is in
/// that table's own `measure` column, which is what tells a consumer which definition a
/// `opportunities` value was computed under.
fn accessibility_metrics() -> Vec<Metric<'static>> {
    vec![
        Metric {
            name: "opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_AGGREGATION_KEY,
        },
        Metric {
            name: "reachable_opportunity_share",
            unit: "proportion",
            aggregation_key: ACCESSIBILITY_AGGREGATION_KEY,
        },
        Metric {
            name: "reachable_opportunity_locations",
            unit: "locations",
            aggregation_key: ACCESSIBILITY_AGGREGATION_KEY,
        },
        Metric {
            name: "unreachable_opportunity_locations",
            unit: "locations",
            aggregation_key: ACCESSIBILITY_AGGREGATION_KEY,
        },
        Metric {
            name: "opportunity_locations_without_cost",
            unit: "locations",
            aggregation_key: ACCESSIBILITY_AGGREGATION_KEY,
        },
        Metric {
            name: "total_opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_AGGREGATION_KEY,
        },
        // The equity pair: the same measure summarised once per zone and once per person.
        Metric {
            name: "mean_opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
        Metric {
            name: "population_weighted_opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
        Metric {
            name: "median_opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
        Metric {
            name: "min_opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
        Metric {
            name: "max_opportunities",
            unit: "opportunities",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
        Metric {
            name: "zones_without_costs",
            unit: "zones",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
        Metric {
            name: "persons_included",
            unit: "persons",
            aggregation_key: ACCESSIBILITY_SUMMARY_KEY,
        },
    ]
}

/// Build the accessibility part of the local report, and the script that fills it.
///
/// The section is derived from the module's own status rather than from a separate flag, so
/// the page cannot claim tables the module did not write. Each table is embedded as a bounded
/// preview and links to the CSV that holds every row.
fn accessibility_section(
    path: &Path,
    statuses: &[ModuleStatus],
) -> Result<(String, String), AnalysisError> {
    let Some(status) = statuses
        .iter()
        .find(|status| status.module == "accessibility")
    else {
        return Ok((String::new(), String::new()));
    };
    if status.status != STATUS_COMPLETE {
        return Ok((
            format!(
                "<h2>Accessibility to supplied opportunities</h2><p>Unavailable: {}. See <a href=\"accessibility_diagnostics.csv\">accessibility_diagnostics.csv</a>.</p>",
                escape_html(status.reason.as_deref().unwrap_or(STATUS_UNAVAILABLE))
            ),
            String::new(),
        ));
    }
    let map = fs::read_to_string(path.join("accessibility_map.svg")).map_err(io_error)?;
    let (zones, zones_truncated) = csv_preview_for_script(
        &path.join("accessibility_zones.csv"),
        ACCESSIBILITY_PREVIEW_ROWS,
    )?;
    let summary = csv_for_script(&path.join("accessibility_summary.csv"))?;
    let (persons, persons_truncated) = csv_preview_for_script(
        &path.join("accessibility_persons.csv"),
        ACCESSIBILITY_PREVIEW_ROWS,
    )?;
    let diagnostics = csv_for_script(&path.join("accessibility_diagnostics.csv"))?;
    let preview = |truncated: bool, table: &str, rows: usize| {
        if truncated {
            format!(
                "Showing the first {rows} rows of <a href=\"{table}\">{table}</a>, which holds every row."
            )
        } else {
            format!("Every row is listed in <a href=\"{table}\">{table}</a>.")
        }
    };
    let section = format!(
        "<h2>Accessibility to supplied opportunities</h2><p>The declared measure is <code>{}</code>: the summed weight of every supplied opportunity whose <em>potential</em> travel cost from the origin is at or below the threshold. Costs come from the supplied cost file only. A realized trip duration is not a potential destination, so the observed leg and journey tables above are never used as a substitute. A zone with no supplied cost from it has no value and is exported with a status rather than as zero.</p><h3>Accessibility map</h3><p>One panel per category, mode, departure period and threshold, on a shared projection. A filled circle is an origin zone, shaded from the lowest to the highest value in its own panel; a gray circle is an origin with no supplied cost, which is not a low value. A green ring is a zone holding opportunities of that category, sized by their total weight.</p><div id=\"accessibility-map\">{map}</div><h3>Cumulative opportunities by origin zone</h3><p>{}</p><div id=\"accessibility-zones\"></div><h3>Accessibility totals and equity comparison</h3><p>The mean describes the zones that have a supplied cost; the population-weighted mean describes what a person in this run's population actually faces. The two differ exactly when opportunities are unevenly distributed over people. A zone without any supplied cost is counted in <code>zones_without_costs</code> and left out of the statistics rather than counted as holding zero opportunities. A zone with <em>some</em> missing costs is included, so read <code>zones_without_costs</code> together with the missing-cost counts in the zone table above.</p><div id=\"accessibility-summary\"></div><h3>Per-person accessibility</h3><p>A person is placed in the zone nearest their first non-stage activity. {}</p><div id=\"accessibility-persons\"></div><h3>Accessibility inputs and coverage</h3><div id=\"accessibility-diagnostics\"></div><p><a href=\"accessibility_zones.csv\">origin zones (CSV)</a> · <a href=\"accessibility_summary.csv\">totals and equity comparison (CSV)</a> · <a href=\"accessibility_persons.csv\">per person (CSV)</a> · <a href=\"accessibility_diagnostics.csv\">input coverage (CSV)</a> · <a href=\"accessibility_map.svg\">map (SVG)</a></p>",
        accessibility::MEASURE,
        preview(
            zones_truncated,
            "accessibility_zones.csv",
            ACCESSIBILITY_PREVIEW_ROWS
        ),
        preview(
            persons_truncated,
            "accessibility_persons.csv",
            ACCESSIBILITY_PREVIEW_ROWS
        ),
    );
    // The declarations are emitted before the other tables render, because the script that
    // fills these tables runs earlier in the same inline block.
    let scripts = format!(
        "const az={zones};const asum={summary};const ap={persons};const ad={diagnostics};csvTable('#accessibility-zones',az);csvTable('#accessibility-summary',asum);csvTable('#accessibility-persons',ap);csvTable('#accessibility-diagnostics',ad);"
    );
    Ok((section, scripts))
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
    pce_by_vehicle: &BTreeMap<&str, f64>,
    class_by_vehicle: &BTreeMap<&str, &str>,
    counts: &mut LinkVolumesByHour,
    counts_by_class: &mut LinkVolumesByClass,
) {
    let Some(visit) = link_visit(event) else {
        return;
    };
    // The same crossing event feeds both reports: the side and the vehicle decide which PCE
    // total it lands in, and the position along the link decides its speed.
    let (link, vehicle, side) = match &visit {
        LinkVisit::Enter { link, vehicle, .. } => (link, vehicle, FlowSide::Entry),
        LinkVisit::Leave { link, vehicle, .. } => (link, vehicle, FlowSide::Exit),
    };
    let id = link.external();
    if !ids.contains(id) {
        return;
    }
    let hour = hour_start_seconds(time.as_nanos(), interval);
    // Vehicles that are not in the recorded catalog, e.g. transit or DRT units, leave the
    // PCE total for the interval unusable instead of silently counting as zero.
    let pce = pce_by_vehicle.get(vehicle.external()).copied();
    counts
        .entry(hour)
        .or_default()
        .entry(id.to_owned())
        .or_default()
        .record(side, pce);
    if let Some(class) = class_by_vehicle.get(vehicle.external()) {
        counts_by_class
            .entry((*class).to_owned())
            .or_default()
            .entry(hour)
            .or_default()
            .entry(id.to_owned())
            .or_default()
            .record(side, None);
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
    let mut hours: BTreeSet<_> = counts.keys().copied().collect();
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
            let volumes = volumes_of(counts, hour, link.id.external());
            if volumes.entries + volumes.exits > 0 {
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
        .values()
        .flat_map(|links| links.iter())
        .filter(|(_, volumes)| volumes.entries + volumes.exits > 0)
        .map(|(link_id, _)| link_id.as_str())
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
        let data_attributes = format!(
            "{data_attributes} data-link-id=\"{}\"",
            xml_escape(link.id.external())
        );
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
                let volumes = volumes_of(counts, hour, link.id.external());
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
    expected_journeys: &BTreeMap<String, Vec<ExpectedJourney>>,
    interval: u32,
    simulation_end_time: u32,
    sample_size: f64,
    link_hourly: &[LinkHourlyMetric],
) -> Result<(), AnalysisError> {
    let mut hourly = BufWriter::new(File::create(path.join("link_hourly.csv")).map_err(io_error)?);
    writeln!(
        hourly,
        "link_id,hour_start_seconds,entry_vehicles,exit_vehicles"
    )
    .map_err(io_error)?;
    // The hourly rows are built once, with each link's classification attached, so the map and
    // the group tables filter exactly the rows this table exports.
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
    let mut capacity =
        BufWriter::new(File::create(path.join("link_capacity.csv")).map_err(io_error)?);
    writeln!(
        capacity,
        "link_id,interval_start_seconds,capacity_pce_per_hour,effective_capacity_pce,permlanes,interval_hours,sample_size,entry_vehicles,exit_vehicles,entry_pce,exit_pce,entry_unresolved_pce,exit_unresolved_pce,entry_pce_scaled,exit_pce_scaled,entry_flow_pce_per_hour,exit_flow_pce_per_hour,entry_vc,exit_vc,entry_vc_status,exit_vc_status"
    )
    .map_err(io_error)?;

    // One pass over the intervals and the links feeds the coverage and per-link tables, so a
    // link's volumes are looked up and its utilization derived exactly once. The interval width
    // is derived per interval, because the final one can be shorter than the configured interval.
    let mut histograms: BTreeMap<u64, IntervalHistograms> = BTreeMap::new();
    for hour in interval_starts(counts, interval, simulation_end_time) {
        let interval_hours = covered_interval_hours(hour, interval, simulation_end_time);
        let interval_histograms = histograms.entry(hour).or_default();
        let mut used = 0usize;
        for link in links {
            let link_id = link.id.external();
            let volumes = volumes_of(counts, hour, link_id);
            used += usize::from(volumes.entries + volumes.exits > 0);
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
    let mut legs = BufWriter::new(File::create(path.join("legs.csv")).map_err(io_error)?);
    writeln!(legs, "person_id,leg_index,mode,departure_seconds,departure_hour_seconds,arrival_seconds,duration_seconds,status").map_err(io_error)?;
    let mut by_mode_hour = BTreeMap::<ModeHour, HourlyLegs>::new();
    let mut person_totals = BTreeMap::<String, f64>::new();
    let mut person_activity = BTreeMap::<String, PersonActivity>::new();
    let mut journey_rows = Vec::new();
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
    let observed_by_key: BTreeMap<_, _> = observed_legs
        .iter()
        .map(|leg| ((leg.person_id.as_str(), leg.leg_index), leg))
        .collect();
    for (person, journeys) in expected_journeys {
        for journey in journeys {
            let components: Vec<_> = journey
                .leg_indices
                .iter()
                .filter_map(|index| observed_by_key.get(&(person.as_str(), *index)).copied())
                .collect();
            let complete = components.len() == journey.leg_indices.len()
                && components
                    .iter()
                    .all(|leg| matches!(leg.completion, LegCompletion::Completed { .. }));
            let departure_seconds = components.first().map(|leg| leg.departure_seconds);
            let departure_hour = departure_seconds
                .map(|seconds| (seconds as u64) / u64::from(interval) * u64::from(interval));
            let duration_seconds = if complete {
                components.first().and_then(|first| {
                    components.last().and_then(|last| {
                        last.completion
                            .arrival_seconds()
                            .map(|arrival| arrival - first.departure_seconds)
                    })
                })
            } else {
                None
            };
            journey_rows.push(JourneyRow {
                person_id: person.clone(),
                journey_index: journey.journey_index,
                departure_seconds,
                departure_hour,
                origin: journey.origin.clone(),
                destination: journey.destination.clone(),
                origin_link: journey.origin_link.clone(),
                destination_link: journey.destination_link.clone(),
                purpose: journey.purpose.clone(),
                main_mode: analysis_main_mode(&journey.component_modes),
                component_modes: journey.component_modes.join("|"),
                component_leg_indices: journey
                    .leg_indices
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<_>>()
                    .join("|"),
                duration_seconds,
                completion: if components.is_empty() {
                    "not_departed"
                } else if complete {
                    "completed"
                } else if components
                    .iter()
                    .any(|leg| matches!(leg.completion, LegCompletion::Stuck))
                {
                    "stuck"
                } else if components
                    .iter()
                    .any(|leg| matches!(leg.completion, LegCompletion::MissingArrival))
                {
                    "missing_arrival"
                } else {
                    "incomplete"
                },
                distance_meters: journey.distance_meters,
                distance_provenance: journey.distance_provenance.clone(),
            });
        }
    }
    write_journeys(path, &journey_rows)?;
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

const ANALYSIS_MODE_HIERARCHY: &[&str] = &[
    "non_network_walk",
    "undefined",
    "transit_walk",
    "other",
    "walk",
    "bike",
    "taxi",
    "drt",
    "ride",
    "motorcycle",
    "truck",
    "car",
    "pt",
    "train",
    "ship",
    "airplane",
    "freight",
];

/// Mirrors MATSim's default analysis hierarchy: the highest ranked component wins, so access
/// walks collapse into transit journeys and repeated transit legs remain one journey.
fn analysis_main_mode(modes: &[String]) -> String {
    let known = modes
        .iter()
        .filter_map(|mode| {
            ANALYSIS_MODE_HIERARCHY
                .iter()
                .position(|known| *known == mode)
                .map(|rank| (rank, mode))
        })
        .max_by_key(|(rank, _)| *rank);
    let mut unknown: Vec<_> = modes
        .iter()
        .filter(|mode| !ANALYSIS_MODE_HIERARCHY.contains(&mode.as_str()))
        .collect();
    unknown.sort();
    unknown.dedup();
    if !unknown.is_empty()
        && known.is_none_or(|(rank, _)| {
            rank <= ANALYSIS_MODE_HIERARCHY
                .iter()
                .position(|mode| *mode == "walk")
                .unwrap()
        })
    {
        return if unknown.len() == 1 {
            unknown[0].to_string()
        } else {
            "multiple_unknown_modes".to_owned()
        };
    }
    if !unknown.is_empty() {
        return "unknown_mixed_modes".to_owned();
    }
    known.map_or_else(|| "undefined".to_owned(), |(_, mode)| mode.clone())
}

pub(super) fn distance_class(distance_meters: Option<f64>) -> &'static str {
    match distance_meters {
        Some(distance) if distance < 1_000.0 => "under_1_km",
        Some(distance) if distance < 5_000.0 => "1_to_5_km",
        Some(distance) if distance < 10_000.0 => "5_to_10_km",
        Some(distance) if distance < 25_000.0 => "10_to_25_km",
        Some(_) => "25_km_or_more",
        None => "unknown",
    }
}

fn write_journeys(path: &Path, journeys: &[JourneyRow]) -> Result<(), AnalysisError> {
    let mut journey_table =
        BufWriter::new(File::create(path.join("journeys.csv")).map_err(io_error)?);
    writeln!(journey_table, "person_id,journey_index,departure_seconds,departure_hour_seconds,origin,destination,origin_link,destination_link,purpose,main_mode,component_modes,component_leg_indices,duration_seconds,completion,distance_meters,distance_class,distance_provenance").map_err(io_error)?;
    for journey in journeys {
        writeln!(
            journey_table,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv(&journey.person_id),
            journey.journey_index,
            number_opt(journey.departure_seconds),
            journey
                .departure_hour
                .map_or_else(String::new, |hour| hour.to_string()),
            csv(&journey.origin),
            csv(&journey.destination),
            csv(&journey.origin_link),
            csv(&journey.destination_link),
            csv(&journey.purpose),
            csv(&journey.main_mode),
            csv(&journey.component_modes),
            csv(&journey.component_leg_indices),
            number_opt(journey.duration_seconds),
            journey.completion,
            number_opt(journey.distance_meters),
            distance_class(journey.distance_meters),
            journey.distance_provenance,
        )
        .map_err(io_error)?;
    }

    type ShareKey = (u64, String, String, String);
    let mut totals = BTreeMap::<(u64, String, String), u64>::new();
    let mut shares = BTreeMap::<ShareKey, u64>::new();
    let mut durations = BTreeMap::<(String, String), Vec<f64>>::new();
    let mut distances = BTreeMap::<(String, String), Vec<f64>>::new();
    for journey in journeys {
        let (Some(hour), Some(main_mode)) = (journey.departure_hour, Some(&journey.main_mode))
        else {
            continue;
        };
        let class = distance_class(journey.distance_meters).to_owned();
        let key = (hour, journey.purpose.clone(), class.clone());
        *totals.entry(key.clone()).or_default() += 1;
        *shares
            .entry((hour, journey.purpose.clone(), class, main_mode.clone()))
            .or_default() += 1;
        if let Some(duration) = journey.duration_seconds {
            durations
                .entry((main_mode.clone(), journey.purpose.clone()))
                .or_default()
                .push(duration);
        }
        if let Some(distance) = journey.distance_meters {
            distances
                .entry((main_mode.clone(), journey.purpose.clone()))
                .or_default()
                .push(distance);
        }
    }
    let mut share_table =
        BufWriter::new(File::create(path.join("journey_mode_share.csv")).map_err(io_error)?);
    writeln!(
        share_table,
        "departure_hour_seconds,purpose,distance_class,main_mode,journeys,share"
    )
    .map_err(io_error)?;
    for ((hour, purpose, class, mode), count) in shares {
        let total = totals[&(hour, purpose.clone(), class.clone())];
        writeln!(
            share_table,
            "{hour},{},{},{},{count},{:.6}",
            csv(&purpose),
            class,
            csv(&mode),
            count as f64 / total as f64,
        )
        .map_err(io_error)?;
    }
    let mut summary =
        BufWriter::new(File::create(path.join("journey_summary.csv")).map_err(io_error)?);
    writeln!(summary, "main_mode,purpose,journeys,completed,mean_duration_seconds,std_duration_seconds,median_duration_seconds,p90_duration_seconds,mean_distance_meters,std_distance_meters,median_distance_meters,p90_distance_meters").map_err(io_error)?;
    let groups: BTreeSet<_> = journeys
        .iter()
        .map(|journey| (journey.main_mode.as_str(), journey.purpose.as_str()))
        .collect();
    for (mode, purpose) in groups {
        let key = (mode.to_owned(), purpose.to_owned());
        let mut times = durations.remove(&key).unwrap_or_default();
        times.sort_by(f64::total_cmp);
        let mean_duration = mean(&times);
        let std = std_dev(&times);
        let mut distances = distances.remove(&key).unwrap_or_default();
        distances.sort_by(f64::total_cmp);
        let completed = journeys
            .iter()
            .filter(|journey| {
                journey.main_mode == mode
                    && journey.purpose == purpose
                    && journey.duration_seconds.is_some()
            })
            .count();
        writeln!(
            summary,
            "{},{},{},{completed},{},{},{},{},{},{},{},{}",
            csv(mode),
            csv(purpose),
            journeys
                .iter()
                .filter(|journey| journey.main_mode == mode && journey.purpose == purpose)
                .count(),
            number_opt(mean_duration),
            number_opt(std),
            number_opt(quantile(&times, 0.5)),
            number_opt(quantile(&times, 0.9)),
            number_opt(mean(&distances)),
            number_opt(std_dev(&distances)),
            number_opt(quantile(&distances, 0.5)),
            number_opt(quantile(&distances, 0.9)),
        )
        .map_err(io_error)?;
    }
    Ok(())
}

fn mean(values: &[f64]) -> Option<f64> {
    (!values.is_empty()).then(|| values.iter().sum::<f64>() / values.len() as f64)
}

fn std_dev(values: &[f64]) -> Option<f64> {
    let average = mean(values)?;
    Some(
        (values
            .iter()
            .map(|value| (value - average).powi(2))
            .sum::<f64>()
            / values.len() as f64)
            .sqrt(),
    )
}

fn quantile(sorted: &[f64], quantile: f64) -> Option<f64> {
    if sorted.is_empty() {
        return None;
    }
    let index = ((sorted.len() - 1) as f64 * quantile).ceil() as usize;
    Some(sorted[index])
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

/// One exported CSV file rendered as a table of the report.
struct ReportTable {
    /// JavaScript variable and DOM id of the rendered table.
    name: &'static str,
    title: &'static str,
    file: &'static str,
}

/// The speed tables hold no quoted field, so their lines split on commas. The agent travel tables
/// quote person identifiers and are rendered with the quote-aware parser instead.
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

/// The transit tables of the report, each with the file that holds every row.
const TRANSIT_TABLES: &[(&str, &str, &str)] = &[
    (
        "transit-availability",
        "Metric availability",
        "transit_availability.csv",
    ),
    ("transit-outcomes", "Trip outcomes", "transit_outcomes.csv"),
    (
        "transit-stops",
        "Boardings and alightings by line and stop",
        "transit_stop_hourly.csv",
    ),
    (
        "transit-lines",
        "Waiting, in-vehicle time and delay by line",
        "transit_line_summary.csv",
    ),
    (
        "transit-occupancy",
        "Occupancy and load factor by departure and segment",
        "transit_occupancy.csv",
    ),
    (
        "transit-journeys",
        "Access, egress and transfers by journey",
        "transit_journeys.csv",
    ),
    (
        "transit-trips",
        "Passenger transit trips",
        "transit_trips.csv",
    ),
    (
        "transit-validation-summary",
        "Observed demand summary",
        "transit_validation_summary.csv",
    ),
    (
        "transit-validation-matches",
        "Matched observed demand",
        "transit_validation_matches.csv",
    ),
    (
        "transit-validation-unmatched",
        "Unmatched observed demand",
        "transit_validation_unmatched.csv",
    ),
];

const TRANSIT_NOTE: &str = "Public transport is modeled by teleportation in this build: each passenger trip is recorded with its line, route, access and egress stop and scheduled boarding time, and no transit vehicle drives through the network. Waiting is the scheduled boarding time minus the passenger's departure at the stop, in-vehicle time is the arrival minus the boarding time, and arrival delay compares the arrival with the schedule. A passenger reaching the stop after the scheduled departure is a missed service and has no waiting time. Boardings, alightings and loads are expanded by the reciprocal of the sample size; load factors divide them by the vehicle capacity declared in the vehicle file. Metrics whose inputs are absent (service records, schedule, capacity, vehicle-level service events) are left blank and listed as unavailable instead of being inferred.";

/// Section markup and script that render the transit tables.
fn transit_report(path: &Path) -> Result<(String, String), AnalysisError> {
    let mut section = format!("<h2>Public transport</h2><p>{TRANSIT_NOTE}</p>");
    let mut render = String::new();
    for (id, title, file) in TRANSIT_TABLES {
        let (rows, truncated) = csv_preview_for_script(&path.join(file), LEGS_PREVIEW_ROWS)?;
        let more = if truncated {
            format!(" Showing the first {LEGS_PREVIEW_ROWS} rows.")
        } else {
            String::new()
        };
        section.push_str(&format!(
            "<h3>{title}</h3><p><a href=\"{file}\">{file}</a>{more}</p><div id=\"{id}\"></div>"
        ));
        render.push_str(&format!("csvTable('#{id}',{rows});"));
    }
    Ok((section, render))
}

/// Explains how the link speeds are reconstructed, next to the tables they are rendered in.
const SPEED_NOTE: &str = "Speeds are reconstructed from full-link traversals and assigned to the interval in which the vehicle entered the link. The representative speed divides the total travelled distance by the total travel time; the arithmetic vehicle-speed mean and population standard deviation describe the single traversals. A link without a full-link traversal has no speed: QSim inserts a vehicle at the end of the first link of a leg, so the first link of a network leg never covers its whole length and is reported as a partial traversal instead. The traversal records table lists every record that cannot produce a full-link speed, such as those partial traversals, traversals that never finished, and records without a positive duration.";

/// Explains where the service tables come from, next to the tables themselves.
const SERVICE_NOTE: &str = "Computed only from the supplied request, passenger, fleet and schedule records; no service is simulated. A request counts as rejected only when its request record says so, never because a completed leg is missing, and a request with neither a rejection nor a passenger record is unserved. Wait is pickup minus submission; the detour ratio is in-vehicle time over the supplied direct travel time. A drive task is occupied when a served request is on board for its whole span. Metrics whose inputs were not supplied stay blank and are listed as unavailable.";

fn write_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &[ModuleStatus],
    link_hourly: &[LinkHourlyMetric],
) -> Result<(), AnalysisError> {
    let coverage = fs::read_to_string(path.join("coverage.csv")).map_err(io_error)?;
    let coverage = json_for_script(&coverage.lines().collect::<Vec<_>>())?;
    let modules = json_for_script(statuses)?;
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
    // The same two flags the published catalog was written from, so the table the report
    // renders and the catalog a consumer reads can never disagree.
    let metrics = json_for_script(&metrics(
        manifest.excess_delay_clip_seconds.is_some(),
        manifest.accessibility.is_configured(),
    ))?;
    let hourly = json_for_script(link_hourly)?;
    // The filter list comes from the same constant the CSV exporters group by, so a
    // dimension cannot be exported without also being offered as a filter.
    let dimensions = json_for_script(&FILTER_DIMENSIONS)?;
    let network_map = fs::read_to_string(path.join("network_map.svg")).map_err(io_error)?;
    let speeds = embed_tables(path, SPEED_TABLES)?;
    let leg_hourly = csv_for_script(&path.join("leg_hourly.csv"))?;
    let daily = csv_for_script(&path.join("daily_summary.csv"))?;
    let persons = csv_for_script(&path.join("person_daily.csv"))?;
    let network_link_metrics = csv_for_script(&path.join("network_distance_time.csv"))?;
    let network_summary = csv_for_script(&path.join("network_distance_time_summary.csv"))?;
    let en_route_agents = csv_for_script(&path.join("en_route_agents.csv"))?;
    let diagnostics = csv_for_script(&path.join("network_distance_time_diagnostics.csv"))?;
    let network_script = format!(
        "const nd={network_link_metrics};const ns={network_summary};const ea={en_route_agents};const dg={diagnostics};csvTable('#network-summary',ns);csvTable('#network-link-metrics',nd);csvTable('#en-route-agents',ea);csvTable('#network-distance-diagnostics',dg);const delayColumn=parseCsv(ns[0]).indexOf('free_flow_relative_delay_seconds');const ratioColumn=parseCsv(ns[0]).indexOf('relative_speed_ratio');const peak=ns.slice(1).map(parseCsv).filter(row=>row[delayColumn]!=='').sort((a,b)=>Number(b[delayColumn])-Number(a[delayColumn]))[0];const slowest=ns.slice(1).map(parseCsv).filter(row=>row[ratioColumn]!=='').sort((a,b)=>Number(a[ratioColumn])-Number(b[ratioColumn]))[0];document.querySelector('#peak-delay').textContent=(peak?`Peak interval by total signed free-flow delay: ${{peak[0]}} s (${{peak[delayColumn]}} vehicle-seconds). `:'Peak delay unavailable: no valid free-flow references. ')+(slowest?`Lowest relative-speed interval: ${{slowest[0]}} s (ratio ${{slowest[ratioColumn]}}).`:'Relative-speed profile unavailable.');"
    );
    let journeys = csv_for_script(&path.join("journeys.csv"))?;
    let journey_shares = csv_for_script(&path.join("journey_mode_share.csv"))?;
    let journey_summary = csv_for_script(&path.join("journey_summary.csv"))?;
    let journey_survey = csv_for_script(&path.join("journey_survey_comparison.csv"))?;
    // One row per leg, so only a bounded preview is embedded and the rest stays in the CSV.
    let (legs, legs_truncated) = csv_preview_for_script(&path.join("legs.csv"), LEGS_PREVIEW_ROWS)?;
    let validation_summary = csv_for_script(&path.join("validation_summary.csv"))?;
    let validation_matches = csv_preview_for_script(&path.join("validation_matches.csv"), 500)?.0;
    let cross_run = csv_for_script(&path.join("cross_run_comparison.csv"))?;
    let group_burdens = csv_for_script(&path.join("group_burdens.csv"))?;
    let group_module_outcomes = csv_for_script(&path.join("group_module_outcomes.csv"))?;
    let equity_comparison = csv_for_script(&path.join("equity_comparison.csv"))?;
    let (person_demographics, person_demographics_truncated) = csv_preview_for_script(
        &path.join("person_demographics.csv"),
        PERSON_DEMOGRAPHIC_PREVIEW_ROWS,
    )?;
    let demographic_note = format!(
        "People are grouped by configured attributes; missing values are kept as {UNKNOWN}. Group sizes use the configured weight, defaulting to one when unavailable. Only completed days contribute travel burdens. The equity criterion is {}: {}",
        demographic::EQUITY_CRITERION,
        demographic::EQUITY_CRITERION_DESCRIPTION,
    );
    let demographic_note = if person_demographics_truncated {
        format!(
            "{demographic_note} The preview shows the first {PERSON_DEMOGRAPHIC_PREVIEW_ROWS} rows; the CSV contains all people."
        )
    } else {
        demographic_note
    };
    let validation_note = "vehicle_class accepts all or a vehicle type ID. Calibration and holdout plots are kept separate.";
    let validation_plots = "<h3>Calibration count</h3><img src=\"validation_scatter_count_calibration.svg\" alt=\"Calibration count scatterplot\"><h3>Holdout count</h3><img src=\"validation_scatter_count_holdout.svg\" alt=\"Holdout count scatterplot\"><h3>Calibration speed</h3><img src=\"validation_scatter_speed_calibration.svg\" alt=\"Calibration speed scatterplot\"><h3>Holdout speed</h3><img src=\"validation_scatter_speed_holdout.svg\" alt=\"Holdout speed scatterplot\"><h3>Calibration time profile</h3><img src=\"validation_time_profiles_calibration.svg\" alt=\"Calibration observed and simulated counts by period\"><h3>Holdout time profile</h3><img src=\"validation_time_profiles_holdout.svg\" alt=\"Holdout observed and simulated counts by period\"><h3>Calibration residual map</h3><img src=\"validation_residual_map_calibration.svg\" alt=\"Calibration link count residual map\"><h3>Holdout residual map</h3><img src=\"validation_residual_map_holdout.svg\" alt=\"Holdout link count residual map\">";
    let cross_run_section = "<h2>Cross-run comparison</h2><p>Rows contain metrics from the latest completed report in each configured comparison run.</p><div id=\"cross-run\"></div><p><a href=\"cross_run_comparison.csv\">Cross-run comparison CSV</a></p>";
    let (accessibility_section, accessibility_scripts) = accessibility_section(path, statuses)?;
    let (service_requests, service_requests_truncated) =
        csv_preview_for_script(&path.join("service_requests.csv"), LEGS_PREVIEW_ROWS)?;
    let service_section = format!(
        "<h2>DRT and taxi service performance</h2><p>{SERVICE_NOTE}</p><h3>Requests by outcome</h3><div id=\"service-summary\"></div><h3>Fleet distance, occupancy and utilization</h3><div id=\"service-vehicles\"></div><h3>Driven distance by passengers on board</h3><div id=\"service-occupancy\"></div><h3>Service constraints</h3><div id=\"service-constraints\"></div><h3>Metric availability</h3><div id=\"service-availability\"></div><h3>Excluded records</h3><div id=\"service-diagnostics\"></div><h3>Requests</h3><p>{}</p><div id=\"service-requests\"></div>",
        if service_requests_truncated {
            format!(
                "Showing the first {LEGS_PREVIEW_ROWS} rows of <a href=\"service_requests.csv\">service_requests.csv</a>, which holds every request."
            )
        } else {
            "Every request is listed in <a href=\"service_requests.csv\">service_requests.csv</a>."
                .to_owned()
        }
    );
    let mut service_render = format!("csvTable('#service-requests',{service_requests});");
    for (id, file) in [
        ("service-summary", "service_summary.csv"),
        ("service-vehicles", "service_vehicles.csv"),
        ("service-occupancy", "service_occupancy.csv"),
        ("service-constraints", "service_constraints.csv"),
        ("service-availability", "service_availability.csv"),
        ("service-diagnostics", "service_diagnostics.csv"),
    ] {
        service_render.push_str(&format!(
            "csvTable('#{id}',{});",
            csv_for_script(&path.join(file))?
        ));
    }
    let survey_section = "<h2>Travel survey comparison</h2><p>Weighted survey journeys are compared using the same journey definition and distribution bins. Calibration and holdout partition survey records; both use the same simulated distribution. Denominators are shown for each split and metric; unmatched groups remain visible.</p><div id=\"journey-survey\"></div><p><a href=\"journey_survey_comparison.csv\">Survey comparison CSV</a></p>";
    let legs_note = if legs_truncated {
        format!(
            "Showing the first {LEGS_PREVIEW_ROWS} rows of <a href=\"legs.csv\">legs.csv</a>, which holds every leg."
        )
    } else {
        "Every observed and planned leg is listed in <a href=\"legs.csv\">legs.csv</a>.".to_owned()
    };
    let (transit_section, transit_render) = transit_report(path)?;
    let speed_declarations = speeds
        .iter()
        .map(EmbeddedTable::declaration)
        .collect::<Vec<_>>()
        .join("");
    let speed_sections = sections(&speeds);
    let speed_renders = speeds
        .iter()
        .map(EmbeddedTable::render)
        .collect::<Vec<_>>()
        .join("");
    let html = substitute_template(
        REPORT_TEMPLATE,
        &[
            ("__ITERATION__", &manifest.iteration.to_string()),
            ("__LINKS__", &manifest.eligible_links.to_string()),
            ("__INTERVAL__", &manifest.interval_seconds.to_string()),
            ("__REPORT_STYLE__", REPORT_STYLE),
            ("__MODULE_TABLE_SCRIPT__", MODULE_TABLE_SCRIPT),
            ("__CSV_TABLE_SCRIPT__", CSV_TABLE_SCRIPT),
            ("__NETWORK_MAP__", &network_map),
            ("__DIMENSIONS__", &dimensions),
            ("__LINK_HOURLY__", &hourly),
            ("__COVERAGE__", &coverage),
            ("__LINK_CAPACITY__", &capacity),
            ("__VC_HISTOGRAM__", &histogram_rows),
            ("__METRICS__", &metrics),
            ("__MODULES__", &modules),
            ("__SPEED_DECLARATIONS__", &speed_declarations),
            ("__SPEED_SECTIONS__", &speed_sections),
            ("__SPEED_RENDERS__", &speed_renders),
            ("__SPEED_NOTE__", SPEED_NOTE),
            ("__LEG_HOURLY__", &leg_hourly),
            ("__DAILY__", &daily),
            ("__PERSONS__", &persons),
            ("__JOURNEYS__", &journeys),
            ("__JOURNEY_SHARES__", &journey_shares),
            ("__JOURNEY_SUMMARY__", &journey_summary),
            ("__LEGS__", &legs),
            ("__JOURNEY_SURVEY__", &journey_survey),
            ("__SURVEY_SECTION__", survey_section),
            ("__VALIDATION_SUMMARY__", &validation_summary),
            ("__VALIDATION_MATCHES__", &validation_matches),
            ("__VALIDATION_NOTE__", validation_note),
            ("__VALIDATION_PLOTS__", validation_plots),
            ("__CROSS_RUN_SECTION__", cross_run_section),
            (
                "__CROSS_RUN_RENDER__",
                &format!("csvTable('#cross-run',{cross_run});"),
            ),
            ("__ACCESSIBILITY_SECTION__", &accessibility_section),
            ("__ACCESSIBILITY_SCRIPTS__", &accessibility_scripts),
            ("__DEMOGRAPHIC_NOTE__", &demographic_note),
            ("__GROUP_BURDENS__", &group_burdens),
            ("__PERSON_DEMOGRAPHICS__", &person_demographics),
            ("__GROUP_MODULE_OUTCOMES__", &group_module_outcomes),
            ("__EQUITY_COMPARISON__", &equity_comparison),
            ("__SERVICE_SECTION__", &service_section),
            ("__SERVICE_RENDER__", &service_render),
            ("__TRANSIT_SECTION__", &transit_section),
            ("__TRANSIT_RENDER__", &transit_render),
            ("__LEGS_NOTE__", &legs_note),
            ("__NETWORK_ANALYSIS_SCRIPT__", &network_script),
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

/// Opens one of the exported CSV tables for writing.
fn table_writer(path: &Path, name: &str) -> Result<BufWriter<File>, AnalysisError> {
    Ok(BufWriter::new(
        File::create(path.join(name)).map_err(io_error)?,
    ))
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

    #[test]
    fn analysis_main_mode_collapses_access_walk_and_transit_transfers() {
        let modes = ["walk", "pt", "pt", "walk"].map(str::to_owned);
        assert_eq!(analysis_main_mode(&modes), "pt");
        assert_eq!(analysis_main_mode(&["walk".to_owned()]), "walk");
        assert_eq!(
            analysis_main_mode(&["custom_mode".to_owned(), "pt".to_owned()]),
            "unknown_mixed_modes"
        );
        assert_eq!(distance_class(Some(4999.0)), "1_to_5_km");
        assert_eq!(distance_class(None), "unknown");
        assert_eq!(quantile(&[10.0, 20.0, 30.0, 40.0], 0.9), Some(40.0));
    }

    #[deterministic_id_test]
    fn captures_one_journey_across_stage_activities_and_keeps_route_distance() {
        use crate::simulation::scenario::network::Link;
        use crate::simulation::scenario::population::{
            InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
            InternalPlanElement, InternalRoute, Population,
        };

        let link = || Id::<Link>::create("l");
        let activity = |kind: &str| {
            InternalPlanElement::Activity(InternalActivity::new(
                None,
                kind,
                link(),
                None,
                None,
                None,
            ))
        };
        let leg = |mode: &str, distance: f64| {
            InternalPlanElement::Leg(InternalLeg {
                mode: Id::create(mode),
                routing_mode: None,
                dep_time: None,
                trav_time: None,
                route: Some(InternalRoute::Generic(InternalGenericRoute::new(
                    link(),
                    link(),
                    None,
                    Some(distance),
                    None,
                ))),
                attributes: InternalAttributes::default(),
            })
        };
        let plan = InternalPlan {
            score: None,
            selected: true,
            elements: vec![
                activity("home"),
                leg("walk", 900.0),
                activity("pt interaction"),
                leg("pt", 3000.0),
                activity("pt interaction"),
                leg("pt", 2000.0),
                activity("pt interaction"),
                leg("walk", 800.0),
                activity("work"),
            ],
        };
        let overflow_plan = InternalPlan {
            score: None,
            selected: true,
            elements: vec![
                activity("home"),
                leg("walk", f64::MAX),
                activity("pt interaction"),
                leg("walk", f64::MAX),
                activity("work"),
            ],
        };
        let population = Population::from_persons(vec![
            InternalPerson::new(Id::create("person"), plan),
            InternalPerson::new(Id::create("overflow"), overflow_plan),
        ]);

        let expected = capture_expected_travel(&population);

        let person = expected
            .iter()
            .find(|person| person.person_id == "person")
            .unwrap();
        let overflow = expected
            .iter()
            .find(|person| person.person_id == "overflow")
            .unwrap();
        assert_eq!(person.journeys.len(), 1);
        assert_eq!(person.journeys[0].origin, "home");
        assert_eq!(person.journeys[0].destination, "work");
        assert_eq!(person.journeys[0].purpose, "work");
        assert_eq!(
            person.journeys[0].component_modes,
            ["walk", "pt", "pt", "walk"]
        );
        assert_eq!(person.journeys[0].leg_indices.len(), 4);
        assert_eq!(person.journeys[0].distance_meters, Some(6700.0));
        assert_eq!(person.journeys[0].distance_provenance, "planned_route");
        assert_eq!(overflow.journeys[0].distance_meters, None);
        assert_eq!(overflow.journeys[0].distance_provenance, "unavailable");
    }

    #[test]
    fn journey_exports_include_incomplete_trips_without_fabricating_durations() {
        let dir = tempfile::tempdir().unwrap();
        let journeys = [
            JourneyRow {
                person_id: "person".to_owned(),
                journey_index: 0,
                departure_seconds: Some(3500.0),
                departure_hour: Some(0),
                origin: "home".to_owned(),
                destination: "work".to_owned(),
                origin_link: "home-link".to_owned(),
                destination_link: "work-link".to_owned(),
                purpose: "work".to_owned(),
                main_mode: analysis_main_mode(&[
                    "walk".to_owned(),
                    "pt".to_owned(),
                    "walk".to_owned(),
                ]),
                component_modes: "walk|pt|walk".to_owned(),
                component_leg_indices: "1|3|5".to_owned(),
                duration_seconds: Some(1800.0),
                completion: "completed",
                distance_meters: Some(5000.0),
                distance_provenance: "planned_route".to_owned(),
            },
            JourneyRow {
                person_id: "person".to_owned(),
                journey_index: 1,
                departure_seconds: Some(8000.0),
                departure_hour: Some(7200),
                origin: "work".to_owned(),
                destination: "home".to_owned(),
                origin_link: "work-link".to_owned(),
                destination_link: "home-link".to_owned(),
                purpose: "home".to_owned(),
                main_mode: "car".to_owned(),
                component_modes: "car".to_owned(),
                component_leg_indices: "7".to_owned(),
                duration_seconds: None,
                completion: "incomplete",
                distance_meters: None,
                distance_provenance: "unavailable".to_owned(),
            },
        ];
        write_journeys(dir.path(), &journeys).unwrap();

        let exported = fs::read_to_string(dir.path().join("journeys.csv")).unwrap();
        assert!(exported.lines().nth(1).unwrap().contains("5_to_10_km"));
        assert!(exported.contains("\"work\",\"home\",\"work-link\",\"home-link\",\"home\",\"car\",\"car\",\"7\",,incomplete,,unknown,unavailable"));
        let shares = fs::read_to_string(dir.path().join("journey_mode_share.csv")).unwrap();
        assert!(shares.contains("0,\"work\",5_to_10_km,\"pt\",1,1.000000"));
        let summary = fs::read_to_string(dir.path().join("journey_summary.csv")).unwrap();
        assert!(summary.contains("\"pt\",\"work\",1,1,1800.000000,0.000000,1800.000000,1800.000000,5000.000000,0.000000,5000.000000,5000.000000"));
        assert!(summary.contains("\"car\",\"home\",1,0,,,,,,,,"));
    }

    #[test]
    fn cross_run_report_reads_only_each_runs_recorded_latest_iteration() {
        let root = tempfile::tempdir().unwrap();
        let mut runs = Vec::new();
        for (name, iteration, mode) in [("run-a", 2, "pt"), ("run-b", 4, "walk")] {
            let run = root.path().join(name);
            fs::create_dir_all(run.join(format!("ITERS/it.{iteration}/events"))).unwrap();
            let analysis = run.join(ANALYSIS_DIR);
            fs::create_dir_all(&analysis).unwrap();
            fs::write(
                analysis.join(MANIFEST_FILE),
                format!(
                    r#"{{"status":"complete","failure":null,"iteration":{iteration},"interval_seconds":3600,"simulation_end_time":86400,"partitions":[0],"input_format":"xml","eligible_links":0,"random_seed":1,"sample_size":1.0,"network_input":null,"population_input":null,"software_version":"test","link_labels":{{}},"urban_boundary":null}}"#
                ),
            )
            .unwrap();
            fs::write(
                analysis.join("journey_mode_share.csv"),
                format!(
                    "departure_hour_seconds,purpose,distance_class,main_mode,journeys,share\n0,work,1_to_5_km,{mode},1,1.000000\n"
                ),
            )
            .unwrap();
            runs.push(run);
        }
        let output = root.path().join("comparison-output");
        fs::create_dir_all(&output).unwrap();

        let report = compare_latest_run_reports(&output, &runs).unwrap();

        assert!(report.is_file());
        let shares =
            fs::read_to_string(report.parent().unwrap().join("journey_mode_share.csv")).unwrap();
        assert!(shares.contains("run-a,0,work,1_to_5_km,pt,1,1.000000"));
        assert!(shares.contains("run-b,0,work,1_to_5_km,walk,1,1.000000"));
        let html = fs::read_to_string(&report).unwrap();
        assert!(html.contains("latest completed iteration report"));
        let manifest = fs::read_to_string(report.parent().unwrap().join(MANIFEST_FILE)).unwrap();
        assert!(manifest.contains("\"iteration\": 2"));
        assert!(manifest.contains("\"iteration\": 4"));
    }

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
        let metadata = AnalysisRunMetadata::from_run(
            0,
            // An unsampled run, so the link tables scale nothing. These assertions cover the
            // agent travel tables, which do not depend on the fraction.
            1.0,
            &garage,
            expected_travel,
            AnalysisInputPaths {
                network: None,
                network_file: None,
                population: None,
                vehicles: None,
            },
        );
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
                ..Analysis::default()
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
        let statuses: serde_json::Value = read_json(&output.join("module_status.json")).unwrap();
        let agent_travel = statuses
            .as_array()
            .expect("module status is an array")
            .iter()
            .find(|status| status["module"] == "agent_travel")
            .expect("agent_travel module status is reported");
        assert_eq!(agent_travel["status"], "complete");
        assert!(agent_travel["reason"].is_null());
        let catalog: serde_json::Value = read_json(&output.join("metric_catalog.json")).unwrap();
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

    fn expected_person(person_id: &str, legs: &[(usize, &str)]) -> PersonExpectedTravel {
        PersonExpectedTravel {
            person_id: person_id.to_owned(),
            home_coord: None,
            legs: legs
                .iter()
                .map(|(leg_index, mode)| ExpectedLeg {
                    leg_index: *leg_index,
                    mode: (*mode).to_owned(),
                    departure_seconds: None,
                    expected_travel_seconds: None,
                    distance_meters: None,
                    transit: false,
                })
                .collect(),
            journeys: Vec::new(),
        }
    }
}
