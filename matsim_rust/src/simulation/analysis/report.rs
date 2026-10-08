use super::*;
use std::io::{BufRead, BufReader};

const SUMMARY_ROWS: usize = 200;

#[derive(Serialize)]
struct VisualTable {
    file: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    records: u64,
    columns: Vec<VisualColumn>,
    histogram: Vec<HistogramBin>,
}

#[derive(Serialize)]
struct HistogramBin {
    metric: String,
    lower: String,
    upper: String,
    links: u64,
}

#[derive(Serialize)]
struct VisualColumn {
    name: String,
    group: String,
    count: u64,
    missing: u64,
    invalid: u64,
    min: Option<f64>,
    mean: Option<f64>,
    max: Option<f64>,
}

impl VisualColumn {
    fn observe(&mut self, value: &str) {
        if value.is_empty() {
            self.missing += 1;
        } else if let Ok(value) = value.parse::<f64>()
            && value.is_finite()
        {
            self.count += 1;
            self.min = Some(self.min.map_or(value, |old| old.min(value)));
            self.max = Some(self.max.map_or(value, |old| old.max(value)));
            // Divide before subtracting to avoid overflow for opposite finite extremes.
            let old = self.mean.unwrap_or(0.0);
            self.mean = Some(old + (value / self.count as f64 - old / self.count as f64));
        } else {
            self.invalid += 1;
        }
    }
}

/// Whole-population tables stay in their CSVs. Only bounded summary rows are embedded;
/// every numeric column is reduced over all records, so the charts never sample the population.
fn visual_table(path: &Path, file: &str) -> Result<VisualTable, AnalysisError> {
    let mut reader = ::csv::Reader::from_path(path.join(file))
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    let headers = reader
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let summary = file.contains("summary")
        || file.contains("diagnostics")
        || file.contains("availability")
        || matches!(
            file,
            "coverage.csv"
                | "leg_hourly.csv"
                | "en_route_agents.csv"
                | "transit_outcomes.csv"
                | "runtime.csv"
                | "vc_histogram.csv"
        );
    let grouping = headers
        .iter()
        .enumerate()
        .filter_map(|(i, name)| {
            matches!(
                name.as_str(),
                "unit"
                    | "money_unit"
                    | "metric"
                    | "account"
                    | "pollutant"
                    | "dimension"
                    | "category"
                    | "urban_area"
                    | "road_type"
                    | "road_size"
                    | "main_mode"
                    | "mode"
                    | "purpose"
                    | "split"
                    | "scope"
                    | "cohort"
                    | "vehicle_class"
                    | "vehicle_category"
                    | "service_modeling"
                    | "outcome"
                    | "act_type"
                    | "status"
            )
            .then_some(i)
        })
        .collect::<Vec<_>>();
    let candidates = headers
        .iter()
        .enumerate()
        .filter_map(|(i, name)| (!name.ends_with("_id") && !grouping.contains(&i)).then_some(i))
        .collect::<Vec<_>>();
    let catalog = metrics(true, true);
    let mut accumulators = BTreeMap::<(usize, Vec<String>), VisualColumn>::new();
    let mut histogram = BTreeMap::<(String, String), HistogramBin>::new();
    let histogram_columns = (file == "vc_histogram.csv").then(|| {
        ["metric", "bin_lower", "bin_upper", "links"]
            .map(|name| headers.iter().position(|header| header == name))
    });
    let mut rows = Vec::new();
    let mut records = 0;
    let mut record = ::csv::StringRecord::new();
    while reader
        .read_record(&mut record)
        .map_err(|error| AnalysisError::new(error.to_string()))?
    {
        records += 1;
        if let Some([Some(metric), Some(lower), Some(upper), Some(links)]) = histogram_columns {
            let count = record[links]
                .parse::<u64>()
                .map_err(|error| AnalysisError::new(error.to_string()))?;
            let bin = histogram
                .entry((record[metric].to_owned(), record[lower].to_owned()))
                .or_insert_with(|| HistogramBin {
                    metric: record[metric].to_owned(),
                    lower: record[lower].to_owned(),
                    upper: record[upper].to_owned(),
                    links: 0,
                });
            bin.links = bin
                .links
                .checked_add(count)
                .ok_or_else(|| AnalysisError::new("V/C histogram count overflow"))?;
        }
        if summary && rows.len() < SUMMARY_ROWS {
            rows.push(
                record
                    .iter()
                    .map(|value| value.chars().take(160).collect())
                    .collect(),
            );
        }
        let group = grouping
            .iter()
            .map(|&i| record.get(i).unwrap_or_default().to_owned())
            .collect::<Vec<_>>();
        for &i in &candidates {
            accumulators
                .entry((i, group.clone()))
                .or_insert_with(|| VisualColumn {
                    name: headers[i].clone(),
                    group: grouping
                        .iter()
                        .zip(&group)
                        .map(|(&i, value)| format!("{}={value}", headers[i]))
                        .collect::<Vec<_>>()
                        .join(", "),
                    count: 0,
                    missing: 0,
                    invalid: 0,
                    min: None,
                    mean: None,
                    max: None,
                })
                .observe(record.get(i).unwrap_or_default());
        }
    }
    let columns = accumulators
        .into_values()
        .filter(|column| {
            column.count > 0 || catalog.iter().any(|metric| metric.name == column.name)
        })
        .collect();
    Ok(VisualTable {
        file: file.to_owned(),
        headers,
        rows,
        records,
        columns,
        histogram: histogram.into_values().collect(),
    })
}

