//! Behavioural verification of the daily activity-pattern and zonal origin-destination
//! reports, driven through the same `analyze_final_iteration` interface a run uses.

use macros::deterministic_id_test;
use rust_qsim::simulation::InternalAttributes;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration, capture_expected_travel,
};
use rust_qsim::simulation::config::{Analysis, CompressionType, LinkLabels, ZoneSystem};
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::network::{Link, Network, Node};
use rust_qsim::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
    InternalPlanElement, InternalRoute, Population,
};
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

/// A published report plus the temporary output it came from.
///
/// The output directory is deleted when the report is dropped, so it is owned here rather than
/// borrowed from the test that ran the analysis.
struct PatternReport {
    dir: PathBuf,
    _output: TempDir,
}

impl PatternReport {
    fn read(&self, table: &str) -> String {
        fs::read_to_string(self.dir.join(table))
            .unwrap_or_else(|error| panic!("cannot read {table}: {error}"))
    }

    /// The single data row of a table whose key column is `column = key`, as a column map.
    fn row(&self, table: &str, column: &str, key: &str) -> BTreeMap<String, String> {
        rows(&self.read(table))
            .into_iter()
            .find(|row| row[column] == key)
            .unwrap_or_else(|| {
                panic!(
                    "{table} has no row with {column} = {key}:\n{}",
                    self.read(table)
                )
            })
    }
}

/// A CSV table as column maps, one per data row.
///
/// The report quotes every string it writes, so identifiers arrive quoted; they are unquoted
/// here so a test can look a row up by the value a reader sees.
fn rows(table: &str) -> Vec<BTreeMap<String, String>> {
    let mut lines = table.lines();
    let header: Vec<String> = lines
        .next()
        .expect("a report table has a header")
        .split(',')
        .map(unquote)
        .collect();
    lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            header
                .iter()
                .cloned()
                .zip(line.split(',').map(unquote))
                .collect()
        })
        .collect()
}

fn unquote(value: &str) -> String {
    value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value)
        .replace("\"\"", "\"")
}

fn act_start(time: f64, person: &str, act_type: &str, link: &str) -> String {
    format!(
        r#"<event time="{time}" type="actstart" person="{person}" link="{link}" actType="{act_type}" x="0.0" y="0.0"/>"#
    )
}

fn act_end(time: f64, person: &str, act_type: &str, link: &str) -> String {
    format!(
        r#"<event time="{time}" type="actend" person="{person}" link="{link}" actType="{act_type}" x="0.0" y="0.0"/>"#
    )
}

fn departure(time: f64, person: &str, mode: &str, link: &str) -> String {
    format!(
        r#"<event time="{time}" type="departure" person="{person}" link="{link}" legMode="{mode}" computationalRoutingMode="{mode}"/>"#
    )
}

fn arrival(time: f64, person: &str, mode: &str, link: &str) -> String {
    format!(
        r#"<event time="{time}" type="arrival" person="{person}" link="{link}" legMode="{mode}"/>"#
    )
}

