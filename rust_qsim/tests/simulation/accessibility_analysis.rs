//! Accessibility to supplied opportunities: thresholds, weights, missing costs and the
//! declared measure's exports.
//!
//! Every test drives the shared analysis interface, so it verifies the published tables, the
//! local report and the module status together rather than any one of them alone.

use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration, capture_expected_travel,
};
use rust_qsim::simulation::config::{Accessibility, Analysis, CompressionType};
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::network::{Link, Network};
use rust_qsim::simulation::scenario::population::{
    InternalActivity, InternalPerson, InternalPlan, InternalPlanElement, Population,
};
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Three zones at distinct coordinates, plus one that no cost ever leaves.
const ZONES: &str = "zone_id,x,y\nz1,0,0\nz2,1000,0\nz3,0,1000\nz4,2000,2000\n";

/// Job, school and service locations. The weights are deliberately uneven and include a
/// zero, so a total that ignores weights or counts locations cannot pass.
const OPPORTUNITIES: &str = "opportunity_id,category,x,y,count\n\
    job-a,jobs,0,0,100\n\
    job-b,jobs,1000,0,250\n\
    job-c,jobs,0,1000,50\n\
    school-a,schools,0,0,40\n\
    service-a,services,1000,0,0\n";

/// A complete car matrix at the 08:00 departure period, plus a walk table at 16:00 only.
/// The walk matrix makes (walk, 28800) a mode/period combination the cost file never
/// supplies, which has to be reported as unavailable rather than as zero accessibility.
const TRAVEL_COSTS: &str = "origin_zone,destination_zone,mode,period_start_seconds,travel_time_seconds\n\
    z1,z1,car,28800,0\n\
    z1,z2,car,28800,1800\n\
    z1,z3,car,28800,3600\n\
    z2,z1,car,28800,1800\n\
    z2,z2,car,28800,0\n\
    z2,z3,car,28800,5400\n\
    z3,z1,car,28800,3600\n\
    z3,z2,car,28800,5400\n\
    z3,z3,car,28800,0\n\
    z1,z1,walk,57600,0\n\
    z1,z2,walk,57600,600\n";

/// The thresholds the nominal report uses. 1800 s is a cost in the matrix and a threshold, so
/// every assertion about it also pins the inclusive boundary.
const THRESHOLDS: [f64; 2] = [1800.0, 3600.0];

/// A run with one empty final-iteration event partition, which is all the required
/// link-coverage module needs to publish a report.
fn empty_run(output: &Path, iteration: u32) {
    let events = output.join(format!("ITERS/it.{iteration}/events"));
    fs::create_dir_all(&events).unwrap();
    fs::write(events.join("events.0.xml"), "<events></events>").unwrap();
}

fn metadata(
    expected_travel: Vec<rust_qsim::simulation::analysis::PersonExpectedTravel>,
) -> AnalysisRunMetadata {
    AnalysisRunMetadata::from_run(
        4711,
        1.0,
        0,
        &Garage::default(),
        expected_travel,
        AnalysisInputPaths::default(),
    )
}

/// The three supplied files, and the settings that name them.
///
/// The configured paths are relative, so the report records them the way a researcher would
/// configure them; they resolve from the run's output directory, exactly like the
/// observed-data path.
struct Inputs {
    settings: Accessibility,
    zones: String,
    opportunities: String,
    costs: String,
}

impl Inputs {
    fn new(zones: &str, opportunities: &str, costs: &str, thresholds: &[f64]) -> Self {
        Self {
            settings: Accessibility {
                opportunities: Some(PathBuf::from("accessibility/opportunities.csv")),
                zones: Some(PathBuf::from("accessibility/zones.csv")),
                travel_costs: Some(PathBuf::from("accessibility/travel_costs.csv")),
                thresholds_seconds: thresholds.to_vec(),
            },
            zones: zones.to_owned(),
            opportunities: opportunities.to_owned(),
            costs: costs.to_owned(),
        }
    }

    /// Materialize the inputs under `output`, where their relative paths resolve from.
    fn install(&self, output: &Path) {
        let dir = output.join("accessibility");
        fs::create_dir_all(&dir).unwrap();
        for (configured, content) in [
            (&self.settings.zones, &self.zones),
            (&self.settings.opportunities, &self.opportunities),
            (&self.settings.travel_costs, &self.costs),
        ] {
            fs::write(
                dir.join(configured.as_deref().unwrap().file_name().unwrap()),
                content,
            )
            .unwrap();
        }
    }
}