pub(super) fn write_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &[ModuleStatus],
    _link_hourly: &[LinkHourlyMetric],
    _zone_system: &ZoneSystem,
) -> Result<(), AnalysisError> {
    let html = render_report(path, manifest, &statuses)?;
    fs::write(path.join("index.html"), html).map_err(io_error)
}

/// Refresh presentation without replaying events or rewriting the published metric exports.
pub(super) fn refresh_visual_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &serde_json::Value,
) -> Result<PathBuf, AnalysisError> {
    let html = render_report(path, manifest, statuses)?;
    let mut temporary = tempfile::NamedTempFile::new_in(path).map_err(io_error)?;
    std::io::Write::write_all(&mut temporary, html.as_bytes()).map_err(io_error)?;
    let report = path.join("index.html");
    temporary
        .persist(&report)
        .map_err(|error| io_error(error.error))?;
    Ok(report)
}

fn render_report(
    path: &Path,
    manifest: &Manifest,
    statuses: &impl Serialize,
) -> Result<String, AnalysisError> {
    let mut files = fs::read_dir(path)
        .map_err(io_error)?
        .map(|entry| {
            entry
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .map_err(io_error)
        })
        .collect::<Result<Vec<_>, _>>()?;
    files.sort();
    let tables = files
        .iter()
        .filter(|file| file.ends_with(".csv"))
        .map(|file| visual_table(path, file))
        .collect::<Result<Vec<_>, _>>()?;
    // The same two flags the published catalog was written from, so the report and catalog agree.
    let catalog = metrics(
        manifest.excess_delay_clip_seconds.is_some(),
        manifest.accessibility.is_configured(),
    );
    let data = json_for_script(&serde_json::json!({
        "iteration": manifest.iteration, "interval": manifest.interval_seconds,
        "end": manifest.simulation_end_time, "sample": manifest.sample_size,
        "links": manifest.eligible_links, "seed": manifest.random_seed,
        "zone_name": manifest.zone_system.name,
        "tables": tables, "modules": statuses, "catalog": catalog,
        "transit_tables": TRANSIT_TABLES,
        "dataset_descriptions": description_map(DATASET_DESCRIPTIONS),
        "metric_descriptions": description_map(METRIC_DESCRIPTIONS),
        "coverage_map": fs::metadata(path.join("network_coverage.html"))
            .and_then(|metadata| metadata.modified()).ok()
            .zip(fs::metadata(path.join("network_map.svg"))
                .and_then(|metadata| metadata.modified()).ok())
            .is_some_and(|(map, source)| map >= source)
            && path.join("network_coverage.js").is_file(),
        "notes": {"zone": ZONE_NOTE, "service": SERVICE_NOTE, "transit": TRANSIT_NOTE,
            "equity": demographic::EQUITY_CRITERION_DESCRIPTION,
            "equity_criterion": demographic::EQUITY_CRITERION},
        "maps": files.iter().filter(|file| file.ends_with(".svg")).collect::<Vec<_>>()
    }))?;
    let html = render_template(
        REPORT_TEMPLATE,
        &[
            ("__REPORT_DATA__", &data),
            ("__ITERATION__", &manifest.iteration.to_string()),
            ("__SPEED_NOTE__", SPEED_NOTE),
            ("__PATTERN_NOTE__", ACTIVITY_PATTERN_NOTE),
        ],
    );
    Ok(html)
}

pub(super) fn refresh_runtime_report(
    report: &Path,
    runtime: &AnalysisRuntimeMetadata,
) -> Result<(), AnalysisError> {
    let directory = report
        .parent()
        .ok_or_else(|| AnalysisError::new("analysis report has no parent directory"))?;
    write_runtime_tables(directory, runtime)?;
    let mut html = fs::read_to_string(report).map_err(io_error)?;
    let start = "<script id=\"report-data\" type=\"application/json\">";
    let begin = html
        .find(start)
        .map(|offset| offset + start.len())
        .ok_or_else(|| AnalysisError::new("report has no visual data"))?;
    let end = html[begin..]
        .find("</script>")
        .map(|offset| begin + offset)
        .ok_or_else(|| AnalysisError::new("report data is incomplete"))?;
    let mut data: serde_json::Value = serde_json::from_str(&html[begin..end])
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    let table = visual_table(directory, "runtime.csv")?;
    let tables = data["tables"]
        .as_array_mut()
        .ok_or_else(|| AnalysisError::new("report has no metric sources"))?;
    let old = tables
        .iter_mut()
        .find(|table| table["file"] == "runtime.csv")
        .ok_or_else(|| AnalysisError::new("report has no runtime metrics"))?;
    *old = serde_json::to_value(table).map_err(|error| AnalysisError::new(error.to_string()))?;
    html.replace_range(begin..end, &json_for_script(&data)?);
    fs::write(report, html).map_err(io_error)
}

