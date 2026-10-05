use super::*;

pub(super) const REPORT_STYLE: &str = "body{font:16px system-ui;max-width:1100px;margin:3rem auto;padding:0 1rem;color:#17212b}table{border-collapse:collapse;margin-bottom:2rem}td,th{border:1px solid #ccd;padding:.5rem}a{color:#075ea8}pre{background:#f4f6f9;border:1px solid #ccd;padding:1rem;overflow:auto}";

const MODULE_TABLE_SCRIPT: &str = "function table(root,headers,rows){const t=document.createElement('table'),head=t.createTHead().insertRow();headers.forEach(x=>{const cell=document.createElement('th');cell.textContent=x;head.appendChild(cell)});const body=t.createTBody();rows.forEach(row=>{const tr=body.insertRow();row.forEach(x=>{const cell=tr.insertCell();cell.textContent=x})});root.replaceChildren(t)}table(document.querySelector('#modules'),['Module','Status','Reason'],m.map(x=>[x.module,x.status,x.reason||'']))";

/// Complete report shell. Substituted in one pass by [`render_template`], so a link
/// label that happens to read like a token cannot corrupt the payloads.

const REPORT_TEMPLATE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>MATSim analysis</title><style>__REPORT_STYLE__label{margin-right:1rem}</style></head><body><h1>Simulation analysis</h1><p>Completed final iteration __ITERATION__; __LINKS__ eligible directed links in __INTERVAL__-second intervals.</p><h2>Final-run network coverage map</h2><p>Green links were used at least once in the final iteration; gray links were unused. Dashed links are expressways. Hover over a link for its classifications.</p><div id="map-container">__NETWORK_MAP__</div><h2>Observed validation</h2><p>Count observations are expanded by the reciprocal of the simulated sample fraction. Only exact link, period, class and metric matches are compared; __VALIDATION_NOTE__ A blank relative error means the observed reference is zero.</p><h3>Validation summary</h3><div id="validation-summary"></div><h3>Matched observations</h3><div id="validation-matches"></div>__VALIDATION_PLOTS__<p><a href="validation_summary.csv">Summary CSV</a> · <a href="validation_matches.csv">Matched observations CSV</a> · <a href="validation_unmatched.csv">Unmatched observations CSV</a></p><h2>Coverage by group</h2><p>Urban area, road type, and road size are grouped independently. Missing labels are retained as unknown; geographic boundary crossings are explicit.</p><div id="groups"></div><h2>Hourly link metrics</h2><p>Filter on any combination of classifications to compare link volumes by group.</p><div id="filters"></div><div id="hourly"></div><h2>Hourly network coverage</h2><div id="coverage"></div><h2>PCE volumes and capacity utilization</h2><p>Volumes are passenger-car-equivalent weighted, matching how the link flow cap is charged, and are scaled up by the simulated sample fraction to describe the full population. Raw vehicle counts, observed PCE volumes and scaled PCE volumes are exported separately. The V/C denominator is the link's own network capacity multiplied by the length of the interval the simulation covered; lanes are never applied again, and a value on a bin edge belongs to the higher bin. A link that carried no vehicles is counted as unused whatever its capacity says, while missing PCE or an invalid capacity leaves the ratio blank and is reported per link.</p><h3>Per-link PCE volumes, capacity and V/C</h3><div id="capacity"></div><h3>V/C distribution</h3><p id="histogram-metric-label">Entry V/C (default view)</p><div id="histogram"></div><button id="histogram-toggle" type="button">Show exit V/C</button><h2>Interval link speeds</h2><p>__SPEED_NOTE__</p>__SPEED_SECTIONS__<h2>Network distance, time and congestion</h2><p>Vehicle distance uses the observed fraction of each link. Partial and unfinished traversals are reported in diagnostics. A traversal crossing an interval boundary is assigned whole to its entry interval, so no within-link path is inferred. Relative delay is signed; clipped excess delay, when configured, sums positive link delay capped per link and interval. Passenger distance and time are unavailable because link-level passenger occupancy is not recorded. <a href="network_distance_time_diagnostics.csv">Traversal diagnostics (CSV)</a>.</p><h3>Network totals and peak-hour profile</h3><p id="peak-delay"></p><div id="network-summary"></div><h3>Per-link distance, time and relative speed</h3><div id="network-link-metrics"></div><h3>Traversal exclusions</h3><div id="network-distance-diagnostics"></div><h2>Available metrics</h2><div id="metrics"></div><h2>Agent travel</h2><p>Leg completion uses observed departure and arrival events. Incomplete persons retain completed-leg duration totals; missing arrivals are excluded from duration means. Verified non-travelers have an expected plan with no legs. Journeys run between substantive activities; stage activities such as transit transfers stay within the journey. Main mode follows the MATSim analysis hierarchy. Distances sum planned route distances, including prepared teleported routes, and report when any component is unavailable.</p><h3>En-route agent profile</h3><p>Counts use observed person departures, arrivals, and stuck events across all travel modes. Person-seconds are allocated by event timestamps; no within-link position or occupancy is inferred.</p><div id="en-route-agents"></div><h3>Departures and duration by interval and mode</h3><div id="leg-hourly"></div><h3>Journey mode share by hour, purpose, and distance</h3><div id="journey-shares"></div><h3>Journey duration and distance distributions</h3><div id="journey-summary"></div><h3>Journey components and completion</h3><div id="journeys"></div><h3>Daily cohort means</h3><div id="daily"></div><h3>Person daily totals and status</h3><div id="persons"></div><h3>Observed and planned legs</h3><p>__LEGS_NOTE__</p><div id="legs"></div>__SURVEY_SECTION____TRANSIT_SECTION____CROSS_RUN_SECTION____SERVICE_SECTION__<h2>Demographic outcomes and equity</h2><p>__DEMOGRAPHIC_NOTE__</p><h3>Group sizes and travel burdens</h3><div id="group-burdens"></div><h3>Person groups</h3><div id="person-demographics"></div><h3>Other modules' group outcomes</h3><div id="group-module-outcomes"></div><h3>Equity comparison</h3><div id="equity-comparison"></div><h2>Module status</h2><div id="modules"></div><p>Machine-readable data: <a href="network_map.svg">coverage map (SVG)</a>, <a href="link_classification.csv">link classifications (CSV)</a>, <a href="group_coverage.csv">group coverage (CSV)</a>, <a href="link_hourly.csv">link volumes (CSV)</a>, <a href="link_capacity.csv">PCE volumes, capacity and V/C (CSV)</a>, <a href="vc_histogram.csv">V/C distribution (CSV)</a>, <a href="coverage.csv">coverage (CSV)</a>, <a href="link_speed_hourly.csv">link speeds (CSV)</a>, <a href="link_speed_summary.csv">interval speed summary (CSV)</a>, <a href="link_speed_histogram.csv">speed histogram (CSV)</a>, <a href="link_speed_diagnostics.csv">speed traversal records (CSV)</a>, <a href="leg_hourly.csv">legs by interval and mode (CSV)</a>, <a href="journeys.csv">journey components and completion (CSV)</a>, <a href="journey_mode_share.csv">journey mode shares (CSV)</a>, <a href="journey_summary.csv">journey distributions (CSV)</a>, <a href="person_daily.csv">person daily totals (CSV)</a>, <a href="daily_summary.csv">daily cohort means (CSV)</a>, <a href="legs.csv">legs (CSV)</a>, <a href="journey_survey_comparison.csv">journey survey (CSV)</a>, <a href="service_summary.csv">service summary (CSV)</a>, <a href="service_requests.csv">service requests (CSV)</a>, <a href="service_vehicles.csv">service vehicles (CSV)</a>, <a href="service_occupancy.csv">service occupancy (CSV)</a>, <a href="service_constraints.csv">service constraints (CSV)</a>, <a href="service_availability.csv">service availability (CSV)</a>, <a href="service_diagnostics.csv">service diagnostics (CSV)</a>, <a href="transit_trips.csv">transit trips (CSV)</a>, <a href="transit_stop_hourly.csv">transit boardings and alightings (CSV)</a>, <a href="transit_line_summary.csv">transit line summary (CSV)</a>, <a href="transit_occupancy.csv">transit occupancy (CSV)</a>, <a href="transit_journeys.csv">transit journeys (CSV)</a>, <a href="transit_outcomes.csv">transit outcomes (CSV)</a>, <a href="transit_availability.csv">transit availability (CSV)</a>, <a href="transit_validation_summary.csv">transit observed demand (CSV)</a>, <a href="run_metadata.json">expected travel and vehicle/PCE metadata (JSON)</a>, <a href="manifest.json">run manifest</a>, <a href="metric_catalog.json">metric catalog</a>.</p><script>const d=__LINK_HOURLY__;const c=__COVERAGE__;const a=__METRICS__;const cap=__LINK_CAPACITY__;const bins=__VC_HISTOGRAM__;const m=__MODULES__;const D=__DIMENSIONS__;const lh=__LEG_HOURLY__;const dy=__DAILY__;const pd=__PERSONS__;const lg=__LEGS__;const gb=__GROUP_BURDENS__;const pg=__PERSON_DEMOGRAPHICS__;const gm=__GROUP_MODULE_OUTCOMES__;const eq=__EQUITY_COMPARISON__;const js=__JOURNEY_SHARES__;const jy=__JOURNEY_SUMMARY__;const jn=__JOURNEYS__;const jsurvey=__JOURNEY_SURVEY__;__SPEED_DECLARATIONS____MODULE_TABLE_SCRIPT__;__CSV_TABLE_SCRIPT__;csvTable('#validation-summary',__VALIDATION_SUMMARY__);csvTable('#validation-matches',__VALIDATION_MATCHES__);__TRANSIT_RENDER____CROSS_RUN_RENDER____SERVICE_RENDER__table(document.querySelector('#coverage'),['hour_start_seconds','eligible_links','used_links','unused_links','used_percent'],c.slice(1).map(x=>x.split(',')));__SPEED_RENDERS__table(document.querySelector('#capacity'),cap[0].split(','),cap.slice(1).map(x=>x.split(',')));const metricColumn=bins[0].indexOf('metric');let metric='entry_vc';function histogram(){const root=document.querySelector('#histogram');root.replaceChildren();table(root,bins[0],bins.slice(1).filter(x=>x[metricColumn]===metric));document.querySelector('#histogram-metric-label').textContent=metric==='entry_vc'?'Entry V/C (default view)':'Exit V/C';document.querySelector('#histogram-toggle').textContent=metric==='entry_vc'?'Show exit V/C':'Show entry V/C';}histogram();document.querySelector('#histogram-toggle').addEventListener('click',()=>{metric=metric==='entry_vc'?'exit_vc':'entry_vc';histogram()});table(document.querySelector('#metrics'),['Metric','Unit','Aggregation key'],a.map(x=>[x.name,x.unit,x.aggregation_key]));csvTable('#leg-hourly',lh);csvTable('#journey-shares',js);csvTable('#journey-survey',jsurvey);csvTable('#journey-summary',jy);csvTable('#journeys',jn);csvTable('#daily',dy);csvTable('#persons',pd);csvTable('#legs',lg);csvTable('#group-burdens',gb);csvTable('#person-demographics',pg);csvTable('#group-module-outcomes',gm);csvTable('#equity-comparison',eq);const selectors=[];D.forEach(([key,title])=>{const label=document.createElement('label');label.textContent=title+' ';const select=document.createElement('select');select.append(new Option('All',''));[...new Set(d.map(x=>x[key]))].sort().forEach(value=>select.append(new Option(value,value)));label.append(select);document.querySelector('#filters').append(label);select.addEventListener('change',renderHourly);selectors.push([key,select])});function selectedRows(){return d.filter(row=>selectors.every(([key,select])=>select.value===''||row[key]===select.value))}function renderHourly(){const rows=selectedRows();table(document.querySelector('#hourly'),['link_id','hour_start_seconds','entry_vehicles','exit_vehicles','urban_area','road_type','road_size'],rows.map(row=>[row.link_id,row.hour_start_seconds,row.entry_vehicles,row.exit_vehicles,row.urban_area,row.road_type,row.road_size]));renderGroups(rows);updateMap()}function renderGroups(rows){const groups=new Map();rows.forEach(row=>D.map(([dimension])=>[dimension,row[dimension]]).forEach(([dimension,category])=>{const key=JSON.stringify([dimension,category,row.hour_start_seconds]);let group=groups.get(key);if(!group){group={dimension,category,hour:row.hour_start_seconds,eligible:0,used:0};groups.set(key,group)}group.eligible++;if(row.entry_vehicles+row.exit_vehicles>0)group.used++}));const values=[...groups.values()].map(group=>[group.dimension,group.category,group.hour,group.eligible,group.used,group.eligible-group.used,(group.used*100/group.eligible).toFixed(6)]);table(document.querySelector('#groups'),['Dimension','Group','Hour start (s)','Eligible','Used','Unused','Used (%)'],values)}function updateMap(){document.querySelectorAll('#network-map line').forEach(line=>{line.style.display=selectors.every(([key,select])=>select.value===''||line.getAttribute('data-'+key.replace('_','-'))===select.value)?'':'none'})}renderHourly()__NETWORK_ANALYSIS_SCRIPT__</script></body></html>"#;