fn nominal_inputs(thresholds: &[f64]) -> Inputs {
    Inputs::new(ZONES, OPPORTUNITIES, TRAVEL_COSTS, thresholds)
}

/// Run the analysis with the given accessibility settings and return the report directory.
fn analyze(
    output: &Path,
    settings: Accessibility,
    expected_travel: Vec<rust_qsim::simulation::analysis::PersonExpectedTravel>,
) -> Result<PathBuf, rust_qsim::simulation::analysis::AnalysisError> {
    empty_run(output, 0);
    analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &metadata(expected_travel),
        &Network::new(),
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
            accessibility: settings,
            ..Analysis::default()
        },
    )
    .map(|report| report.parent().unwrap().to_owned())
}

fn table(report: &Path, name: &str) -> String {
    fs::read_to_string(report.join(name))
        .unwrap_or_else(|error| panic!("cannot read {name}: {error}"))
}

/// Index one CSV by a selection of its columns, so an assertion names the row it means
/// instead of relying on the export order.
fn rows(csv: &str) -> Vec<HashMap<String, String>> {
    let mut lines = csv.lines();
    let header: Vec<String> = lines
        .next()
        .expect("a report table has a header")
        .split(',')
        .map(str::to_owned)
        .collect();
    lines
        .map(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            assert_eq!(fields.len(), header.len(), "row does not match header");
            header
                .iter()
                .cloned()
                .zip(fields.iter().map(|field| field.to_string()))
                .collect()
        })
        .collect()
}

fn zone_row(
    report: &Path,
    origin: &str,
    category: &str,
    mode: &str,
    threshold: u64,
) -> HashMap<String, String> {
    rows(&table(report, "accessibility_zones.csv"))
        .into_iter()
        .find(|row| {
            row["origin_zone"] == format!("\"{origin}\"")
                && row["category"] == format!("\"{category}\"")
                && row["mode"] == format!("\"{mode}\"")
                && row["departure_period_start_seconds"] == "28800"
                && row["threshold_seconds"] == format!("{threshold}.000000")
        })
        .unwrap_or_else(|| panic!("no zone row for {origin}/{category}/{mode}/{threshold}s"))
}

fn summary_row(
    report: &Path,
    category: &str,
    mode: &str,
    threshold: u64,
) -> HashMap<String, String> {
    rows(&table(report, "accessibility_summary.csv"))
        .into_iter()
        .find(|row| {
            row["category"] == format!("\"{category}\"")
                && row["mode"] == format!("\"{mode}\"")
                && row["departure_period_start_seconds"] == "28800"
                && row["threshold_seconds"] == format!("{threshold}.000000")
        })
        .unwrap_or_else(|| panic!("no summary row for {category}/{mode}/{threshold}s"))
}

fn person_row(
    report: &Path,
    person: &str,
    category: &str,
    threshold: u64,
) -> HashMap<String, String> {
    rows(&table(report, "accessibility_persons.csv"))
        .into_iter()
        .find(|row| {
            row["person_id"] == format!("\"{person}\"")
                && row["category"] == format!("\"{category}\"")
                && row["mode"] == "\"car\""
                && row["threshold_seconds"] == format!("{threshold}.000000")
        })
        .unwrap_or_else(|| panic!("no person row for {person}/{category}/{threshold}s"))
}

fn module_status(report: &Path, module: &str) -> (String, String) {
    let statuses = table(report, "module_status.json");
    let value: serde_json::Value = serde_json::from_str(&statuses).unwrap();
    let entry = value
        .as_array()
        .expect("module statuses are an array")
        .iter()
        .find(|entry| entry["module"] == module)
        .unwrap_or_else(|| panic!("no status for module {module}"));
    (
        entry["status"].as_str().unwrap().to_owned(),
        entry["reason"].as_str().unwrap_or_default().to_owned(),
    )
}

