use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration, capture_expected_travel,
    capture_person_demographics, reanalyze_completed_run,
};
use rust_qsim::simulation::config::{Analysis, CommandLineArgs, CompressionType, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
use rust_qsim::simulation::io::xml::population::{
    IOActivity, IOLeg, IOPerson, IOPlan, IOPlanElement,
};
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scenario::network::Network;
use rust_qsim::simulation::scenario::population::Population;
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::fs;
use std::path::{Path, PathBuf};

/// The demographic module's own tables and the section that presents them.
const MODULE: &str = "demographic_equity";

/// A person with the given attributes and a plan that departs once per mode.
///
/// The attributes go through the same XML conversion the population readers use, because a
/// person's attributes are only writable through the input model.
fn person(id: &str, attributes: &[(&str, &str, &str)], legs: &[&str]) -> IOPerson {
    let mut elements = vec![IOPlanElement::Activity(activity("home"))];
    for (index, mode) in legs.iter().enumerate() {
        elements.push(IOPlanElement::Leg(IOLeg {
            attributes: None,
            mode: (*mode).to_owned(),
            dep_time: None,
            trav_time: None,
            route: None,
        }));
        elements.push(IOPlanElement::Activity(activity(
            if index + 1 == legs.len() {
                "home"
            } else {
                "work"
            },
        )));
    }
    IOPerson {
        id: id.to_owned(),
        attributes: Some(IOAttributes {
            attributes: attributes
                .iter()
                .map(|(name, class, value)| {
                    IOAttribute::new_with_class(
                        (*name).to_owned(),
                        (*class).to_owned(),
                        (*value).to_owned(),
                    )
                })
                .collect(),
        }),
        plans: vec![IOPlan {
            attributes: None,
            selected: true,
            score: None,
            elements,
        }],
    }
}

fn activity(kind: &str) -> IOActivity {
    IOActivity {
        attributes: None,
        r#type: kind.to_owned(),
        link: Some("l".to_owned()),
        x: None,
        y: None,
        start_time: None,
        end_time: None,
        max_dur: None,
    }
}

fn settings() -> Analysis {
    Analysis {
        enabled: true,
        interval_seconds: 3600,
        person_group_attributes: vec!["income".to_owned(), "age".to_owned()],
        person_weight_attribute: Some("weight".to_owned()),
        person_cost_attribute: Some("cost".to_owned()),
        ..Analysis::default()
    }
}

/// The four people every test in this file shares: a complete car commuter with a weight and a
/// cost, a walker without either, a person whose leg never arrives, and a non-traveler with no
/// attributes at all.
fn population() -> Population {
    Population::from_persons(
        vec![
            person(
                "commuter",
                &[
                    ("income", "java.lang.String", "low"),
                    ("age", "java.lang.Integer", "30"),
                    ("weight", "java.lang.Double", "2.0"),
                    ("cost", "java.lang.Double", "4.0"),
                ],
                &["car", "car"],
            ),
            person(
                "walker",
                &[("income", "java.lang.String", "high")],
                &["walk"],
            ),
            person(
                "stranded",
                &[
                    ("income", "java.lang.String", "low"),
                    ("weight", "java.lang.Double", "0.5"),
                ],
                &["car"],
            ),
            person("homebody", &[], &[]),
        ]
        .into_iter()
        .map(Into::into)
        .collect(),
    )
}

/// Write one partition of person events and publish a report for the run they describe.
fn publish_run(
    output: &Path,
    events: &str,
    settings: &Analysis,
    population: &Population,
) -> PathBuf {
    let events_dir = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events_dir).unwrap();
    fs::write(events_dir.join("events.0.xml"), events).unwrap();
    let metadata = AnalysisRunMetadata::from_run(
        1,
        1.0,
        &Garage::default(),
        capture_expected_travel(population),
        AnalysisInputPaths::default(),
    )
    .with_person_demographics(capture_person_demographics(population, settings));
    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &metadata,
        &Network::new(),
        settings,
    )
    .unwrap();
    report
}