pub(super) const REPORT_STYLE: &str = "body{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;margin-bottom:2rem}td,th{border:1px solid #ccd;padding:.5rem}a{color:#075ea8}pre{background:#f4f6f9;border:1px solid #ccd;padding:1rem;overflow:auto}";

const MODULE_TABLE_SCRIPT: &str = "function table(root,headers,rows){const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)});const body=t.createTBody();rows.forEach(row=>{const tr=body.insertRow();row.forEach(x=>{const cell=tr.insertCell();cell.textContent=x})});root.replaceChildren(t)}table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))";

/// Complete report shell. Substituted in one pass by [`render_template`], so a link
/// label that happens to read like a token cannot corrupt the payloads.
const REPORT_TEMPLATE: &str = include_str!("report.html");

/// Renders the agent travel tables. They quote person identifiers, so the header and every row
/// are split with a quote-aware parser instead of `String.split(',')`.
pub(super) const CSV_TABLE_SCRIPT: &str = "function parseCsv(line){const fields=[];let field='',quoted=false;for(let i=0;i<line.length;i++){const ch=line[i];if(ch.charCodeAt(0)===34){if(quoted&&line.charCodeAt(i+1)===34){field+=String.fromCharCode(34);i++}else{quoted=!quoted}}else if(ch===','&&!quoted){fields.push(field);field=''}else{field+=ch}}fields.push(field);return fields}function csvTable(id,rows){table(document.querySelector(id),parseCsv(rows[0]),rows.slice(1).map(parseCsv))}";

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

const TRANSIT_NOTE: &str = "Public transport service modeling is recorded per exported trip. Waiting is boarding time minus departure at the stop, in-vehicle time is arrival minus boarding time, and arrival delay compares arrival with the schedule. A missed service has no waiting time. Boardings, alightings and loads are expanded by the reciprocal of the sample size; load factors use the declared vehicle capacity. Missing service, schedule, capacity or vehicle-level inputs remain unavailable instead of being inferred.";

/// Explains the censoring convention, next to the tables that report it.
const ACTIVITY_PATTERN_NOTE: &str = "Activity times come from the recorded activity start and end events, and travel time from \
     the observed leg completions. The recording window opens at the simulation start, so the \
     first activity of a day was already in progress when it did and its total duration is only \
     a lower bound; it is flagged as left-censored. An activity with no end before the run shuts \
     down is right-censored for the same reason. Censored activities still report the seconds \
     they were observed inside the window, so in-window activity time plus travel time can be \
     reconciled against the observed span of the day; a nonzero timeline gap is time the \
     recorded events do not account for.";

/// Explains what the zone and urban-area tables resolve, next to the tables themselves.
const ZONE_NOTE: &str = "Zones come from the supplied zone system, never from the network: a journey is located by \
     the links its origin and destination name, and a person by the supplied person geography. A \
     location the zone system does not cover is reported as unmapped and still forms OD rows and \
     boundary crossings, so the matrices account for every observed journey. Only journeys with \
     an observed departure enter a matrix, because the matrix is keyed by departure interval; a \
     journey that never departed keeps its zone counts instead.";

/// Explains how the link speeds are reconstructed, next to the tables they are rendered in.
const SPEED_NOTE: &str = "Speeds are reconstructed from full-link traversals and assigned to the interval in which the vehicle entered the link. The representative speed divides the total travelled distance by the total travel time; the arithmetic vehicle-speed mean and population standard deviation describe the single traversals. A link without a full-link traversal has no speed: QSim inserts a vehicle at the end of the first link of a leg, so the first link of a network leg never covers its whole length and is reported as a partial traversal instead. The traversal records table lists every record that cannot produce a full-link speed, such as those partial traversals, traversals that never finished, and records without a positive duration.";

/// Explains where the service tables come from, next to the tables themselves.
const SERVICE_NOTE: &str = "Computed only from the supplied request, passenger, fleet and schedule records; no service is simulated. A request counts as rejected only when its request record says so, never because a completed leg is missing, and a request with neither a rejection nor a passenger record is unserved. Wait is pickup minus submission; the detour ratio is in-vehicle time over the supplied direct travel time. A drive task is occupied when a served request is on board for its whole span. Metrics whose inputs were not supplied stay blank and are listed as unavailable.";

