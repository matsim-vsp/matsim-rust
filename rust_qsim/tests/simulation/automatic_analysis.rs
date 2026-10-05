use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration,
};
use rust_qsim::simulation::config::{Analysis, CommandLineArgs, CompressionType, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scenario::network::{Link, Network, Node};
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::fs;

#[deterministic_id_test(rust_qsim)]
fn final_iteration_report_exports_all_links_and_hourly_coverage() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let earlier_events = output.join("ITERS/it.6/events");
    fs::create_dir_all(&earlier_events).unwrap();
    fs::write(
        earlier_events.join("events.0.xml"),
        format!(
            "<events>{}</events>",
            "<event time=\"3600\" type=\"entered link\" link=\"used\" vehicle=\"earlier\" />"
                .repeat(100)
        ),
    )
    .unwrap();
    let events = output.join("ITERS/it.7/events");
    fs::create_dir_all(&events).unwrap();
    let link_id = |index| {
        if index == 0 {
            "used".to_owned()
        } else {
            format!("used-{index}")
        }
    };
    for (rank, selected) in [(0, [0, 2, 4].as_slice()), (1, [1, 3].as_slice())] {
        let rows = selected
            .iter()
            .map(|&index| {
                format!(
                    "<event time=\"3600\" type=\"entered link\" link=\"{}\" vehicle=\"veh-{index}\" />",
                    link_id(index)
                )
            })
            .chain(selected.iter().map(|&index| {
                format!(
                    "<event time=\"3700\" type=\"left link\" link=\"{}\" vehicle=\"veh-{index}\" />",
                    link_id(index)
                )
            }))
            .collect::<String>();
        let late_event = if rank == 0 {
            "<event time=\"86401\" type=\"entered link\" link=\"used\" vehicle=\"late\" />"
        } else {
            ""
        };
        fs::write(
            events.join(format!("events.{rank}.xml")),
            format!("<events>{rows}{late_event}</events>"),
        )
        .unwrap();
    }

    let from = Node::new(Id::create("from"), Coordinate::new_2d(0.0, 0.0), 0, 1);
    let to = Node::new(Id::create("to"), Coordinate::new_2d(1.0, 0.0), 0, 1);
    let mut network = Network::new();
    network.add_node(from.clone());
    network.add_node(to.clone());
    for index in 0..200 {
        let id = if index == 0 {
            "used".to_owned()
        } else if index < 5 {
            format!("used-{index}")
        } else {
            format!("unused-{index}")
        };
        network.add_link(Link::new_with_default(Id::create(&id), &from, &to));
    }
    let garage = Garage::default();
    let metadata =
        AnalysisRunMetadata::from_run(4711, &garage, Vec::new(), AnalysisInputPaths::default());

    let report = analyze_final_iteration(
        output,
        7,
        2,
        CompressionType::None,
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap();

    assert!(report.is_file());
    let hourly = fs::read_to_string(report.parent().unwrap().join("link_hourly.csv")).unwrap();
    assert!(hourly.contains("\"used\",3600,1,1"));
    assert!(hourly.contains("\"used\",86400,1,0"));
    assert!(hourly.contains("\"unused-5\",3600,0,0"));
    let coverage = fs::read_to_string(report.parent().unwrap().join("coverage.csv")).unwrap();
    assert!(coverage.contains("3600,200,5,195,2.500000"));
    let html = fs::read_to_string(&report).unwrap();
    assert!(
        html.contains("const volumes=[\"link_id,hour_start_seconds,entry_vehicles,exit_vehicles\"")
    );
    let status = fs::read_to_string(report.parent().unwrap().join("module_status.json")).unwrap();
    assert!(status.contains("\"status\": \"unavailable\""));
    let run_metadata =
        fs::read_to_string(report.parent().unwrap().join("run_metadata.json")).unwrap();
    assert!(run_metadata.contains("\"expected_travel\": []"));
    assert!(run_metadata.contains("\"vehicle_types\": []"));
    let manifest = fs::read_to_string(report.parent().unwrap().join("manifest.json")).unwrap();
    assert!(manifest.contains("\"iteration\": 7"));
    assert!(manifest.contains("\"random_seed\": 4711"));

    let missing_partition = analyze_final_iteration(
        output,
        7,
        3,
        CompressionType::None,
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap_err();
    assert!(
        missing_partition
            .to_string()
            .contains("missing final-iteration event partition")
    );

    let recorded_events: Vec<Vec<u8>> = (0..2)
        .map(|rank| fs::read(events.join(format!("events.{rank}.xml"))).unwrap())
        .collect();
    fs::write(events.join("events.0.xml"), "<events><event").unwrap();
    let parse_error = analyze_final_iteration(
        output,
        7,
        1,
        CompressionType::None,
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap_err();
    assert!(
        parse_error
            .to_string()
            .contains("failed to parse event XML")
    );
    assert!(report.is_file());
    // A required-module failure records its own diagnostics and leaves the completed report.
    let failure_dir = output.join("analysis-failure");
    let failure_manifest = fs::read_to_string(failure_dir.join("manifest.json")).unwrap();
    assert!(failure_manifest.contains("\"status\": \"failed\""));
    assert!(failure_manifest.contains("failed to parse event XML"));
    let failure_status = fs::read_to_string(failure_dir.join("module_status.json")).unwrap();
    assert!(failure_status.contains("\"module\": \"link_coverage\""));
    assert!(failure_status.contains("\"required\": true"));
    assert!(failure_status.contains("\"status\": \"failed\""));
    assert!(failure_status.contains("\"required\": false"));
    assert!(!failure_dir.join("link_hourly.csv").exists());
    let failure_report = fs::read_to_string(failure_dir.join("index.html")).unwrap();
    assert!(failure_report.contains("Analysis failed"));

    let proto_events = output.join("ITERS/it.8/events");
    fs::create_dir_all(&proto_events).unwrap();
    fs::write(proto_events.join("events.0.binpb"), [0x80]).unwrap();
    let truncated_proto = analyze_final_iteration(
        output,
        8,
        1,
        CompressionType::Proto,
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap_err();
    assert!(
        truncated_proto
            .to_string()
            .contains("failed to parse protobuf events")
    );
    assert!(report.is_file());

    // Recovering from a failed attempt republishes the completed report and clears the failure.
    fs::write(events.join("events.0.xml"), &recorded_events[0]).unwrap();
    fs::write(events.join("events.1.xml"), &recorded_events[1]).unwrap();
    analyze_final_iteration(
        output,
        7,
        2,
        CompressionType::None,
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap();
    assert!(!failure_dir.exists());
    assert!(!output.join(".analysis-failure-staging").exists());
    assert_eq!(
        fs::read_to_string(report.parent().unwrap().join("coverage.csv")).unwrap(),
        coverage,
    );

    // A crash between the two renames of a publish leaves only the backup. The next attempt
    // reclaims it instead of losing the previous report.
    fs::rename(report.parent().unwrap(), output.join(".analysis-backup")).unwrap();
    assert!(!report.parent().unwrap().exists());
    let republished = analyze_final_iteration(
        output,
        7,
        2,
        CompressionType::None,
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap();
    assert!(republished.is_file());
    assert!(!output.join(".analysis-backup").exists());
    assert_eq!(
        fs::read_to_string(report.parent().unwrap().join("coverage.csv")).unwrap(),
        coverage,
    );
}

#[deterministic_id_test(rust_qsim)]
fn simulation_publishes_only_the_final_iteration_report_after_shutdown() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/3-links/3-links-config-1.yml",
    ));
    config.controller_mut().last_iteration = 1;
    config.output_mut().analysis.enabled = true;
    config.output_mut().analysis.interval_seconds = 3600;
    let output_dir = config.output().output_dir.clone();
    let controller = ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap();
    controller.run();

    let report_dir = output_dir.join("analysis");
    let manifest = fs::read_to_string(report_dir.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"iteration\": 1"));
    assert!(report_dir.join("index.html").is_file());
    let run_metadata = fs::read_to_string(report_dir.join("run_metadata.json")).unwrap();
    assert!(run_metadata.contains("\"person_id\": \"100\""));
    assert!(run_metadata.contains("\"expected_travel_seconds\": null"));
    assert!(run_metadata.contains("\"vehicle_type_id\": \"car\""));
    assert!(run_metadata.contains("\"pce\": 2.0"));
    assert!(!output_dir.join("ITERS/it.0/analysis").exists());
    assert!(
        output_dir
            .join("ITERS/it.1/events/events.0.xml.zst")
            .is_file()
    );
    let hourly = fs::read_to_string(report_dir.join("link_hourly.csv")).unwrap();
    assert!(hourly.contains("link2"));
}

#[deterministic_id_test(rust_qsim)]
fn protobuf_partition_replay_matches_compressed_xml_report() {
    let run = |config_path: &str, output_dir: &str, table: &str| {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(config_path));
        config.controller_mut().last_iteration = 1;
        config.output_mut().output_dir = output_dir.into();
        config.output_mut().analysis.enabled = true;
        let output = config.output().output_dir.clone();
        let controller = ControllerBuilder::default_with_scenario(Scenario::load(config))
            .build()
            .unwrap();
        controller.run();
        fs::read_to_string(output.join("analysis").join(table)).unwrap()
    };

    for table in [
        "link_hourly.csv",
        "coverage.csv",
        "link_speed_hourly.csv",
        "link_speed_summary.csv",
        "link_speed_histogram.csv",
        "link_speed_diagnostics.csv",
    ] {
        let xml = run(
            "./tests/resources/3-links/3-links-config-1.yml",
            "./test_output/simulation/analysis_xml_equivalence",
            table,
        );
        let protobuf = run(
            "./tests/resources/3-links/3-links-config-2.yml",
            "./test_output/simulation/analysis_proto_equivalence",
            table,
        );
        assert_eq!(xml, protobuf, "{table} differs between event formats");
    }
}