/// Events where every planned leg of the commuter and the walker arrives.
const COMPLETE_DAY: &str = r#"<events>
  <event time="100" type="departure" person="commuter" link="l" legMode="car" computationalRoutingMode="car" />
  <event time="400" type="arrival" person="commuter" link="l" legMode="car" />
  <event time="500" type="departure" person="commuter" link="l" legMode="car" computationalRoutingMode="car" />
  <event time="700" type="arrival" person="commuter" link="l" legMode="car" />
  <event time="10" type="departure" person="walker" link="l" legMode="walk" computationalRoutingMode="walk" />
  <event time="40" type="arrival" person="walker" link="l" legMode="walk" />
  <event time="10" type="departure" person="stranded" link="l" legMode="car" computationalRoutingMode="car" />
</events>"#;

/// The same population with a shorter car day, a longer walk and a person the run never simulated.
const IMPROVED_DAY: &str = r#"<events>
  <event time="100" type="departure" person="commuter" link="l" legMode="car" computationalRoutingMode="car" />
  <event time="200" type="arrival" person="commuter" link="l" legMode="car" />
  <event time="500" type="departure" person="commuter" link="l" legMode="car" computationalRoutingMode="car" />
  <event time="600" type="arrival" person="commuter" link="l" legMode="car" />
  <event time="10" type="departure" person="walker" link="l" legMode="walk" computationalRoutingMode="walk" />
  <event time="140" type="arrival" person="walker" link="l" legMode="walk" />
</events>"#;

/// The `group_burdens.csv` rows of one dimension, keyed by group.
fn burdens(report_dir: &Path, dimension: &str) -> Vec<Vec<String>> {
    let table = fs::read_to_string(report_dir.join("group_burdens.csv")).unwrap();
    let mut lines = table.lines();
    lines.next().unwrap();
    lines
        .map(|line| line.split(',').map(str::to_owned).collect())
        .filter(|row: &Vec<String>| row[0] == format!("\"{dimension}\""))
        .collect()
}

fn equity_row(report_dir: &Path, dimension: &str, group: &str) -> Vec<String> {
    let table = fs::read_to_string(report_dir.join("equity_comparison.csv")).unwrap();
    table
        .lines()
        .skip(1)
        .map(|line| line.split(',').map(str::to_owned).collect())
        .find(|row: &Vec<String>| {
            row[1] == format!("\"{dimension}\"") && row[2] == format!("\"{group}\"")
        })
        .unwrap_or_else(|| panic!("no equity row for {dimension}={group}\n{table}"))
}

fn module_status(report_dir: &Path) -> String {
    fs::read_to_string(report_dir.join("module_status.json")).unwrap()
}

#[deterministic_id_test(rust_qsim)]
fn group_burdens_retain_weights_missing_attributes_and_incomplete_persons() {
    let temp = tempfile::tempdir().unwrap();
    let report = publish_run(temp.path(), COMPLETE_DAY, &settings(), &population());
    let report_dir = report.parent().unwrap();

    // The low-income group holds the weighted commuter and the stranded person. Weights are
    // summed, the stranded person keeps its group size without contributing a burden, and the
    // person who supplied no attribute is reported as unknown rather than dropped.
    let income = burdens(report_dir, "income");
    assert_eq!(
        income,
        vec![
            vec![
                "\"income\"",
                "\"high\"",
                "1",
                "1.000000",
                "0.222222",
                "1",
                "1",
                "1",
                "0",
                "30.000000",
                "30.000000",
                "30.000000",
                "0",
                ""
            ],
            vec![
                "\"income\"",
                "\"low\"",
                "2",
                "2.500000",
                "0.555556",
                "0",
                "1",
                "1",
                "1",
                "500.000000",
                "500.000000",
                "500.000000",
                "1",
                "4.000000"
            ],
            vec![
                "\"income\"",
                "\"unknown\"",
                "1",
                "1.000000",
                "0.222222",
                "1",
                "0",
                "1",
                "0",
                "0.000000",
                "0.000000",
                "0.000000",
                "0",
                ""
            ],
        ]
    );
    // Every configured dimension is reported, including the one only the commuter answered.
    let age = burdens(report_dir, "age");
    assert_eq!(age.len(), 2);
    assert_eq!(
        age[0],
        vec![
            "\"age\"",
            "\"30\"",
            "1",
            "2.000000",
            "0.444444",
            "0",
            "1",
            "1",
            "0",
            "500.000000",
            "500.000000",
            "500.000000",
            "1",
            "4.000000"
        ]
    );
    assert_eq!(
        age[1],
        vec![
            "\"age\"",
            "\"unknown\"",
            "3",
            "2.500000",
            "0.555556",
            "2",
            "1",
            "2",
            "1",
            "15.000000",
            "30.000000",
            "30.000000",
            "0",
            ""
        ]
    );

    // A person without a usable weight is counted as one, and the table says which is which.
    let people = fs::read_to_string(report_dir.join("person_demographics.csv")).unwrap();
    assert!(people.contains("\"commuter\",\"income\",\"low\",2.000000,supplied,4.000000"));
    assert!(people.contains("\"walker\",\"income\",\"high\",1.000000,default,"));
    assert!(people.contains("\"homebody\",\"income\",\"unknown\",1.000000,default,"));
    assert!(people.contains("\"homebody\",\"age\",\"unknown\",1.000000,default,"));

    let status = module_status(report_dir);
    assert!(
        status.contains(&format!("\"module\": \"{MODULE}\"")),
        "{status}"
    );
    // The module is complete, and the two modules without configured input stay unavailable.
    assert_eq!(
        status.matches("\"status\": \"complete\"").count(),
        5,
        "{status}"
    );
    assert_eq!(
        status.matches("\"status\": \"unavailable\"").count(),
        3,
        "{status}"
    );

    // The report presents the tables and states the equity criterion next to them.
    let html = fs::read_to_string(&report).unwrap();
    assert!(html.contains("Demographic outcomes and equity"));
    assert!(html.contains("csvTable('#group-burdens'"));
    assert!(html.contains("csvTable('#person-demographics'"));
    assert!(html.contains("csvTable('#equity-comparison'"));
    assert!(
        html.contains("lower_daily_completed_travel_time"),
        "the criterion is stated"
    );
    assert!(html.contains("group_burdens.csv"));

    // The metric catalog describes the new columns.
    let catalog = fs::read_to_string(report_dir.join("metric_catalog.json")).unwrap();
    for metric in [
        "\"group_persons\"",
        "\"weighted_share\"",
        "\"comparable_persons\"",
        "\"winners\"",
        "\"losers\"",
        "\"not_comparable_persons\"",
    ] {
        assert!(
            catalog.contains(metric),
            "{metric} is missing from the catalog"
        );
    }
}