/// Human-readable description of every exported CSV dataset, surfaced in the per-section metric
/// view so the "Detailed dataset" dropdown tells the reader what each file actually contains.
const DATASET_DESCRIPTIONS: &[(&str, &str)] = &[
    // Travel family.
    (
        "journey_summary.csv",
        "Pre-aggregated journey counts by main mode, purpose and (when present) departure interval. Each row summarises many person-journeys, so the range covers typical observed outcomes.",
    ),
    (
        "journey_mode_share.csv",
        "Mode share of journeys and distance by main mode, purpose and distance class. Each row is a slice of the population.",
    ),
    (
        "journey_survey_comparison.csv",
        "Observed vs. simulated journey counts by main mode and purpose, joined to the supplied travel survey when available.",
    ),
    (
        "journeys.csv",
        "One row per observed journey with departure, arrival, travel time and distance. Use this when you need the full distribution of single journeys, not a pre-aggregated summary.",
    ),
    (
        "legs.csv",
        "One row per observed leg with mode, departure, travel time and distance. Includes access, egress and transfer legs.",
    ),
    (
        "leg_hourly.csv",
        "Leg counts and travel-time aggregates per departure hour and mode. The hourly narrative chart on the Travel page reads this dataset.",
    ),
    (
        "person_daily.csv",
        "One row per simulated person-day, with journey counts, total travel time and total distance.",
    ),
    (
        "daily_summary.csv",
        "Day-level aggregates by cohort (all persons, complete persons, planned-day persons). The cohort definitions differ; the report labels them explicitly.",
    ),
    (
        "en_route_agents.csv",
        "Counts of agents departing, arriving and stuck during each interval. The 'When people travel' overview chart reads this dataset.",
    ),
    // Network family.
    (
        "coverage.csv",
        "Used and unused eligible-link counts per interval, plus share used. Powers the coverage chart and the green/gray link map.",
    ),
    (
        "group_coverage.csv",
        "Coverage broken down by the supplied road-type, road-size, urban-area or label group.",
    ),
    (
        "link_capacity.csv",
        "Per-link PCE volumes and capacity utilisation per interval. Each row is one link in one interval.",
    ),
    (
        "link_hourly.csv",
        "Per-link entry and exit vehicle counts per interval.",
    ),
    (
        "link_hourly_by_class.csv",
        "Per-link entry/exit counts broken down by supplied road class.",
    ),
    (
        "link_speed_summary.csv",
        "Run-wide link-speed mean and standard deviation, aggregated from single traversals.",
    ),
    (
        "link_speed_hourly.csv",
        "Mean link speed per interval across the whole network.",
    ),
    (
        "link_speed_by_class.csv",
        "Mean link speed per interval by supplied road class.",
    ),
    (
        "link_speed_histogram.csv",
        "Histogram of single-traversal vehicle speeds.",
    ),
    (
        "link_speed_diagnostics.csv",
        "Records that cannot contribute to link-speed totals (partial traversals, missing duration, etc.).",
    ),
    (
        "network_distance_time.csv",
        "Per-link observed distance and travel time across the run.",
    ),
    (
        "network_distance_time_summary.csv",
        "Run-wide vehicle distance, time and relative-speed ratio. The vehicle-distance and vehicle-time series on the Network page read this dataset.",
    ),
    (
        "network_distance_time_diagnostics.csv",
        "Records that cannot contribute to network distance or time totals.",
    ),
    (
        "urban_area_summary.csv",
        "Coverage, distance and time totals grouped by the supplied urban-area classification.",
    ),
    (
        "zone_od.csv",
        "Origin-destination matrix between supplied zones.",
    ),
    (
        "zone_flows.csv",
        "Boundary-crossing flows between supplied zones.",
    ),
    (
        "zone_summary.csv",
        "Per-zone totals of journeys, mode share, distance and travel time.",
    ),
    // Capacity family.
    (
        "vc_histogram.csv",
        "Histogram of link-intervals by V/C ratio bin (entry and exit). The capacity chart on the Capacity page reads this dataset.",
    ),
    // Activities family.
    (
        "activity_patterns.csv",
        "Per-person counts of substantive activities and pattern completeness.",
    ),
    (
        "activity_durations.csv",
        "Per-activity observed duration, censoring flag and timing.",
    ),
    (
        "activity_type_summary.csv",
        "Aggregated duration by activity type. The activity-pattern chart reads from the related activity_pattern_summary.",
    ),
    (
        "activity_pattern_summary.csv",
        "Counts of persons by pattern status (truncated, complete, etc.). The 'Daily activity patterns' bars on the Activities page read this dataset.",
    ),
    // Transit family.
    (
        "transit_availability.csv",
        "Which transit metrics could be computed for each line, stop and hour. Rows that remain unavailable are listed explicitly.",
    ),
    (
        "transit_outcomes.csv",
        "Trip outcomes (boarded, denied, missed) by hour. The transit line chart on the Public transport page reads this dataset.",
    ),
    (
        "transit_stop_hourly.csv",
        "Boardings and alightings by line, stop and hour.",
    ),
    (
        "transit_line_summary.csv",
        "Per-line waiting, in-vehicle time and arrival delay. The 'mean wait seconds' metric is the default pick here.",
    ),
    (
        "transit_occupancy.csv",
        "Per-departure and segment occupancy and load factor.",
    ),
    (
        "transit_journeys.csv",
        "Per-journey access, egress, transfer counts and timings.",
    ),
    (
        "transit_trips.csv",
        "Passenger transit trips with route, stop sequence and timing.",
    ),
    (
        "transit_validation_summary.csv",
        "Observed transit demand summary matched to the supplied counts.",
    ),
    (
        "transit_validation_matches.csv",
        "Matched observed demand records per stop and hour.",
    ),
    (
        "transit_validation_unmatched.csv",
        "Observed demand records that could not be matched to a transit service record.",
    ),
    // Additional family.
    (
        "economic_appraisal.csv",
        "Cost and benefit breakdown by category, account and mode.",
    ),
    (
        "economic_summary.csv",
        "Run-wide economic totals by category, account and unit.",
    ),
    (
        "noise_summary.csv",
        "Sound levels in dB by source, period and receiver group.",
    ),
    (
        "noise_availability.csv",
        "Which noise metrics could be computed.",
    ),
    (
        "noise_maps.csv",
        "Receiver locations used for noise exposure maps.",
    ),
    (
        "emissions_hourly.csv",
        "Hourly emitted mass by pollutant, mode and road class.",
    ),
    ("group_burdens.csv", "Per-group exposure and burden totals."),
    (
        "group_module_outcomes.csv",
        "Per-group outcome totals across modules.",
    ),
    (
        "equity_comparison.csv",
        "Per-group comparison of supplied criterion.",
    ),
    (
        "person_demographics.csv",
        "Per-person demographic attributes joined to the analysis.",
    ),
    (
        "validation_summary.csv",
        "Observed vs. simulated counts and speeds summary.",
    ),
    (
        "validation_matches.csv",
        "Matched observed vs. simulated records.",
    ),
    (
        "validation_unmatched.csv",
        "Observed records that could not be matched.",
    ),
    (
        "cross_run_comparison.csv",
        "Mode shares and other metrics across compared runs.",
    ),
    (
        "service_summary.csv",
        "DRT/taxi service totals by line or hour.",
    ),
    (
        "service_requests.csv",
        "Per-request service timing and outcome.",
    ),
    (
        "service_vehicles.csv",
        "Per-vehicle service timing and occupancy.",
    ),
    (
        "service_occupancy.csv",
        "Per-task occupancy and occupied-task counts.",
    ),
    (
        "service_constraints.csv",
        "Per-request supplied constraints.",
    ),
    (
        "service_availability.csv",
        "Which service metrics could be computed.",
    ),
    (
        "service_diagnostics.csv",
        "Service records that cannot contribute to totals.",
    ),
    (
        "accessibility_summary.csv",
        "Accessibility totals by category, mode, departure period and threshold.",
    ),
    (
        "accessibility_zones.csv",
        "Per-zone accessibility to supplied opportunities.",
    ),
    (
        "accessibility_persons.csv",
        "Per-person accessibility to supplied opportunities.",
    ),
    (
        "accessibility_diagnostics.csv",
        "Records that cannot contribute to accessibility totals.",
    ),
    (
        "link_classification.csv",
        "Per-link road class and label group classification.",
    ),
    (
        "runtime.csv",
        "Simulation and analysis runtime, worker count and peak memory. The Execution context KPIs read this dataset.",
    ),
];