#[deterministic_id_test(rust_qsim)]
fn cumulative_opportunities_count_weights_within_an_inclusive_threshold() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let inputs = nominal_inputs(&THRESHOLDS);
    inputs.install(output);
    let report = analyze(output, inputs.settings, Vec::new()).unwrap();

    // The declared measure is named on every row, so a consumer never has to infer it.
    let z1_jobs_1800 = zone_row(&report, "z1", "jobs", "car", 1800);
    assert_eq!(
        z1_jobs_1800["measure"],
        "cumulative_opportunities_within_threshold"
    );
    // job-a (100) sits in z1 and job-b (250) is exactly 1800 s away, which the inclusive
    // threshold counts. job-c is 3600 s away, so it is out of reach at this threshold.
    assert_eq!(z1_jobs_1800["status"], "available");
    assert_eq!(z1_jobs_1800["opportunities"], "350.000000");
    assert_eq!(z1_jobs_1800["reachable_opportunity_locations"], "2");
    assert_eq!(z1_jobs_1800["unreachable_opportunity_locations"], "1");
    assert_eq!(z1_jobs_1800["opportunity_locations_without_cost"], "0");
    // 350 of the 400 job weight, not three of three locations.
    assert_eq!(z1_jobs_1800["total_opportunities"], "400.000000");
    assert_eq!(z1_jobs_1800["reachable_opportunity_share"], "0.875000");

    // The larger threshold admits the third location. The boundary is the same 1800 s cost
    // counted at both thresholds, so nothing about it depends on the threshold chosen.
    let z1_jobs_3600 = zone_row(&report, "z1", "jobs", "car", 3600);
    assert_eq!(z1_jobs_3600["opportunities"], "400.000000");
    assert_eq!(z1_jobs_3600["unreachable_opportunity_locations"], "0");
    assert_eq!(z1_jobs_3600["reachable_opportunity_share"], "1.000000");

    // z3 can only reach itself, so the total is the weight of its own single location.
    let z3_jobs_1800 = zone_row(&report, "z3", "jobs", "car", 1800);
    assert_eq!(z3_jobs_1800["opportunities"], "50.000000");
    assert_eq!(z3_jobs_1800["reachable_opportunity_locations"], "1");
    assert_eq!(z3_jobs_1800["reachable_opportunity_share"], "0.125000");

    // A different category at the same threshold: only the school, at its own weight.
    let z1_schools = zone_row(&report, "z1", "schools", "car", 1800);
    assert_eq!(z1_schools["opportunities"], "40.000000");
    assert_eq!(z1_schools["total_opportunities"], "40.000000");
    assert_eq!(z1_schools["reachable_opportunity_share"], "1.000000");

    // A reachable location with a zero weight is still a reachable location, and the share of
    // a zero-weight category is blank rather than a division by zero.
    let z1_services = zone_row(&report, "z1", "services", "car", 1800);
    assert_eq!(z1_services["status"], "available");
    assert_eq!(z1_services["opportunities"], "0.000000");
    assert_eq!(z1_services["reachable_opportunity_locations"], "1");
    assert_eq!(z1_services["total_opportunities"], "0.000000");
    assert_eq!(z1_services["reachable_opportunity_share"], "");

    // Every origin zone, category, mode, period and threshold combination is exported, so a
    // gap in the cost file cannot hide a missing row.
    let zone_rows = rows(&table(&report, "accessibility_zones.csv"));
    // 3 categories x 2 modes x 2 periods x 2 thresholds x 4 origin zones.
    assert_eq!(zone_rows.len(), 3 * 2 * 2 * 2 * 4);
    // The walk table only covers 57600, and the car table only 28800, so each mode is
    // reported at both periods rather than only where it happens to have data.
    let walk_28800 = zone_row(&report, "z1", "jobs", "walk", 1800);
    assert_eq!(walk_28800["status"], "unavailable:no_travel_costs");
    assert_eq!(walk_28800["opportunities"], "");

    // The report renders the measure, the map and the tables, and links the full CSVs.
    let html = fs::read_to_string(report.join("index.html")).unwrap();
    assert!(html.contains("Accessibility to supplied opportunities"));
    assert!(html.contains("cumulative_opportunities_within_threshold"));
    assert!(html.contains("id=\"accessibility-zones\""));
    assert!(html.contains("id=\"accessibility-summary\""));
    assert!(html.contains("id=\"accessibility-persons\""));
    assert!(html.contains("id=\"accessibility-map\""));
    assert!(html.contains("accessibility_zones.csv"));
    assert!(report.join("accessibility_map.svg").is_file());
    let map = fs::read_to_string(report.join("accessibility_map.svg")).unwrap();
    assert!(
        map.contains("<title>z1 | 350.000000 opportunities of jobs within 1800 s"),
        "the map does not label the zone's measure"
    );
    // An origin with no supplied cost is drawn outside the ramp, never as a low value.
    assert!(map.contains("unavailable:no_origin_costs"));
}