#[deterministic_id_test(rust_qsim)]
fn equity_comparison_counts_winners_and_losers_of_a_consistent_population() {
    let temp = tempfile::tempdir().unwrap();
    let baseline_dir = temp.path().join("baseline");
    let comparison_dir = temp.path().join("comparison");
    fs::create_dir_all(&baseline_dir).unwrap();
    fs::create_dir_all(&comparison_dir).unwrap();
    let comparison_settings = settings();
    publish_run(
        &comparison_dir,
        IMPROVED_DAY,
        &comparison_settings,
        &population(),
    );

    let mut baseline_settings = settings();
    baseline_settings.comparison_runs = vec![comparison_dir.clone()];
    let report = publish_run(
        &baseline_dir,
        COMPLETE_DAY,
        &baseline_settings,
        &population(),
    );
    let report_dir = report.parent().unwrap();

    // The commuter's shorter car day makes them a winner in the low-income group, the walker's
    // longer day makes them a loser in the high-income group, and the non-traveler is unchanged.
    let low = equity_row(report_dir, "income", "low");
    assert_eq!(
        low[3], "\"lower_daily_completed_travel_time\"",
        "the criterion travels with the row"
    );
    assert_eq!(low[4], "2", "both low-income persons are counted");
    assert_eq!(low[5], "2", "both are in the comparison run");
    assert_eq!(
        low[6], "1",
        "only the commuter has a completed day in both runs"
    );
    assert_eq!(low[7], "1", "the commuter is a winner");
    assert_eq!(low[8], "0");
    assert_eq!(low[9], "0");
    assert_eq!(low[12], "1", "the stranded person is not comparable");
    assert_eq!(low[13], "500.000000");
    assert_eq!(low[14], "200.000000");
    assert_eq!(low[15], "-300.000000");

    let high = equity_row(report_dir, "income", "high");
    assert_eq!(high[6], "1");
    assert_eq!(high[7], "0");
    assert_eq!(high[8], "0");
    assert_eq!(high[9], "1", "the walker's longer day is a loss");
    assert_eq!(high[13], "30.000000");
    assert_eq!(high[14], "130.000000");
    assert_eq!(high[15], "100.000000");

    // The whole population is reported as its own group, and the person the comparison run never
    // simulated is retained as a population difference rather than dropped.
    let all = equity_row(report_dir, "all", "all");
    assert_eq!(all[4], "4");
    assert_eq!(all[6], "3");
    assert_eq!(all[7], "1");
    assert_eq!(all[8], "1");
    assert_eq!(all[9], "1");
    assert_eq!(all[12], "1");
    assert!(all[0].contains("comparison"));

    let status = module_status(report_dir);
    assert!(status.contains(&format!("\"module\": \"{MODULE}\"")));
    assert!(!status.contains("\"failed\""));
}