/// Human-readable description of every metric the catalog exports, surfaced when a column is picked.
const METRIC_DESCRIPTIONS: &[(&str, &str)] = &[
    // Counts and shares.
    (
        "duration_seconds",
        "Travel time from departure to arrival for completed journeys, legs or trips.",
    ),
    (
        "distance_meters",
        "Travel distance covered by a journey, leg or trip, in metres.",
    ),
    (
        "mean_duration_seconds",
        "Average duration across the records in this row, in seconds.",
    ),
    (
        "mean_distance_meters",
        "Average distance across the records in this row, in metres.",
    ),
    (
        "median_duration_seconds",
        "Median duration across the records in this row, in seconds.",
    ),
    (
        "median_distance_meters",
        "Median distance across the records in this row, in metres.",
    ),
    (
        "journeys",
        "Number of planned journeys represented by this row.",
    ),
    (
        "completed",
        "Number of journeys that reached their destination in this row.",
    ),
    (
        "completed_journeys",
        "Number of journeys that reached their destination in this row.",
    ),
    (
        "planned_journeys",
        "Number of planned journeys in this row.",
    ),
    ("persons", "Number of persons represented by this row."),
    ("legs", "Number of legs represented by this row."),
    (
        "departures",
        "Number of agent departures recorded during this interval.",
    ),
    (
        "arrivals",
        "Number of agent arrivals recorded during this interval.",
    ),
    (
        "stuck",
        "Number of agents that got stuck during this interval.",
    ),
    (
        "peak_agents",
        "Maximum number of agents in transit at any moment during this interval.",
    ),
    (
        "departure_hour_seconds",
        "Start of the departure hour, in seconds from midnight.",
    ),
    (
        "hour_start_seconds",
        "Start of the interval, in seconds from midnight.",
    ),
    // Coverage and link volumes.
    (
        "entry_vehicles",
        "Number of vehicles that entered the link during the interval.",
    ),
    (
        "exit_vehicles",
        "Number of vehicles that exited the link during the interval.",
    ),
    (
        "eligible_links",
        "Number of eligible links in the interval.",
    ),
    (
        "used_links",
        "Number of eligible links with at least one traversal in the interval.",
    ),
    (
        "unused_links",
        "Number of eligible links with no traversal in the interval.",
    ),
    (
        "used_percent",
        "Share of eligible links used in the interval, in percent.",
    ),
    (
        "capacity_pce_per_hour",
        "Hourly capacity of the link in passenger-car equivalents.",
    ),
    (
        "effective_capacity_pce",
        "Effective PCE capacity of the link in the interval.",
    ),
    (
        "entry_pce",
        "PCE count entering the link during the interval.",
    ),
    (
        "exit_pce",
        "PCE count exiting the link during the interval.",
    ),
    (
        "entry_pce_scaled",
        "PCE count entering the link, scaled up by the reciprocal of the sample size.",
    ),
    (
        "exit_pce_scaled",
        "PCE count exiting the link, scaled up by the reciprocal of the sample size.",
    ),
    (
        "entry_vc",
        "Entry volume-to-capacity ratio (entry PCE / effective capacity).",
    ),
    (
        "exit_vc",
        "Exit volume-to-capacity ratio (exit PCE / effective capacity).",
    ),
    ("link_count", "Number of links represented by this row."),
    // Speeds and times.
    (
        "mean_link_speed_mps",
        "Average vehicle speed on the link, in metres per second.",
    ),
    (
        "vehicle_distance_meters",
        "Total vehicle distance covered, in metres.",
    ),
    (
        "vehicle_time_seconds",
        "Total vehicle time spent on the network, in seconds.",
    ),
    (
        "free_flow_relative_delay_seconds",
        "Signed delay vs. free-flow time, in seconds. Negative values mean faster than free flow; positive values mean slower.",
    ),
    (
        "relative_speed_ratio",
        "Ratio of simulated to free-flow speed (1.0 means on par with free flow; below 1.0 means slower).",
    ),
    // Transit.
    (
        "mean_wait_seconds",
        "Average waiting time (boarding minus departure at the stop) in seconds.",
    ),
    (
        "mean_ivt_seconds",
        "Average in-vehicle time (arrival minus boarding) in seconds.",
    ),
    (
        "mean_delay_seconds",
        "Average arrival delay vs. the schedule, in seconds.",
    ),
    ("boardings", "Number of boardings recorded."),
    ("alightings", "Number of alightings recorded."),
    ("load", "Vehicle load at the recorded point, in passengers."),
    (
        "load_factor",
        "Load divided by declared vehicle capacity, as a fraction.",
    ),
    // Activities.
    (
        "mean_duration",
        "Average duration of the activity, in seconds.",
    ),
    (
        "total_duration",
        "Total duration across the records in this row, in seconds.",
    ),
    (
        "censored_left",
        "Number of activities that were left-censored by the recording window.",
    ),
    (
        "censored_right",
        "Number of activities that were right-censored by the recording window.",
    ),
    // General numerics.
    ("value", "Numeric value of the record in its declared unit."),
    ("count", "Number of records contributing to this row."),
];