#[deterministic_id_test(rust_qsim)]
fn missing_costs_are_excluded_and_reported_instead_of_counted_as_reachable() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    // z1 has no cost to z3, so the z3 locations cannot be reached from z1. z4 has no cost at
    // all, so nothing can be reached from it.
    let costs = TRAVEL_COSTS.replace("z1,z3,car,28800,3600\n", "");
    let inputs = Inputs::new(ZONES, OPPORTUNITIES, &costs, &THRESHOLDS);
    inputs.install(output);
    let report = analyze(output, inputs.settings, Vec::new()).unwrap();

    // A destination with no supplied cost is neither reachable nor unreachable: it is
    // excluded and counted, so the measure cannot claim a partial cost matrix is complete.
    let z1_jobs = zone_row(&report, "z1", "jobs", "car", 3600);
    assert_eq!(z1_jobs["status"], "available_missing_costs");
    assert_eq!(z1_jobs["opportunities"], "350.000000");
    assert_eq!(z1_jobs["reachable_opportunity_locations"], "2");
    assert_eq!(z1_jobs["unreachable_opportunity_locations"], "0");
    assert_eq!(z1_jobs["opportunity_locations_without_cost"], "1");
    // The share is against the whole category, so the missing cost lowers it rather than
    // silently shrinking the denominator.
    assert_eq!(z1_jobs["total_opportunities"], "400.000000");
    assert_eq!(z1_jobs["reachable_opportunity_share"], "0.875000");

    // An origin with no outgoing cost has no measure at all. Reporting zero would be a claim
    // the inputs do not support.
    let z4_jobs = zone_row(&report, "z4", "jobs", "car", 1800);
    assert_eq!(z4_jobs["status"], "unavailable:no_origin_costs");
    assert_eq!(z4_jobs["opportunities"], "");
    assert_eq!(z4_jobs["reachable_opportunity_locations"], "");
    assert_eq!(z4_jobs["reachable_opportunity_share"], "");

    // The summary counts the unusable zones instead of folding them in as zero-opportunity
    // zones, which would drag every mean down.
    let jobs = summary_row(&report, "jobs", "car", 1800);
    assert_eq!(jobs["origin_zones"], "4");
    assert_eq!(jobs["zones_without_costs"], "1");
    assert_eq!(jobs["measure"], "cumulative_opportunities_within_threshold");
    // z1 = 350, z2 = 350, z3 = 50 and z4 is excluded, so the mean is over three zones.
    assert_eq!(jobs["mean_opportunities"], "250.000000");
    assert_eq!(jobs["min_opportunities"], "50.000000");
    assert_eq!(jobs["max_opportunities"], "350.000000");
    assert_eq!(jobs["median_opportunities"], "350.000000");

    let diagnostics = rows(&table(&report, "accessibility_diagnostics.csv"));
    let diagnostic = |metric: &str| {
        diagnostics
            .iter()
            .find(|row| row["metric"] == metric)
            .unwrap_or_else(|| panic!("no {metric} diagnostic"))["value"]
            .clone()
    };
    assert_eq!(diagnostic("cost_rows"), "10");
    assert_eq!(diagnostic("cost_tables"), "2");
    assert_eq!(diagnostic("cost_rows_with_unknown_zones"), "0");
    assert_eq!(diagnostic("zones"), "4");
    assert_eq!(diagnostic("opportunity_locations"), "5");
    assert_eq!(diagnostic("opportunity_weights"), "440.000000");
}

#[deterministic_id_test(rust_qsim)]
fn cost_rows_for_unknown_zones_are_counted_rather_than_treated_as_missing_pairs() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    // A rectangular cost matrix is routinely wider than the zones under study, so a row that
    // names an unlisted zone is counted instead of failing the module.
    let costs = format!("{TRAVEL_COSTS}z1,zone-outside,car,28800,600\n");
    let inputs = Inputs::new(ZONES, OPPORTUNITIES, &costs, &THRESHOLDS);
    inputs.install(output);
    let report = analyze(output, inputs.settings, Vec::new()).unwrap();

    assert_eq!(module_status(&report, "accessibility").0, "complete");
    let diagnostics = table(&report, "accessibility_diagnostics.csv");
    assert!(diagnostics.contains("cost_rows,12\n"), "{diagnostics}");
    assert!(diagnostics.contains("cost_rows_with_unknown_zones,1\n"));
    // The ignored row does not become a missing cost for a real destination.
    assert_eq!(
        zone_row(&report, "z1", "jobs", "car", 1800)["opportunity_locations_without_cost"],
        "0"
    );
}