fn stuck(time: f64, person: &str) -> String {
    format!(r#"<event time="{time}" type="stuckAndAbort" person="{person}"/>"#)
}

fn activity(kind: &str, link: &str) -> InternalPlanElement {
    InternalPlanElement::Activity(InternalActivity::new(
        None,
        kind,
        Id::create(link),
        None,
        None,
        None,
    ))
}

fn leg(mode: &str, from: &str, to: &str) -> InternalPlanElement {
    InternalPlanElement::Leg(InternalLeg {
        mode: Id::create(mode),
        routing_mode: None,
        dep_time: None,
        trav_time: None,
        route: Some(InternalRoute::Generic(InternalGenericRoute::new(
            Id::create(from),
            Id::create(to),
            None,
            Some(2000.0),
            None,
        ))),
        attributes: InternalAttributes::default(),
    })
}

/// A `home -> work -> home` day: three substantive activities and two journeys.
fn day_plan(home: &str, work: &str) -> InternalPlan {
    InternalPlan {
        score: None,
        selected: true,
        elements: vec![
            activity("home", home),
            leg("car", home, work),
            activity("work", work),
            leg("car", work, home),
            activity("home", home),
        ],
    }
}

fn person(person_id: &str, home: &str, work: &str) -> InternalPerson {
    InternalPerson::new(Id::create(person_id), day_plan(home, work))
}

/// The three links, each with the urban area the report should classify it as. An empty area
/// means the run supplies no label, so the link stays unclassified.
const LINKS: [(&str, &str); 3] = [
    ("home-link", "inner"),
    ("work-link", "outer"),
    ("far-link", ""),
];

/// A two-node network holding the given links.
fn network_with_links(links: &[(&str, &str)]) -> Network {
    let from = Node::new(Id::create("from"), Coordinate::new_2d(0.0, 0.0), 0, 1);
    let to = Node::new(Id::create("to"), Coordinate::new_2d(1.0, 0.0), 0, 1);
    let mut network = Network::new();
    network.add_node(from.clone());
    network.add_node(to.clone());
    for (link, _) in links {
        network.add_link(Link::new_with_default(Id::create(link), &from, &to));
    }
    network
}

/// Run the analysis over the given recorded event XML and plan.
fn analyze(events: &str, population: Population, zone_system: ZoneSystem) -> PatternReport {
    analyze_into(
        TempDir::new().unwrap(),
        events,
        population,
        zone_system,
        0,
        86400,
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

/// As [`analyze`], but with an explicit recording-window start.
fn analyze_from(
    events: &str,
    population: Population,
    zone_system: ZoneSystem,
    start_time: u32,
) -> PatternReport {
    analyze_into(
        TempDir::new().unwrap(),
        events,
        population,
        zone_system,
        start_time,
        86400,
    )
    .unwrap_or_else(|error| panic!("{error}"))
}

/// Run the analysis with an explicit end time too, so an invalid window can be exercised.
fn analyze_reporting_failure(
    events: &str,
    population: Population,
    zone_system: ZoneSystem,
    start_time: u32,
) -> String {
    analyze_into(
        TempDir::new().unwrap(),
        events,
        population,
        zone_system,
        start_time,
        86400,
    )
    .err()
    .map(|error| error.to_string())
    .unwrap_or_else(|| "the analysis accepted an invalid window".to_owned())
}

fn analyze_into(
    output_dir: TempDir,
    events: &str,
    population: Population,
    zone_system: ZoneSystem,
    start_time: u32,
    simulation_end_time: u32,
) -> Result<PatternReport, String> {
    let events_dir = output_dir.path().join("ITERS/it.0/events");
    fs::create_dir_all(&events_dir).unwrap();
    fs::write(events_dir.join("events.0.xml"), events).unwrap();
    let network = network_with_links(&LINKS);
    let metadata = AnalysisRunMetadata::from_run(
        1,
        1.0,
        start_time,
        &Garage::default(),
        capture_expected_travel(&population),
        AnalysisInputPaths::default(),
    );
    let report = analyze_final_iteration(
        output_dir.path(),
        0,
        1,
        CompressionType::None,
        simulation_end_time,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
            link_labels: LINKS
                .iter()
                // An empty area supplies no label, so the report has to classify the link
                // itself rather than inherit a label from the fixture.
                .filter(|(_, area)| !area.is_empty())
                .map(|(link, area)| {
                    (
                        (*link).to_owned(),
                        LinkLabels {
                            urban_area: Some((*area).to_owned()),
                            ..Default::default()
                        },
                    )
                })
                .collect(),
            zone_system,
            ..Analysis::default()
        },
    )
    .map_err(|error| error.to_string())?;
    Ok(PatternReport {
        dir: report.parent().unwrap().to_path_buf(),
        _output: output_dir,
    })
}

fn zone_system(
    name: &str,
    link_zones: &[(&str, &str)],
    person_zones: &[(&str, &str)],
) -> ZoneSystem {
    ZoneSystem {
        name: Some(name.to_owned()),
        link_zones: link_zones
            .iter()
            .map(|(link, zone)| ((*link).to_owned(), (*zone).to_owned()))
            .collect(),
        person_zones: person_zones
            .iter()
            .map(|(person, zone)| ((*person).to_owned(), (*zone).to_owned()))
            .collect(),
    }
}

/// The shared four-person day: two commuters crossing the same zone boundary in opposite
/// directions, one traveller who gets stuck, and one person the run never observed.
///
/// The run is simulated for a full day and shut down while three of the four people are still
/// at their destination, so every day here ends with a right-censored activity.
fn events_and_population() -> (String, Population) {
    let events = [
        // Commutes home -> work at 08:00, works, and commutes back at 10:00.
        act_start(0.0, "commuter", "home", "home-link"),
        act_end(28800.0, "commuter", "home", "home-link"),
        departure(28800.0, "commuter", "car", "home-link"),
        arrival(30600.0, "commuter", "car", "work-link"),
        act_start(30600.0, "commuter", "work", "work-link"),
        act_end(36000.0, "commuter", "work", "work-link"),
        departure(36000.0, "commuter", "car", "work-link"),
        arrival(37800.0, "commuter", "car", "home-link"),
        act_start(37800.0, "commuter", "home", "home-link"),
        // Works the other direction: the same boundary, from the other side.
        act_start(0.0, "returner", "home", "work-link"),
        act_end(18000.0, "returner", "home", "work-link"),
        departure(18000.0, "returner", "car", "work-link"),
        arrival(19800.0, "returner", "car", "home-link"),
        act_start(19800.0, "returner", "work", "home-link"),
        // Gets stuck on the way to work, so the day never covers the plan.
        act_start(0.0, "truncated", "home", "home-link"),
        act_end(3600.0, "truncated", "home", "home-link"),
        departure(3600.0, "truncated", "car", "home-link"),
        stuck(7200.0, "truncated"),
    ]
    .join("\n");
    let population = Population::from_persons(vec![
        person("commuter", "home-link", "work-link"),
        person("returner", "work-link", "home-link"),
        person("truncated", "home-link", "work-link"),
        person("absent", "home-link", "work-link"),
    ]);
    (format!("<events>{events}</events>"), population)
}

/// The zone system most tests use: two of three links mapped, and two of four people.
fn partial_zone_system() -> ZoneSystem {
    zone_system(
        "berlin-2018",
        &[("home-link", "zone-a"), ("work-link", "zone-b")],
        &[("commuter", "zone-a"), ("returner", "zone-b")],
    )
}

/// The censoring window opens where the run's recording does, not where the day does.
///
/// An activity starting exactly at the window start was in progress before it, so its
/// duration is a lower bound. Move the window start later and the same activity is measured
/// in full, which is the difference a hardcoded zero would have silently got wrong.
#[deterministic_id_test(rust_qsim)]
fn censoring_follows_the_recorded_window_start_not_the_day_start() {
    let events = [
        act_start(3600.0, "p", "home", "home-link"),
        act_end(28800.0, "p", "home", "home-link"),
        departure(28800.0, "p", "car", "home-link"),
        arrival(30600.0, "p", "car", "work-link"),
        act_start(30600.0, "p", "work", "work-link"),
    ]
    .join("\n");
    let population = Population::from_persons(vec![person("p", "home-link", "work-link")]);

    // A run whose recording opens an hour into the day: the first observed activity starts at
    // the window edge, so it is left-censored just as one starting at zero would be.
    let late = analyze_from(
        &format!("<events>{events}</events>"),
        Population::from_persons(vec![person("p", "home-link", "work-link")]),
        ZoneSystem::default(),
        3600,
    );
    let censored = late.row("activity_durations.csv", "person_id", "p");
    assert_eq!(censored["start_seconds"], "3600.000000");
    assert_eq!(censored["start_censored"], "true");
    assert_eq!(censored["duration_seconds"], "");
    // Its in-window share is still known, which is what the reconciliation uses.
    assert_eq!(censored["in_window_seconds"], "25200.000000");

    // The same recording with a window that opens before the activity is fully measured.
    let early = analyze(
        &format!("<events>{events}</events>"),
        population,
        ZoneSystem::default(),
    );
    let measured = early.row("activity_durations.csv", "person_id", "p");
    assert_eq!(measured["start_censored"], "false");
    assert_eq!(measured["duration_seconds"], "25200.000000");

    // An inverted window cannot describe a day, so it is refused rather than reported as a
    // day with no censoring at all.
    let error = analyze_reporting_failure(
        &format!("<events>{events}</events>"),
        Population::from_persons(vec![person("p", "home-link", "work-link")]),
        ZoneSystem::default(),
        90000,
    );
    assert!(
        error.to_string().contains("after the end time"),
        "an inverted window was not refused: {error}"
    );
}

/// A transit transfer that happened must not stand in for a destination that did not.
///
/// The plan counts substantive activities, so a `home, walk, pt interaction, pt, work` plan has
/// two. The observed stream records the transfer wait as an activity too, so a day that skipped
/// the journey entirely still produced two `actstart` events. Comparing the raw counts would
/// report that day `complete` when its one journey never happened.
#[deterministic_id_test(rust_qsim)]
fn a_stage_activity_never_counts_towards_a_reached_destination() {
    let events = [
        // The transit interaction was performed...
        act_start(0.0, "p", "home", "home-link"),
        act_end(600.0, "p", "home", "home-link"),
        departure(600.0, "p", "walk", "home-link"),
        arrival(1200.0, "p", "walk", "walk-link"),
        act_start(1200.0, "p", "pt interaction", "walk-link"),
        act_end(2400.0, "p", "pt interaction", "walk-link"),
        // ...and the work activity it was a step towards was never reached.
    ]
    .join("\n");
    // The plan is `home, walk, pt interaction, pt, work`: three activities, of which the
    // transit interaction is a stage activity, so the plan counts two destinations.
    let population = Population::from_persons(vec![InternalPerson::new(
        Id::create("p"),
        InternalPlan {
            score: None,
            selected: true,
            elements: vec![
                activity("home", "home-link"),
                leg("walk", "home-link", "walk-link"),
                activity("pt interaction", "walk-link"),
                leg("pt", "walk-link", "work-link"),
                activity("work", "work-link"),
            ],
        },
    )]);

    let report = analyze(
        &format!("<events>{events}</events>"),
        population,
        ZoneSystem::default(),
    );

    let pattern = report.row("activity_patterns.csv", "person_id", "p");
    // The plan's two substantive activities, and one of them observed.
    assert_eq!(pattern["planned_activities"], "2");
    assert_eq!(pattern["observed_activities"], "1");
    assert_eq!(pattern["status"], "truncated");
    // Two `actstart` events were recorded, so comparing raw counts would have read this day as
    // two of two and reported it complete.
    let recorded = rows(&report.read("activity_durations.csv"))
        .iter()
        .filter(|row| row["person_id"] == "p")
        .count();
    assert_eq!(
        recorded, 2,
        "the fixture must record a stage activity to be a regression"
    );

    // The transfer wait is still recorded, so the day reconciles against every observed
    // interval, and it is flagged so a reader can exclude it.
    let durations = rows(&report.read("activity_durations.csv"));
    let transfer = durations
        .iter()
        .find(|row| row["act_type"] == "pt interaction")
        .expect("the transfer wait is recorded");
    assert_eq!(transfer["stage"], "true");
    assert_eq!(transfer["duration_seconds"], "1200.000000");
    assert_eq!(durations[0]["stage"], "false");
}

/// A day that tiles the observation window: in-window activity time plus travel time equals
/// the observed span exactly, with the first activity's pre-window time left uncounted.
#[deterministic_id_test(rust_qsim)]
fn a_complete_day_reconciles_activity_and_travel_time() {
    let events = [
        act_start(0.0, "p", "home", "home-link"),
        act_end(28800.0, "p", "home", "home-link"),
        departure(28800.0, "p", "car", "home-link"),
        arrival(30600.0, "p", "car", "work-link"),
        act_start(30600.0, "p", "work", "work-link"),
        act_end(36000.0, "p", "work", "work-link"),
        departure(36000.0, "p", "car", "work-link"),
        arrival(37800.0, "p", "car", "home-link"),
        act_start(37800.0, "p", "home", "home-link"),
    ]
    .join("\n");
    let population = Population::from_persons(vec![person("p", "home-link", "work-link")]);

    let report = analyze(
        &format!("<events>{events}</events>"),
        population,
        ZoneSystem::default(),
    );

    let pattern = report.row("activity_patterns.csv", "person_id", "p");
    assert_eq!(pattern["status"], "complete");
    assert_eq!(pattern["activity_chain"], "home|work|home");
    assert_eq!(pattern["mode_chain"], "car|car");
    assert_eq!(pattern["journey_mode_chain"], "car|car");
    assert_eq!(pattern["planned_activities"], "3");
    assert_eq!(pattern["observed_activities"], "3");
    assert_eq!(pattern["planned_journeys"], "2");
    assert_eq!(pattern["observed_journeys"], "2");
    assert_eq!(pattern["completed_journeys"], "2");
    // The first activity began before the window opened and the last one never ended.
    assert_eq!(pattern["left_censored_activities"], "1");
    assert_eq!(pattern["right_censored_activities"], "1");
    // 28800 + 5400 in-window activity seconds and 1800 + 1800 travel seconds tile the 37800
    // seconds between the first observed moment and the last. The commuting home at 37800
    // contributes no activity time because the run shut down before it ended.
    assert_eq!(pattern["activity_seconds"], "34200.000000");
    assert_eq!(pattern["travel_seconds"], "3600.000000");
    assert_eq!(pattern["observed_span_seconds"], "37800.000000");
    assert_eq!(pattern["timeline_gap_seconds"], "0.000000");
}

/// The first and last activity of a day are censored explicitly rather than folded into a
/// duration as if the window edge were a real start or end.
#[deterministic_id_test(rust_qsim)]
fn first_and_last_day_activities_report_censoring_explicitly() {
    let (events, population) = events_and_population();
    let report = analyze(&events, population, partial_zone_system());

    let durations = rows(&report.read("activity_durations.csv"));
    let home = durations
        .iter()
        .find(|row| row["person_id"] == "commuter" && row["act_type"] == "home")
        .expect("the commuter performed a home activity");
    // The start was observed, but at the window edge, so the total duration is only a bound.
    assert_eq!(home["start_seconds"], "0.000000");
    assert_eq!(home["end_seconds"], "28800.000000");
    assert_eq!(home["duration_seconds"], "");
    assert_eq!(home["in_window_seconds"], "28800.000000");
    assert_eq!(home["start_censored"], "true");
    assert_eq!(home["end_censored"], "false");
    assert_eq!(home["zone"], "zone-a");
    assert_eq!(home["urban_area"], "inner");
    assert_eq!(home["link_id"], "home-link");

    let work = durations
        .iter()
        .find(|row| row["person_id"] == "commuter" && row["act_type"] == "work")
        .expect("the commuter performed a work activity");
    // Both ends were observed and neither sits on the window edge, so this one has a duration.
    assert_eq!(work["duration_seconds"], "5400.000000");
    assert_eq!(work["in_window_seconds"], "5400.000000");
    assert_eq!(work["start_censored"], "false");
    assert_eq!(work["end_censored"], "false");
    assert_eq!(work["zone"], "zone-b");

    let evening = durations
        .iter()
        .find(|row| row["person_id"] == "commuter" && row["start_seconds"] == "37800.000000")
        .expect("the commuter came home");
    // The run shut down mid-activity, so neither the total nor the in-window share is known.
    assert_eq!(evening["end_seconds"], "");
    assert_eq!(evening["duration_seconds"], "");
    assert_eq!(evening["in_window_seconds"], "");
    assert_eq!(evening["end_censored"], "true");

    // A censored interval never reaches a duration mean. No home activity was observed in
    // full, so home has no mean at all; of the two work activities only the commuter's ended
    // inside the window, so work has exactly one observation.
    let types = report.read("activity_type_summary.csv");
    assert!(types.contains("\"home\",4,3,0,3,1,,"), "{types}");
    assert!(
        types.contains("\"work\",2,2,1,0,1,5400.000000,5400.000000,5400.000000"),
        "{types}"
    );
}

/// An incomplete day is reported as such, and a person the run never observed is not folded
/// into any cohort.
#[deterministic_id_test(rust_qsim)]
fn incomplete_and_unobserved_patterns_are_reported_separately() {
    let (events, population) = events_and_population();
    let report = analyze(&events, population, ZoneSystem::default());

    let truncated = report.row("activity_patterns.csv", "person_id", "truncated");
    // Getting stuck outranks the truncation it caused, so the reason is actionable.
    assert_eq!(truncated["status"], "stuck");
    // The plan holds three activities and only the first was reached.
    assert_eq!(truncated["planned_activities"], "3");
    assert_eq!(truncated["observed_activities"], "1");
    assert_eq!(truncated["planned_journeys"], "2");
    assert_eq!(truncated["observed_journeys"], "1");
    assert_eq!(truncated["completed_journeys"], "0");
    assert_eq!(truncated["activity_chain"], "home");
    // The stuck event is an observed moment, so the day runs to it: the hour from the 01:00
    // departure to the 02:00 abort is recorded but belongs to neither a completed leg nor an
    // activity, and the timeline gap is where it is reported rather than lost from the day.
    assert_eq!(truncated["observed_span_seconds"], "7200.000000");
    assert_eq!(truncated["activity_seconds"], "3600.000000");
    assert_eq!(truncated["travel_seconds"], "0.000000");
    assert_eq!(truncated["timeline_gap_seconds"], "3600.000000");

    let absent = report.row("activity_patterns.csv", "person_id", "absent");
    assert_eq!(absent["status"], "not_observed");
    assert_eq!(absent["observed_activities"], "0");
    assert_eq!(absent["planned_activities"], "3");
    assert_eq!(absent["activity_chain"], "");
    assert_eq!(absent["mode_chain"], "");
    assert_eq!(absent["activity_seconds"], "0.000000");
    // Nothing was observed, so there is no span to reconcile against.
    assert_eq!(absent["observed_span_seconds"], "");
    assert_eq!(absent["timeline_gap_seconds"], "");

    // The returner planned three activities and observed two: the run shut down before the
    // evening trip home happened, so its day is truncated rather than complete.
    let returner = report.row("activity_patterns.csv", "person_id", "returner");
    assert_eq!(returner["status"], "truncated");
    assert_eq!(returner["planned_activities"], "3");
    assert_eq!(returner["observed_activities"], "2");

    // Every one of the four days is classified, and the four statuses are disjoint: a person
    // appears under exactly one, so the status cohorts partition the population.
    let summary = report.read("activity_pattern_summary.csv");
    let mut classified = 0;
    for status in ["not_observed", "stuck", "truncated", "complete"] {
        let row = report.row("activity_pattern_summary.csv", "category", status);
        assert_eq!(row["group"], "status");
        assert_eq!(
            row["persons"], "1",
            "unexpected {status} cohort:\n{summary}"
        );
        classified += 1;
    }
    let everyone = report.row("activity_pattern_summary.csv", "category", "all_persons");
    assert_eq!(everyone["group"], "all");
    assert_eq!(everyone["persons"], classified.to_string());
}

/// A supplied zone system produces mode and time keyed OD cells, and one boundary name is
/// shared by both directions of a crossing.
#[deterministic_id_test(rust_qsim)]
fn zone_system_yields_mode_and_time_od_matrices_and_boundary_flows() {
    let (events, population) = events_and_population();
    let report = analyze(&events, population, partial_zone_system());

    // Each journey is keyed by the interval it departed in, so the two directions of the
    // boundary land in different cells.
    let od = report.read("zone_od.csv");
    assert!(
        od.contains("28800,\"car\",\"zone-a\",\"zone-b\",true,1,1"),
        "{od}"
    );
    assert!(
        od.contains("18000,\"car\",\"zone-b\",\"zone-a\",true,1,1"),
        "{od}"
    );
    // The stuck traveller departed too, so the matrix accounts for it even though the day
    // never finished, and so does the commuter's trip home.
    assert!(
        od.contains("3600,\"car\",\"zone-a\",\"zone-b\",true,1,1"),
        "{od}"
    );
    assert!(
        od.contains("36000,\"car\",\"zone-b\",\"zone-a\",true,1,1"),
        "{od}"
    );
    // Four of the eight planned journeys departed and each gets its own cell; a journey that
    // never departed has no interval, so it is not placed in one.
    assert_eq!(od.lines().count(), 5, "{od}");

    // All four crossings share one boundary name whatever direction and interval they run in,
    // so the flow table is a boundary report rather than a second copy of the matrix.
    let flows = report.read("zone_flows.csv");
    for (from, to, hour) in [
        ("zone-a", "zone-b", 3600),
        ("zone-b", "zone-a", 18000),
        ("zone-a", "zone-b", 28800),
        ("zone-b", "zone-a", 36000),
    ] {
        assert!(
            flows.contains(&format!(
                "\"zone-a|zone-b\",\"{from}\",\"{to}\",\"car\",{hour},1,1"
            )),
            "the crossing at {hour} is not on the shared boundary:\n{flows}"
        );
    }

    // Every planned journey counts towards a zone, including the four that never departed,
    // so the zone totals reconcile with journeys.csv rather than only with the matrix.
    let summary = report.row("zone_summary.csv", "zone", "zone-a");
    assert_eq!(summary["links"], "1");
    assert_eq!(summary["resident_persons"], "1");
    assert_eq!(summary["observing_persons"], "3");
    assert_eq!(summary["journeys_origin"], "4");
    assert_eq!(summary["journeys_destination"], "4");
    assert_eq!(summary["activities"], "4");
    assert_eq!(summary["in_window_activity_seconds"], "32400.000000");
    assert_eq!(summary["left_censored_activities"], "2");
    assert_eq!(summary["right_censored_activities"], "2");
    // Both people the person geography does not cover are counted as unmapped rather than
    // dropped, including the one that was never observed at all.
    let unmapped = report.row("zone_summary.csv", "zone", "unmapped");
    assert_eq!(unmapped["unmapped_persons"], "2");
    assert_eq!(unmapped["links"], "1");
}

/// A link and a person the zone system does not cover stay in the matrices and the summaries,
/// and an unclassified link is not attributed to an area it does not belong to.
#[deterministic_id_test(rust_qsim)]
fn unmapped_locations_are_preserved_rather_than_dropped() {
    let (events, population) = events_and_population();
    let report = analyze(
        &events,
        population,
        // Only one of the three links is mapped, and one of four people.
        zone_system(
            "partial",
            &[("home-link", "zone-a")],
            &[("commuter", "zone-a")],
        ),
    );

    assert_eq!(
        report.row("activity_patterns.csv", "person_id", "commuter")["person_zone"],
        "zone-a"
    );
    assert_eq!(
        report.row("activity_patterns.csv", "person_id", "returner")["person_zone"],
        "unmapped"
    );
    assert_eq!(
        report.row("activity_patterns.csv", "person_id", "absent")["person_zone"],
        "unmapped"
    );
    // Every journey the run observed departing is accounted for somewhere in the matrix.
    let journeys: u64 = rows(&report.read("zone_od.csv"))
        .iter()
        .map(|row| row["journeys"].parse::<u64>().unwrap())
        .sum();
    assert_eq!(journeys, 4);
    assert_daily_totals_reconcile(&report);
    let summary = report.read("zone_summary.csv");
    assert!(summary.contains("\"unmapped\","), "{summary}");
    assert!(summary.contains("\"zone-a\","), "{summary}");

    // A supplied zone system is the report's definition of an urban area, so the summary is
    // keyed by it and the person geography supplies the residents. The link the system does not
    // cover is `unmapped`, and so are the three people it places nowhere.
    let urban = report.read("urban_area_summary.csv");
    assert!(urban.contains("zone_system"), "{urban}");
    let zone_a = report.row("urban_area_summary.csv", "urban_area", "zone-a");
    assert_eq!(zone_a["geography"], "zone_system");
    assert_eq!(zone_a["links"], "1");
    assert_eq!(zone_a["residents"], "1");
    assert_eq!(zone_a["unmapped_residents"], "0");
    assert_eq!(zone_a["activities"], "4");
    assert_eq!(zone_a["journeys_origin"], "4");
    let unmapped_area = report.row("urban_area_summary.csv", "urban_area", "unmapped");
    assert_eq!(unmapped_area["geography"], "zone_system");
    assert_eq!(unmapped_area["links"], "2");
    assert_eq!(unmapped_area["unmapped_residents"], "3");
    // No row is attributed to a link-classification area, because a zone system was supplied.
    assert!(!urban.contains("link_classification"), "{urban}");

    // Without a zone system the summary falls back to the link classification, and the
    // geography column says so rather than leaving the key ambiguous. An activity on the link
    // the run does not label is counted as unclassified instead of joining a labelled area.
    let classified = analyze(
        &format!("<events>{events}</events>"),
        Population::from_persons(vec![person("commuter", "home-link", "work-link")]),
        ZoneSystem::default(),
    );
    let by_area = classified.read("urban_area_summary.csv");
    assert!(by_area.contains("link_classification"), "{by_area}");
    // Every area's activity count is the number of recorded intervals the activity table places
    // in it, so the summary reconciles with the intervals it summarises.
    let intervals = rows(&classified.read("activity_durations.csv"));
    for area in ["inner", "outer", "unknown"] {
        let expected = intervals
            .iter()
            .filter(|row| row["urban_area"] == area)
            .count();
        let row = classified.row("urban_area_summary.csv", "urban_area", area);
        assert_eq!(row["geography"], "link_classification");
        assert_eq!(row["activities"], expected.to_string(), "{area}: {by_area}");
    }
    // The unlabelled link is a row of its own rather than being dropped or merged into a
    // labelled area, and `outer` exists even though nothing was observed on it.
    assert_eq!(
        classified.row("urban_area_summary.csv", "urban_area", "unknown")["links"],
        "1"
    );
    assert_eq!(
        classified.row("urban_area_summary.csv", "urban_area", "outer")["links"],
        "1"
    );
}

/// Every new metric has a table, a presentation in the local report, and a module status.
#[deterministic_id_test(rust_qsim)]
fn pattern_and_zone_metrics_are_exported_presented_and_statused() {
    let (events, population) = events_and_population();
    let report = analyze(&events, population, partial_zone_system());

    let html = fs::read_to_string(report.dir.join("index.html")).unwrap();
    for (table, section) in [
        (
            "activity_patterns.csv",
            "Per-person activity chains and mode chains",
        ),
        (
            "activity_durations.csv",
            "Observed activity intervals and censoring",
        ),
        ("activity_type_summary.csv", "Activity type totals"),
        (
            "activity_pattern_summary.csv",
            "Pattern totals by status and person zone",
        ),
        ("zone_od.csv", "Zone OD by interval and mode"),
        ("zone_flows.csv", "Zone boundary crossings"),
        ("zone_summary.csv", "Zone totals"),
        ("urban_area_summary.csv", "Urban-area summary"),
    ] {
        assert!(report.dir.join(table).is_file(), "{table} was not exported");
        assert!(
            html.contains(section),
            "the report has no {section} section"
        );
        assert!(html.contains(table), "the report does not link {table}");
    }
    // The rows themselves are embedded, so the metrics are readable and not only exported.
    assert!(html.contains("person_id,person_zone,status"), "{html}");
    assert!(html.contains("origin_zone,destination_zone"), "{html}");
    // The report says which zone system produced the zones, and what censoring means.
    assert!(html.contains("Zone system: berlin-2018"), "{html}");
    assert!(html.contains("left-censored"), "{html}");
    assert!(html.contains("right-censored"), "{html}");

    let statuses: serde_json::Value =
        serde_json::from_str(&report.read("module_status.json")).unwrap();
    let status = |module: &str| {
        statuses
            .as_array()
            .expect("module status is an array")
            .iter()
            .find(|status| status["module"] == module)
            .unwrap_or_else(|| panic!("{module} module status is not reported"))
            .clone()
    };
    for module in ["activity_patterns", "urban_areas", "zones"] {
        assert_eq!(status(module)["status"], "complete", "{module}");
    }
    assert_eq!(status("zones")["reason"], "Zone system berlin-2018");
}

/// A run that supplies no zone system leaves the geographic module unavailable and publishes
/// header-only zone tables rather than one fabricated unmapped zone.
#[deterministic_id_test(rust_qsim)]
fn a_run_without_a_zone_system_reports_the_module_unavailable() {
    let (events, population) = events_and_population();
    let report = analyze(&events, population, ZoneSystem::default());

    let od = report.read("zone_od.csv");
    assert_eq!(
        od.lines().count(),
        1,
        "an unconfigured zone system wrote rows: {od}"
    );
    assert!(od.starts_with("departure_hour_seconds,mode,origin_zone"));
    for table in ["zone_flows.csv", "zone_summary.csv"] {
        assert_eq!(
            report.read(table).lines().count(),
            1,
            "{table} has rows without a zone system"
        );
    }
    // The urban-area summary needs no zone system, so it still reports.
    assert!(report.read("urban_area_summary.csv").lines().count() > 1);
    // The activity tables carry a zone for every location regardless, so nothing is lost.
    assert!(
        report
            .read("activity_durations.csv")
            .contains(",\"unmapped\",")
    );

    let statuses: serde_json::Value =
        serde_json::from_str(&report.read("module_status.json")).unwrap();
    let zones = statuses
        .as_array()
        .unwrap()
        .iter()
        .find(|status| status["module"] == "zones")
        .expect("the zones module is reported");
    assert_eq!(zones["status"], "unavailable");
    assert_eq!(zones["reason"], "No zone system is configured");
}

/// The new tables' totals describe the same day as the tables that were already published.
///
/// Daily totals are only trustworthy if they reconcile against `journeys.csv` and `legs.csv`,
/// which the activity-pattern and zone tables are derived from rather than merely resemble.
fn assert_daily_totals_reconcile(report: &PatternReport) {
    let journeys = rows(&report.read("journeys.csv"));
    let patterns = rows(&report.read("activity_patterns.csv"));
    let cohort = report.row("activity_pattern_summary.csv", "category", "all_persons");
    let zone_rows = rows(&report.read("zone_summary.csv"));

    // One pattern row per person, and the cohort counts exactly those rows.
    assert_eq!(cohort["group"], "all");
    assert_eq!(cohort["persons"], patterns.len().to_string());

    // The cohort's journey count is the journeys that departed, which is exactly the journeys
    // the journey table marks with a departure second.
    let departed = journeys
        .iter()
        .filter(|journey| !journey["departure_seconds"].is_empty())
        .count();
    assert_eq!(cohort["journeys"].parse::<usize>().unwrap(), departed);

    // Travel time is summed from the leg table it shares its rows with, so the two agree.
    let completed_leg_seconds: u64 = rows(&report.read("legs.csv"))
        .iter()
        .filter_map(|leg| leg["duration_seconds"].parse::<f64>().ok())
        .map(|seconds| seconds.round() as u64)
        .sum();
    let cohort_travel = cohort["travel_seconds"].parse::<f64>().unwrap().round() as u64;
    assert_eq!(cohort_travel, completed_leg_seconds);

    // Activity time is the cohort's own in-window total.
    let pattern_activity: f64 = patterns
        .iter()
        .map(|row| row["activity_seconds"].parse::<f64>().unwrap())
        .sum();
    assert_eq!(
        cohort["activity_seconds"].parse::<f64>().unwrap(),
        pattern_activity
    );

    // The zone rows partition the same journeys: each contributes one origin and one
    // destination, so both sides total the journey table while the matrix holds only the
    // journeys that departed.
    let origins: u64 = zone_rows
        .iter()
        .map(|zone| zone["journeys_origin"].parse::<u64>().unwrap())
        .sum();
    let destinations: u64 = zone_rows
        .iter()
        .map(|zone| zone["journeys_destination"].parse::<u64>().unwrap())
        .sum();
    assert_eq!(origins as usize, journeys.len());
    assert_eq!(destinations as usize, journeys.len());
}

/// The zone system survives into the manifest, so a standalone rerun rebuilds the same
/// geographic report.
#[deterministic_id_test(rust_qsim)]
fn the_manifest_records_the_supplied_zone_system() {
    let (events, population) = events_and_population();
    let report = analyze(&events, population, partial_zone_system());

    let manifest: serde_json::Value = serde_json::from_str(&report.read("manifest.json")).unwrap();
    assert_eq!(manifest["zone_system"]["name"], "berlin-2018");
    assert_eq!(manifest["zone_system"]["link_zones"]["home-link"], "zone-a");
    assert_eq!(
        manifest["zone_system"]["person_zones"]["commuter"],
        "zone-a"
    );
}