#[deterministic_id_test(rust_qsim)]
fn equity_comparison_counts_people_in_their_group_on_each_side() {
    let temp = tempfile::tempdir().unwrap();
    let baseline_dir = temp.path().join("baseline");
    let comparison_dir = temp.path().join("comparison");
    fs::create_dir_all(&baseline_dir).unwrap();
    fs::create_dir_all(&comparison_dir).unwrap();

    let mut comparison_people = population().persons.into_values().collect::<Vec<_>>();
    let commuter = person(
        "commuter",
        &[
            ("income", "java.lang.String", "middle"),
            ("age", "java.lang.Integer", "30"),
            ("weight", "java.lang.Double", "2.0"),
            ("cost", "java.lang.Double", "4.0"),
        ],
        &["car", "car"],
    );
    // Replace the comparison run's commuter with an otherwise identical person in a new group.
    comparison_people.retain(|person| person.id().external() != "commuter");
    comparison_people.push(commuter.into());
    publish_run(
        &comparison_dir,
        IMPROVED_DAY,
        &settings(),
        &Population::from_persons(comparison_people),
    );

    let mut baseline_settings = settings();
    baseline_settings.comparison_runs = vec![comparison_dir];
    let report = publish_run(
        &baseline_dir,
        COMPLETE_DAY,
        &baseline_settings,
        &population(),
    );
    let report_dir = report.parent().unwrap();

    let low = equity_row(report_dir, "income", "low");
    assert_eq!(
        low[4], "2",
        "baseline group includes both low-income people"
    );
    assert_eq!(low[5], "1", "comparison group excludes the moved commuter");
    assert_eq!(
        low[12], "2",
        "the incomplete person and the commuter who changed groups are not comparable"
    );

    let middle = equity_row(report_dir, "income", "middle");
    assert_eq!(middle[4], "0", "the baseline has nobody in this group");
    assert_eq!(
        middle[5], "1",
        "comparison population includes the moved commuter"
    );
    assert_eq!(
        middle[12], "1",
        "the moved commuter is not comparable in this group"
    );
}

#[deterministic_id_test(rust_qsim)]
fn equity_comparison_rejects_a_run_that_grouped_differently() {
    let temp = tempfile::tempdir().unwrap();
    let baseline_dir = temp.path().join("baseline");
    let comparison_dir = temp.path().join("comparison");
    fs::create_dir_all(&baseline_dir).unwrap();
    fs::create_dir_all(&comparison_dir).unwrap();
    // The comparison run never grouped people, so its report holds no person groups.
    let ungrouped = Analysis {
        enabled: true,
        interval_seconds: 3600,
        ..Analysis::default()
    };
    publish_run(&comparison_dir, IMPROVED_DAY, &ungrouped, &population());

    let mut baseline_settings = settings();
    baseline_settings.comparison_runs = vec![comparison_dir.clone()];
    let report = publish_run(
        &baseline_dir,
        COMPLETE_DAY,
        &baseline_settings,
        &population(),
    );
    let report_dir = report.parent().unwrap();

    // Counting winners against a differently grouped run would describe two populations, so the
    // module fails with the reason and the rest of the report stands.
    let status = module_status(report_dir);
    assert!(status.contains(&format!("\"module\": \"{MODULE}\"")));
    assert!(status.contains("\"status\": \"failed\""), "{status}");
    assert!(
        status.contains("was not analyzed with the person group attribute"),
        "{status}"
    );
    assert!(report_dir.join("link_hourly.csv").is_file());
}