#[deterministic_id_test(rust_qsim)]
fn person_accessibility_follows_their_home_zone_and_counts_unplaceable_people() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let inputs = nominal_inputs(&THRESHOLDS);
    inputs.install(output);
    let population = population();
    let report = analyze(
        output,
        inputs.settings,
        capture_expected_travel(&population),
    )
    .unwrap();

    // Two people live in z1 and one in z3, so the person table repeats each zone's own value.
    let near_z1 = person_row(&report, "near-z1", "jobs", 1800);
    assert_eq!(near_z1["origin_zone"], "\"z1\"");
    assert_eq!(near_z1["status"], "available");
    assert_eq!(near_z1["opportunities"], "350.000000");
    assert_eq!(near_z1["home_x"], "10.000000");
    assert_eq!(near_z1["home_y"], "10.000000");

    let near_z3 = person_row(&report, "near-z3", "jobs", 1800);
    assert_eq!(near_z3["origin_zone"], "\"z3\"");
    assert_eq!(near_z3["opportunities"], "50.000000");

    // A person with no placeable home keeps a row of their own instead of disappearing from
    // the population.
    let unplaceable = person_row(&report, "traveller", "jobs", 1800);
    assert_eq!(unplaceable["origin_zone"], "");
    assert_eq!(unplaceable["status"], "unavailable:no_home_zone");
    assert_eq!(unplaceable["opportunities"], "");

    let diagnostics = table(&report, "accessibility_diagnostics.csv");
    assert!(diagnostics.contains("persons,4\n"));
    assert!(diagnostics.contains("persons_without_home_zone,1\n"));
}

#[deterministic_id_test(rust_qsim)]
fn the_population_weighted_mean_differs_from_the_zone_mean_when_people_are_unevenly_spread() {
    // The equity signal only exists when the two disagree, so this population is built to
    // make them disagree: the populous zone is the well-served one.
    //
    // z1 reaches 350 opportunities and holds three of the four people; z3 reaches 50 and holds
    // one. The three zones that have a supplied cost are z1, z2 and z3, at 350, 350 and 50, so
    // the unweighted zone mean is 250. What a person actually faces is (350*3 + 50*1) / 4 = 275.
    // A run reporting only the zone mean would understate the typical person by 25
    // opportunities, and one reporting only the weighted mean would misdescribe the territory.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let inputs = nominal_inputs(&[1800.0]);
    inputs.install(output);
    let report = analyze(
        output,
        inputs.settings,
        capture_expected_travel(&uneven_population()),
    )
    .unwrap();

    let jobs = summary_row(&report, "jobs", "car", 1800);
    assert_eq!(jobs["origin_zones"], "4");
    // z4 has no supplied cost, so it is counted and excluded rather than folded in as a zero.
    assert_eq!(jobs["zones_without_costs"], "1");
    assert_eq!(jobs["persons_included"], "4");
    // The zone mean covers the three zones with a supplied cost: (350 + 350 + 50) / 3. z2 holds
    // no people but is still a zone of the territory, so it belongs in the mean.
    assert_eq!(jobs["mean_opportunities"], "250.000000");
    // The person-weighted mean is what the four people actually face: (1050 + 50) / 4.
    assert_eq!(jobs["population_weighted_opportunities"], "275.000000");
    // The point of the pair: they are not the same number, so neither alone describes the run.
    assert_ne!(
        jobs["mean_opportunities"],
        jobs["population_weighted_opportunities"]
    );
}

#[deterministic_id_test(rust_qsim)]
fn a_partial_input_set_fails_only_the_accessibility_module() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let mut settings = nominal_inputs(&THRESHOLDS).settings;
    // The three files are one input; supplying two of them cannot produce a measure.
    settings.travel_costs = None;

    let report = analyze(output, settings, Vec::new()).unwrap();
    let (status, reason) = module_status(&report, "accessibility");
    assert_eq!(status, "failed");
    assert!(reason.contains("travel_costs"), "{reason}");
    // The required module and the other optional ones are untouched, and the core report is
    // still published.
    assert_eq!(module_status(&report, "link_coverage").0, "complete");
    assert_eq!(module_status(&report, "agent_travel").0, "complete");
    assert!(table(&report, "link_hourly.csv").contains("link_id"));
    assert!(report.join("index.html").is_file());
    // The empty tables carry headers only, and the diagnostics name the reason.
    assert_eq!(table(&report, "accessibility_zones.csv").lines().count(), 1);
    assert_eq!(
        table(&report, "accessibility_persons.csv").lines().count(),
        1
    );
    assert_eq!(
        table(&report, "accessibility_summary.csv").lines().count(),
        1
    );
    let diagnostics = table(&report, "accessibility_diagnostics.csv");
    assert!(
        diagnostics.contains("module_error,\"output.analysis.accessibility needs travel_costs"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("measure,\"cumulative_opportunities_within_threshold\""),
        "{diagnostics}"
    );
    let html = fs::read_to_string(report.join("index.html")).unwrap();
    assert!(html.contains("Accessibility to supplied opportunities"));
    assert!(html.contains("needs travel_costs"));
}