/// Convert a slice of `(name, description)` tuples into a JSON map so the report can look each
/// description up by name in constant time. The list order is preserved for any consumer that
/// still wants to iterate entries; the JSON object simply makes lookup cheaper.
fn description_map(pairs: &[(&str, &str)]) -> serde_json::Value {
    let mut object = serde_json::Map::with_capacity(pairs.len());
    for &(name, description) in pairs {
        object.insert(
            name.to_owned(),
            serde_json::Value::String(description.to_owned()),
        );
    }
    serde_json::Value::Object(object)
}

/// Fill `template` by scanning it once, left to right.
///
/// A chained `str::replace` would rescan text it had already substituted, so a
/// replacement value that happens to contain another token (a link label reading
/// `__METRICS__`, say) would be rewritten by a later step and corrupt the
/// payload. Advancing past the token after one substitution leaves inserted text
/// untouched. Byte indexing is safe here because the cursor only ever lands on a
/// `char` boundary.
fn render_template(template: &str, replacements: &[(&str, &str)]) -> String {
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

pub(super) fn csv_for_script(path: &Path) -> Result<String, AnalysisError> {
    let csv = fs::read_to_string(path).map_err(io_error)?;
    json_for_script(&csv.lines().collect::<Vec<_>>())
}

fn json_for_script(value: &(impl Serialize + ?Sized)) -> Result<String, AnalysisError> {
    serde_json::to_string(value)
        .map(|json| {
            json.replace('&', "\\u0026")
                .replace('<', "\\u003c")
                .replace('>', "\\u003e")
        })
        .map_err(|error| AnalysisError::new(error.to_string()))
}

/// Embeds at most `rows` lines, header included, without reading the whole file.
/// Reports whether the file had more lines than were embedded.
pub(super) fn csv_preview_for_script(
    path: &Path,
    rows: usize,
) -> Result<(String, bool), AnalysisError> {
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

pub(super) fn write_failure_report(
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

pub(super) fn write_cross_run_report(path: &Path) -> Result<(), AnalysisError> {
    let rows = csv_for_script(&path.join("journey_mode_share.csv"))?;
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Cross-run journey comparison</title><style>{REPORT_STYLE}</style></head><body><h1>Cross-run journey mode shares</h1><p>Each row comes from the latest completed iteration report of its run. Shares are grouped by departure interval, purpose, distance class, and main mode.</p><div id=\"comparison\"></div><p><a href=\"journey_mode_share.csv\">CSV table</a> · <a href=\"manifest.json\">run iterations</a> · <a href=\"metric_catalog.json\">metric catalog</a></p><script>{CSV_TABLE_SCRIPT}csvTable('#comparison',{rows});</script></body></html>"
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
}

pub(super) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{REPORT_TEMPLATE, render_template, visual_table, write_report};
    use crate::simulation::analysis::{LinkHourlyMetric, Manifest, ModuleStatus};
    use crate::simulation::config::ZoneSystem;
    use std::fs;

    /// Report placeholders, in order. A token opens with `__` followed by a letter and closes
    /// at the next `__`; two tokens side by side share no underscore, because each token's
    /// name ends with a letter.
    fn template_tokens() -> Vec<&'static str> {
        let mut tokens = Vec::new();
        let mut rest = REPORT_TEMPLATE;
        while let Some(open) = rest.find("__") {
            let name = &rest[open + 2..];
            if !name.starts_with(|character: char| character.is_ascii_uppercase()) {
                rest = &rest[open + 2..];
                continue;
            }
            let Some(close) = name.find("__") else {
                rest = &rest[open + 2..];
                continue;
            };
            tokens.push(&rest[open..open + 2 + close + 2]);
            rest = &rest[open + 2 + close + 2..];
        }
        tokens
    }

    #[test]
    fn report_data_is_inserted_without_rescanning_tokens() {
        let html = render_template(
            "<main>__TITLE__ __BODY__</main>",
            &[
                ("__TITLE__", "__BODY__"),
                ("__BODY__", "<p>prepared report data</p>"),
            ],
        );

        assert_eq!(html, "<main>__BODY__ <p>prepared report data</p></main>");
    }

    #[test]
    fn writes_html_from_prepared_report_data() {
        let directory = tempfile::tempdir().unwrap();
        for file in [
            "coverage.csv",
            "link_capacity.csv",
            "vc_histogram.csv",
            "network_map.svg",
            "link_speed_hourly.csv",
            "link_speed_summary.csv",
            "link_speed_histogram.csv",
            "link_speed_diagnostics.csv",
            "leg_hourly.csv",
            "daily_summary.csv",
            "person_daily.csv",
            "network_distance_time.csv",
            "network_distance_time_summary.csv",
            "en_route_agents.csv",
            "network_distance_time_diagnostics.csv",
            "journeys.csv",
            "journey_mode_share.csv",
            "journey_summary.csv",
            "journey_survey_comparison.csv",
            "legs.csv",
            "validation_summary.csv",
            "validation_matches.csv",
            "runtime.csv",
            "noise_summary.csv",
            "noise_availability.csv",
            "noise_maps.csv",
            "emissions_hourly.csv",
            "cross_run_comparison.csv",
            "group_burdens.csv",
            "group_module_outcomes.csv",
            "equity_comparison.csv",
            "person_demographics.csv",
            "service_requests.csv",
            "service_summary.csv",
            "service_vehicles.csv",
            "service_occupancy.csv",
            "service_constraints.csv",
            "service_availability.csv",
            "service_diagnostics.csv",
            "transit_availability.csv",
            "transit_outcomes.csv",
            "transit_stop_hourly.csv",
            "transit_line_summary.csv",
            "transit_occupancy.csv",
            "transit_journeys.csv",
            "transit_trips.csv",
            "transit_validation_summary.csv",
            "transit_validation_matches.csv",
            "transit_validation_unmatched.csv",
            "activity_patterns.csv",
            "activity_durations.csv",
            "activity_type_summary.csv",
            "activity_pattern_summary.csv",
            "zone_od.csv",
            "zone_flows.csv",
            "zone_summary.csv",
            "urban_area_summary.csv",
            "economic_summary.csv",
        ] {
            fs::write(directory.path().join(file), "fixture\n").unwrap();
        }
        fs::write(directory.path().join("network_map.svg"), "<svg></svg>").unwrap();
        let manifest: Manifest = serde_json::from_str(
            r#"{"status":"complete","failure":null,"iteration":3,"interval_seconds":3600,"simulation_end_time":3600,"partitions":[0],"input_format":"xml","eligible_links":1,"random_seed":1,"sample_size":1.0,"network_input":null,"population_input":null,"software_version":"test","excess_delay_clip_seconds":null}"#,
        )
        .unwrap();
        let statuses = [ModuleStatus {
            module: "link_coverage",
            required: true,
            status: "complete",
            reason: None,
        }];
        let link_hourly = [LinkHourlyMetric {
            link_id: "link-1".to_owned(),
            hour_start_seconds: 0,
            entry_vehicles: 2,
            exit_vehicles: 1,
            urban_area: "unknown".to_owned(),
            road_type: "unknown".to_owned(),
            road_size: "unknown".to_owned(),
        }];

        write_report(
            directory.path(),
            &manifest,
            &statuses,
            &link_hourly,
            &ZoneSystem::default(),
        )
        .unwrap();

        let html = fs::read_to_string(directory.path().join("index.html")).unwrap();
        assert!(html.contains("Completed final iteration 3"));
        assert!(!html.contains("link-1"));
        assert!(html.contains("report-data"));
        assert!(html.len() < 100_000);

        // A failed presentation refresh must leave the last published page intact.
        fs::write(directory.path().join("coverage.csv"), "a,b\n1,2,3\n").unwrap();
        assert!(
            super::refresh_visual_report(directory.path(), &manifest, &serde_json::json!([]))
                .is_err()
        );
        assert_eq!(
            fs::read_to_string(directory.path().join("index.html")).unwrap(),
            html
        );
    }

    #[test]
    fn visual_metrics_reduce_every_record_and_keep_units_separate() {
        let directory = tempfile::tempdir().unwrap();
        let mut csv = String::from("person_id,unit,value,duration_seconds\n");
        for i in 0..10_000 {
            csv.push_str(&format!("\"person,{i}\",USD,2,10\n"));
        }
        csv.push_str("person,EUR,100,20\nperson,USD,NaN,\nperson,USD,inf,-5\n");
        fs::write(directory.path().join("legs.csv"), csv).unwrap();
        let table = visual_table(directory.path(), "legs.csv").unwrap();
        assert_eq!(table.records, 10_003);
        assert!(
            table.rows.is_empty(),
            "raw person records must stay outside HTML"
        );
        let dollars = table
            .columns
            .iter()
            .find(|c| c.name == "value" && c.group == "unit=USD")
            .unwrap();
        assert_eq!(dollars.count, 10_000);
        assert_eq!(dollars.mean, Some(2.0));
        assert_eq!(dollars.invalid, 2);
        let euros = table
            .columns
            .iter()
            .find(|c| c.name == "value" && c.group == "unit=EUR")
            .unwrap();
        assert_eq!(euros.mean, Some(100.0));
        assert!(serde_json::to_string(&table).unwrap().len() < 2000);
    }

    #[test]
    fn summary_rows_are_bounded_but_metrics_include_the_tail() {
        let directory = tempfile::tempdir().unwrap();
        let mut csv = String::from("hour_start_seconds,used_percent\n");
        for i in 0..1000 {
            csv.push_str(&format!("{i},{}\n", if i == 999 { 100 } else { 0 }));
        }
        fs::write(directory.path().join("coverage.csv"), csv).unwrap();
        let table = visual_table(directory.path(), "coverage.csv").unwrap();
        assert_eq!(table.rows.len(), super::SUMMARY_ROWS);
        assert_eq!(table.records, 1000);
        let percent = table
            .columns
            .iter()
            .find(|c| c.name == "used_percent")
            .unwrap();
        assert_eq!(percent.max, Some(100.0));
        assert_eq!(percent.mean, Some(0.1));
    }

    #[test]
    fn histogram_uses_intervals_beyond_the_summary_limit() {
        let directory = tempfile::tempdir().unwrap();
        let csv = format!(
            "metric,bin_lower,bin_upper,links\n{}",
            "entry_vc,0,0.1,2\n".repeat(1000)
        );
        fs::write(directory.path().join("vc_histogram.csv"), csv).unwrap();
        let table = visual_table(directory.path(), "vc_histogram.csv").unwrap();
        assert_eq!(table.rows.len(), super::SUMMARY_ROWS);
        assert_eq!(table.histogram.len(), 1);
        assert_eq!(table.histogram[0].links, 2000);
    }

    #[test]
    fn finite_extremes_do_not_overflow_the_row_mean() {
        let mut column = super::VisualColumn {
            name: "value".into(),
            group: String::new(),
            count: 0,
            missing: 0,
            invalid: 0,
            min: None,
            mean: None,
            max: None,
        };
        column.observe(&f64::MAX.to_string());
        column.observe(&(-f64::MAX).to_string());
        assert_eq!(column.mean, Some(0.0));
    }

    #[test]
    fn no_section_placeholder_is_repeated_in_the_template() {
        // A token listed twice renders its section twice. Tokens are uppercase words joined
        // by single underscores, so two tokens side by side are `__ONE____TWO__`.
        let mut tokens = template_tokens();
        let total = tokens.len();
        tokens.sort_unstable();
        tokens.dedup();
        assert_eq!(total, tokens.len(), "{tokens:?}");
    }
}