/// Renders the agent travel tables. They quote person identifiers, so the header and every row
/// are split with a quote-aware parser instead of `String.split(',')`.
pub(super) const CSV_TABLE_SCRIPT: &str = "function parseCsv(line){const fields=[];let field='',quoted=false;for(let i=0;i<line.length;i++){const ch=line[i];if(ch.charCodeAt(0)===34){if(quoted&&line.charCodeAt(i+1)===34){field+=String.fromCharCode(34);i++}else{quoted=!quoted}}else if(ch===','&&!quoted){fields.push(field);field=''}else{field+=ch}}fields.push(field);return fields}function csvTable(id,rows){table(document.querySelector(id),parseCsv(rows[0]),rows.slice(1).map(parseCsv))}";

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

pub(super) fn write_report(
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
    let metrics = json_for_script(&metrics(manifest.excess_delay_clip_seconds.is_some()))?;
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
    let html = render_template(
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

pub(super) fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::{render_template, write_report};
    use crate::simulation::analysis::{LinkHourlyMetric, Manifest, ModuleStatus};
    use std::fs;

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

        write_report(directory.path(), &manifest, &statuses, &link_hourly).unwrap();

        let html = fs::read_to_string(directory.path().join("index.html")).unwrap();
        assert!(html.contains("Completed final iteration 3"));
        assert!(html.contains("link-1"));
        assert!(html.contains("\"entry_vehicles\":2"));
    }
}