#[deterministic_id_test(rust_qsim)]
fn accessibility_stays_unavailable_until_its_inputs_are_configured() {
    let temp = tempfile::tempdir().unwrap();
    let report = analyze(temp.path(), Accessibility::default(), Vec::new()).unwrap();

    let (status, reason) = module_status(&report, "accessibility");
    assert_eq!(status, "unavailable");
    assert!(
        reason.contains("No accessibility inputs are configured"),
        "{reason}"
    );
    assert_eq!(table(&report, "accessibility_zones.csv").lines().count(), 1);
    assert!(table(&report, "accessibility_diagnostics.csv").contains("not_configured"));
    // An unconfigured module must not claim the measure in the catalog, or a consumer would
    // look for a table that holds only headers.
    let catalog = table(&report, "metric_catalog.json");
    assert!(!catalog.contains("cumulative_opportunities_within_threshold"));
    assert!(!catalog.contains("\"opportunities\""));
    let html = fs::read_to_string(report.join("index.html")).unwrap();
    assert!(html.contains("No accessibility inputs are configured"));
}

#[deterministic_id_test(rust_qsim)]
fn the_measure_is_registered_in_the_metric_catalog_for_comparison_and_equity() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let inputs = nominal_inputs(&THRESHOLDS);
    inputs.install(output);
    let report = analyze(output, inputs.settings, Vec::new()).unwrap();

    let catalog: serde_json::Value =
        serde_json::from_str(&table(&report, "metric_catalog.json")).unwrap();
    // The same two keys the catalog and the report declare, so this test cannot drift from
    // the module's own aggregation contract.
    const ACCESSIBILITY_AGGREGATION_KEY: &str =
        "origin_zone,category,mode,departure_period_start_seconds,threshold_seconds";
    const ACCESSIBILITY_SUMMARY_KEY: &str =
        "category,mode,departure_period_start_seconds,threshold_seconds";
    let entries = catalog.as_array().expect("the catalog is an array");
    let entry = |name: &str| {
        entries
            .iter()
            .find(|entry| entry["name"] == name)
            .unwrap_or_else(|| panic!("{name} is not registered in the metric catalog"))
    };
    // The catalog names the column, per the rule in docs/architecture.md, so a consumer can look
    // each metric up in the table that exports it. The declared measure name travels in that
    // table's own `measure` column instead.
    assert_eq!(entry("opportunities")["unit"], "opportunities");
    // Only the accessibility entries, which are the ones this module adds. The catalog also
    // holds every other module's metrics, whose tables this test does not read.
    let accessibility_keys = [ACCESSIBILITY_AGGREGATION_KEY, ACCESSIBILITY_SUMMARY_KEY];
    let registered: Vec<&str> = entries
        .iter()
        .filter(|entry| {
            accessibility_keys
                .iter()
                .any(|key| entry["aggregation_key"] == *key)
        })
        .map(|entry| entry["name"].as_str().unwrap())
        .collect();
    assert!(
        !registered.is_empty(),
        "no catalog entry declares an accessibility aggregation key"
    );
    // Every accessibility metric must be a real column of one of the exported tables, which is
    // the rule that makes the catalog usable: a consumer looks a name up in the table.
    let headers: Vec<Vec<String>> = ["accessibility_zones.csv", "accessibility_summary.csv"]
        .iter()
        .map(|name| {
            table(&report, name)
                .lines()
                .next()
                .unwrap()
                .split(',')
                .map(|field| field.trim_matches('"').to_owned())
                .collect()
        })
        .collect();
    for name in registered {
        assert!(
            headers
                .iter()
                .any(|header| header.iter().any(|field| field == name)),
            "{name} is catalogued but is not a column of any accessibility table"
        );
    }
    // The measure is still discoverable, through the column that declares it.
    assert_eq!(
        zone_row(&report, "z1", "jobs", "car", 1800)["measure"],
        "cumulative_opportunities_within_threshold"
    );

    // The aggregation key names the origin, category, mode, period and threshold, which is
    // what a comparison needs to line two runs up and an equity tool needs to group by.
    for name in [
        "opportunities",
        "reachable_opportunity_share",
        "opportunity_locations_without_cost",
    ] {
        assert_eq!(
            entry(name)["aggregation_key"],
            ACCESSIBILITY_AGGREGATION_KEY,
            "{name} declares the wrong aggregation key"
        );
    }
    for name in [
        "mean_opportunities",
        "population_weighted_opportunities",
        "zones_without_costs",
        "persons_included",
    ] {
        assert_eq!(
            entry(name)["aggregation_key"],
            ACCESSIBILITY_SUMMARY_KEY,
            "{name} declares the wrong aggregation key"
        );
    }

    // The report's own copy of the catalog is the published one, so a consumer reading the
    // page and a consumer reading the file cannot disagree.
    let html = fs::read_to_string(report.join("index.html")).unwrap();
    assert!(html.contains("cumulative_opportunities_within_threshold"));
    assert!(html.contains("\"name\":\"opportunities\""));
    assert!(html.contains(ACCESSIBILITY_AGGREGATION_KEY));
}