#[deterministic_id_test(rust_qsim)]
fn an_unconfigured_module_publishes_its_tables_as_empty() {
    let temp = tempfile::tempdir().unwrap();
    let ungrouped = Analysis {
        enabled: true,
        interval_seconds: 3600,
        ..Analysis::default()
    };
    let report = publish_run(temp.path(), COMPLETE_DAY, &ungrouped, &population());
    let report_dir = report.parent().unwrap();

    for table in [
        "person_demographics.csv",
        "group_burdens.csv",
        "group_module_outcomes.csv",
        "equity_comparison.csv",
    ] {
        let content = fs::read_to_string(report_dir.join(table)).unwrap();
        assert_eq!(content.lines().count(), 1, "{table} is not header-only");
    }
    let status = module_status(report_dir);
    assert!(
        status.contains("No person group attributes are configured"),
        "{status}"
    );
    let html = fs::read_to_string(&report).unwrap();
    assert!(html.contains("csvTable('#group-burdens'"));
}

#[deterministic_id_test(rust_qsim)]
fn a_controlled_run_groups_the_people_its_population_supplies() {
    // The controller is the only place the population still exists, so this covers the whole
    // path: the run reads the attributes, records them and groups the report by them.
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/3-links/3-links-config-1.yml",
    ));
    config.controller_mut().last_iteration = 0;
    config.output_mut().output_dir = "./test_output/simulation/demographic_controller".into();
    config.population_mut().path = Some("./tests/resources/3-links/1-agent-attributes.xml".into());
    config.output_mut().analysis = settings();
    let output = config.output().output_dir.clone();
    ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();
    let report_dir = output.join("analysis");

    let people = fs::read_to_string(report_dir.join("person_demographics.csv")).unwrap();
    assert!(
        people.contains("\"100\",\"income\",\"low\",2.500000,supplied,3.750000"),
        "{people}"
    );
    assert!(
        people.contains("\"100\",\"age\",\"42\",2.500000,supplied,3.750000"),
        "{people}"
    );
    let burdens = fs::read_to_string(report_dir.join("group_burdens.csv")).unwrap();
    assert!(
        burdens.contains("\"income\",\"low\",1,2.500000,1.000000"),
        "{burdens}"
    );
    let status = module_status(&report_dir);
    assert!(
        status.contains(&format!("\"module\": \"{MODULE}\"")),
        "{status}"
    );
    assert!(!status.contains("\"failed\""), "{status}");
}

#[deterministic_id_test(rust_qsim)]
fn a_standalone_rerun_reuses_the_recorded_grouping() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    // The rerun reads the eligible links from the recorded output network, so the run has to
    // record one.
    fs::write(
        output.join("output_network.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<network>
    <nodes>
        <node id="a" x="0.0" y="0.0" />
        <node id="b" x="1.0" y="0.0" />
    </nodes>
    <links>
        <link id="l" from="a" to="b" length="1" freespeed="10" capacity="3600.0" permlanes="1.0" oneway="1" modes="car" />
    </links>
</network>"#,
    )
    .unwrap();
    let events_dir = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events_dir).unwrap();
    fs::write(events_dir.join("events.0.xml"), COMPLETE_DAY).unwrap();
    let analysis = settings();
    let population = population();
    let metadata = AnalysisRunMetadata::from_run(
        1,
        1.0,
        &Garage::default(),
        capture_expected_travel(&population),
        AnalysisInputPaths {
            network_file: Some(Path::new("output_network.xml")),
            ..AnalysisInputPaths::default()
        },
    )
    .with_person_demographics(capture_person_demographics(&population, &analysis));
    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &metadata,
        &Network::new(),
        &analysis,
    )
    .unwrap();
    let report_dir = report.parent().unwrap();
    let burdens = fs::read_to_string(report_dir.join("group_burdens.csv")).unwrap();
    let people = fs::read_to_string(report_dir.join("person_demographics.csv")).unwrap();
    let manifest = fs::read_to_string(report_dir.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"person_group_attributes\""));
    assert!(manifest.contains("\"person_weight_attribute\": \"weight\""));
    assert!(manifest.contains("\"person_cost_attribute\": \"cost\""));
    let recorded_metadata = fs::read_to_string(report_dir.join("run_metadata.json")).unwrap();
    assert!(recorded_metadata.contains("\"person_demographics\""));
    assert!(recorded_metadata.contains("\"commuter\""));

    // The rerun rebuilds the report from the recorded grouping and the recorded attributes.
    reanalyze_completed_run(output, None).unwrap();
    assert_eq!(
        fs::read_to_string(report_dir.join("group_burdens.csv")).unwrap(),
        burdens
    );
    assert_eq!(
        fs::read_to_string(report_dir.join("person_demographics.csv")).unwrap(),
        people
    );
}