#[deterministic_id_test(rust_qsim)]
fn duplicate_thresholds_and_reordered_inputs_produce_identical_tables() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();

    let first = nominal_inputs(&[1800.0, 1800.0, 3600.0]);
    first.install(output);
    let first_report = analyze(output, first.settings, Vec::new()).unwrap();
    // A repeated threshold is one row, not two: the measure would be reported twice under two
    // names for the same number.
    let thresholds: Vec<String> = rows(&table(&first_report, "accessibility_zones.csv"))
        .iter()
        .map(|row| row["threshold_seconds"].clone())
        .collect();
    // Three configured thresholds collapse to two rows, because 1800 was configured twice and
    // the measure would otherwise be reported twice under the same value.
    let distinct: std::collections::BTreeSet<&String> = thresholds.iter().collect();
    assert_eq!(distinct.len(), 2);
    assert!(distinct.contains(&"1800.000000".to_owned()));
    assert!(distinct.contains(&"3600.000000".to_owned()));

    // The same inputs in a different order produce byte-identical tables, because zones,
    // opportunities, thresholds and cost tables are all sorted before use.
    let second_output = temp.path().join("second");
    fs::create_dir_all(&second_output).unwrap();
    let shuffled_zones = "zone_id,x,y\nz3,0,1000\nz1,0,0\nz4,2000,2000\nz2,1000,0\n";
    let shuffled_opportunities = "opportunity_id,category,x,y,count\n\
        service-a,services,1000,0,0\n\
        school-a,schools,0,0,40\n\
        job-c,jobs,0,1000,50\n\
        job-b,jobs,1000,0,250\n\
        job-a,jobs,0,0,100\n";
    let second = Inputs::new(
        shuffled_zones,
        shuffled_opportunities,
        TRAVEL_COSTS,
        &THRESHOLDS,
    );
    second.install(&second_output);
    let second_report = analyze(&second_output, second.settings, Vec::new()).unwrap();

    for name in [
        "accessibility_zones.csv",
        "accessibility_summary.csv",
        "accessibility_map.svg",
    ] {
        assert_eq!(
            table(&first_report, name),
            table(&second_report, name),
            "{name} depends on the input file order"
        );
    }
}

#[deterministic_id_test(rust_qsim)]
fn invalid_supplied_inputs_fail_the_module_with_the_offending_row() {
    // Each case names the text a researcher has to look for, so a failure report is
    // actionable without opening the source.
    let cases: [(&str, &str, &str, &str, &str); 6] = [
        (
            "negative weight",
            ZONES,
            "opportunity_id,category,x,y,count\njob-a,jobs,0,0,-1\n",
            TRAVEL_COSTS,
            "has an invalid weight -1",
        ),
        (
            "duplicate location",
            ZONES,
            "opportunity_id,category,x,y,count\njob-a,jobs,0,0,10\njob-a,jobs,0,0,5\n",
            TRAVEL_COSTS,
            "appears more than once",
        ),
        (
            "duplicate zone",
            "zone_id,x,y\nz1,0,0\nz1,10,10\n",
            OPPORTUNITIES,
            TRAVEL_COSTS,
            "appears more than once",
        ),
        (
            "no zone",
            "zone_id,x,y\n",
            OPPORTUNITIES,
            TRAVEL_COSTS,
            "lists no zone",
        ),
        (
            "negative potential cost",
            ZONES,
            OPPORTUNITIES,
            "origin_zone,destination_zone,mode,period_start_seconds,travel_time_seconds\nz1,z2,car,28800,-60\n",
            "is -60",
        ),
        (
            "duplicate cost key",
            ZONES,
            OPPORTUNITIES,
            "origin_zone,destination_zone,mode,period_start_seconds,travel_time_seconds\nz1,z2,car,28800,600\nz1,z2,car,28800,900\n",
            "more than one travel cost",
        ),
    ];
    for (name, zones, opportunities, costs, expected) in cases {
        let temp = tempfile::tempdir().unwrap();
        let output = temp.path();
        let inputs = Inputs::new(zones, opportunities, costs, &THRESHOLDS);
        inputs.install(output);
        let result = analyze(output, inputs.settings, Vec::new());
        // A broken input fails the module rather than the run, so the other modules still
        // publish a report.
        let report = match result {
            Ok(report) => report,
            Err(error) => panic!("{name} should fail only the module, got {error}"),
        };
        let (status, reason) = module_status(&report, "accessibility");
        assert_eq!(status, "failed", "{name} was not reported as failed");
        assert!(reason.contains(expected), "{name}: {reason}");
        assert_eq!(module_status(&report, "link_coverage").0, "complete");
    }
}

#[deterministic_id_test(rust_qsim)]
fn an_empty_threshold_list_is_refused_before_any_table_is_written() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let inputs = nominal_inputs(&[]);
    inputs.install(output);
    let report = analyze(output, inputs.settings, Vec::new()).unwrap();
    let (status, reason) = module_status(&report, "accessibility");
    assert_eq!(status, "failed");
    assert!(
        reason.contains("thresholds_seconds must not be empty"),
        "{reason}"
    );
}

/// A home plan whose first activity is the person's home location.
fn home_plan(home: [f64; 2]) -> InternalPlan {
    let link = || Id::<Link>::create("l");
    let activity = |kind: &str, coord: Option<Coordinate>| {
        InternalPlanElement::Activity(InternalActivity::new(coord, kind, link(), None, None, None))
    };
    InternalPlan {
        score: None,
        selected: true,
        elements: vec![
            activity("home", Some(Coordinate::new_2d(home[0], home[1]))),
            // A stage activity after the home must not become the origin, or the person would
            // be placed at a transit stop.
            activity("pt interaction", Some(Coordinate::new_2d(2000.0, 2000.0))),
            activity("work", Some(Coordinate::new_2d(-50.0, -50.0))),
        ],
        attributes: Default::default(),
    }
}

/// Three people near z1 and one near z3, with no unplaceable person.
///
/// The zone mean and the person-weighted mean differ for this population, which is what the
/// equity pair is for: see `the_population_weighted_mean_differs_from_the_zone_mean...`.
fn uneven_population() -> Population {
    Population::from_persons(vec![
        InternalPerson::new(Id::create("z1-a"), home_plan([10.0, 10.0])),
        InternalPerson::new(Id::create("z1-b"), home_plan([-10.0, 10.0])),
        InternalPerson::new(Id::create("z1-c"), home_plan([0.0, -10.0])),
        InternalPerson::new(Id::create("z3-a"), home_plan([0.0, 900.0])),
    ])
}

/// A population whose home activity coordinates place two people in z1, one in z3, and leave
/// one person without a placeable home.
fn population() -> Population {
    let link = || Id::<Link>::create("l");
    let activity = |kind: &str, coord: Option<Coordinate>| {
        InternalPlanElement::Activity(InternalActivity::new(coord, kind, link(), None, None, None))
    };
    let plan = |home: Option<Coordinate>| InternalPlan {
        score: None,
        selected: true,
        elements: vec![
            activity("home", home),
            // A stage activity before the first real one must not become the home, or the
            // person would be placed at a transit stop.
            activity("pt interaction", Some(Coordinate::new_2d(2000.0, 2000.0))),
            activity("work", Some(Coordinate::new_2d(-50.0, -50.0))),
        ],
        attributes: Default::default(),
    };
    Population::from_persons(vec![
        InternalPerson::new(
            Id::create("near-z1"),
            plan(Some(Coordinate::new_2d(10.0, 10.0))),
        ),
        InternalPerson::new(
            Id::create("near-z1-b"),
            plan(Some(Coordinate::new_2d(-10.0, 10.0))),
        ),
        InternalPerson::new(
            Id::create("near-z3"),
            plan(Some(Coordinate::new_2d(0.0, 900.0))),
        ),
        // Only a stage activity, so the plan has no home to place the person by.
        InternalPerson::new(
            Id::create("traveller"),
            InternalPlan {
                score: None,
                selected: true,
                elements: vec![activity(
                    "pt interaction",
                    Some(Coordinate::new_2d(500.0, 500.0)),
                )],
                attributes: Default::default(),
            },
        ),
    ])
}
