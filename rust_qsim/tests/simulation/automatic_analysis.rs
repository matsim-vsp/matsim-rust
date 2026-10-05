use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::capacity::VC_BIN_COUNT;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration,
};
use rust_qsim::simulation::config::{Analysis, CommandLineArgs, CompressionType, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scenario::network::{Link, Network, Node};
use rust_qsim::simulation::scenario::vehicles::{Garage, InternalVehicle, InternalVehicleType};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tempfile::TempDir;

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
    let metadata = run_metadata(4711, 1.0, &garage, &[]);

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
    assert!(html.contains("const h=[\"link_id,hour_start_seconds,entry_vehicles,exit_vehicles\""));
    let status = fs::read_to_string(report.parent().unwrap().join("module_status.json")).unwrap();
    assert!(status.contains("\"status\": \"unavailable\""));
    let recorded_metadata =
        fs::read_to_string(report.parent().unwrap().join("run_metadata.json")).unwrap();
    assert!(recorded_metadata.contains("\"expected_travel\": []"));
    assert!(recorded_metadata.contains("\"vehicle_types\": []"));
    // The sample size is recorded so a standalone rerun scales the same way.
    assert!(recorded_metadata.contains("\"sample_size\": 1.0"));
    let manifest = fs::read_to_string(report.parent().unwrap().join("manifest.json")).unwrap();
    assert!(manifest.contains("\"iteration\": 7"));
    assert!(manifest.contains("\"random_seed\": 4711"));
    assert!(manifest.contains("\"sample_size\": 1.0"));

    let capacity = fs::read_to_string(report.parent().unwrap().join("link_capacity.csv")).unwrap();
    // A full sample needs no scaling. The garage is empty, so the vehicles cannot be
    // weighted and the ratios stay unavailable, while the capacity stays reportable.
    let used = capacity_row(&capacity, "used", 3600);
    assert_eq!(used["link_id"], "\"used\"");
    assert_eq!(used["interval_start_seconds"], "3600");
    assert_eq!(used["capacity_pce_per_hour"], "1.000000");
    assert_eq!(used["interval_hours"], "1.000000");
    assert_eq!(used["sample_size"], "1.000000");
    assert_eq!(used["entry_vehicles"], "1");
    assert_eq!(used["exit_vehicles"], "1");
    assert_eq!(used["entry_pce"], "");
    assert_eq!(used["entry_unresolved_pce"], "1");
    assert_eq!(used["entry_vc"], "");
    assert_eq!(used["entry_vc_status"], "unavailable:missing_pce");
    assert_eq!(used["exit_vc_status"], "unavailable:missing_pce");
    // Every interval also retains the links without any traffic.
    let idle = capacity_row(&capacity, "unused-5", 3600);
    assert_eq!(idle["entry_vehicles"], "0");
    assert_eq!(idle["entry_vc"], "0.000000");
    assert_eq!(idle["entry_vc_status"], "available");
    let histogram = fs::read_to_string(report.parent().unwrap().join("vc_histogram.csv")).unwrap();
    assert!(histogram.contains(
        "interval_start_seconds,metric,bin_index,bin_lower,bin_upper,links,observations,unused_links,unavailable_links"
    ));
    // Hour 0 has no events at all, so all 200 links are unused. Hour 3600 has five
    // links whose vehicles are absent from the empty garage, so those are reported
    // as unavailable and the remaining 195 as unused.
    assert!(histogram.contains("0,entry_vc,0,0.000,0.100,0,0,200,0"));
    assert!(histogram.contains("3600,entry_vc,0,0.000,0.100,0,0,195,5"));
    assert!(histogram.contains("3600,exit_vc,0,0.000,0.100,0,0,195,5"));
    // The overflow bin is unbounded, and every bin repeats the interval totals.
    assert!(histogram.contains("3600,entry_vc,11,1.200,inf,0,0,195,5"));
    assert_eq!(
        histogram
            .lines()
            .filter(|line| line.starts_with("3600,entry_vc,"))
            .count(),
        VC_BIN_COUNT
    );
    let report_html = fs::read_to_string(&report).unwrap();
    assert!(report_html.contains("link_capacity.csv"));
    assert!(report_html.contains("vc_histogram.csv"));
    // The report opens on the entry-V/C distribution and can switch to the exit one.
    assert!(report_html.contains("Entry V/C (default view)"));
    assert!(report_html.contains("Show exit V/C"));
    assert!(report_html.contains("let metric='entry_vc'"));
    assert!(report_html.contains("'Show exit V/C':'Show entry V/C'"));

    let invalid_sample = analyze_final_iteration(
        output,
        7,
        2,
        CompressionType::None,
        7200,
        &run_metadata(4711, 0.0, &garage, &[]),
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap_err();
    assert!(
        invalid_sample
            .to_string()
            .contains("sample size must be a positive finite number")
    );

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

    // The car has a PCE of 2.0 and the run is a full sample, so the single entry on
    // each link is 2 PCE against 3600 PCE/h of whole-link capacity over one hour.
    let capacity = fs::read_to_string(report_dir.join("link_capacity.csv")).unwrap();
    let used = capacity_row(&capacity, "link1", 32400);
    assert_eq!(used["entry_vehicles"], "1");
    assert_eq!(used["entry_pce"], "2.000000");
    assert_eq!(used["entry_pce_scaled"], "2.000000");
    assert_eq!(used["entry_flow_pce_per_hour"], "2.000000");
    assert_eq!(used["entry_vc"], "0.000556");
    assert_eq!(used["entry_vc_status"], "available");
    // The agent departs at 09:00, so the earlier interval really carried nothing.
    let empty = capacity_row(&capacity, "link1", 0);
    assert_eq!(empty["entry_vc"], "0.000000");
    assert_eq!(empty["entry_vc_status"], "available");
    assert!(manifest.contains("\"sample_size\": 1.0"));
    let histogram = fs::read_to_string(report_dir.join("vc_histogram.csv")).unwrap();
    // link1, link2 and link3 all carry one entry each in the populated interval.
    assert!(histogram.contains("32400,entry_vc,0,0.000,0.100,3,3,0,0"));
    assert!(histogram.contains("0,entry_vc,0,0.000,0.100,0,0,3,0"));
}

#[deterministic_id_test(rust_qsim)]
fn capacity_utilization_scales_by_sample_size_and_mixed_pce() {
    // A quarter sample, a 30 minute interval and a link with two lanes: the ratio
    // must be the observed PCE volume scaled up by four, over 900 PCE/h * 0.5 h.
    let report = capacity_report(
        "pce_scaling",
        0.25,
        1800,
        900.0,
        &[("car", 1.0), ("truck", 3.0)],
        &[
            ("car", "entered link", 60),
            ("car", "left link", 120),
            ("truck", "entered link", 60),
            // Intervals include their start, so this belongs to the next interval.
            ("car", "entered link", 1800),
        ],
    );
    let capacity = report.read("link_capacity.csv");
    // 1.0 + 3.0 = 4 PCE observed, scaled up by 1/0.25 to 16 PCE, 32 PCE/h, over
    // 900 PCE/h * 0.5 h = 450 PCE.
    let row = report.row("link1", 0);
    assert_eq!(row["capacity_pce_per_hour"], "900.000000");
    assert_eq!(row["interval_hours"], "0.500000");
    assert_eq!(row["sample_size"], "0.250000");
    assert_eq!(row["entry_vehicles"], "2");
    assert_eq!(row["exit_vehicles"], "1");
    assert_eq!(row["entry_pce"], "4.000000");
    assert_eq!(row["exit_pce"], "1.000000");
    assert_eq!(row["entry_pce_scaled"], "16.000000");
    assert_eq!(row["exit_pce_scaled"], "4.000000");
    assert_eq!(row["entry_flow_pce_per_hour"], "32.000000");
    assert_eq!(row["exit_flow_pce_per_hour"], "8.000000");
    assert_eq!(row["entry_vc"], "0.035556");
    assert_eq!(row["exit_vc"], "0.008889");
    assert_eq!(row["entry_vc_status"], "available");
    // The two lanes are exported alongside, but never applied to the 900 PCE/h
    // whole-link capacity a second time.
    assert_eq!(row["permlanes"], "2.000000");
    assert!(!capacity.contains("1800.000000"));
    // Intervals include their start and exclude their end, so the entry at exactly
    // 1800 s opens the second interval rather than extending the first.
    let next = report.row("link1", 1800);
    assert_eq!(next["entry_vehicles"], "1");
    assert_eq!(next["entry_pce"], "1.000000");
    assert_eq!(next["entry_pce_scaled"], "4.000000");
    assert_eq!(next["entry_vc"], "0.008889");
    assert_eq!(capacity.matches("\"link1\"").count(), 2);
}

#[deterministic_id_test(rust_qsim)]
fn capacity_utilization_keeps_genuinely_zero_flow_distinct_from_unusable_links() {
    // No vehicle catalog at all: a used link cannot be weighted, while a link that
    // never saw traffic keeps a real zero ratio.
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let events = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events).unwrap();
    fs::write(
        events.join("events.0.xml"),
        "<events><event time=\"60\" type=\"entered link\" link=\"used\" vehicle=\"ghost\" /></events>",
    )
    .unwrap();
    let network = two_link_network(&[("used", 1800.0), ("idle", 1800.0)]);
    let metadata = run_metadata(1, 1.0, &Garage::default(), &[]);
    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
        },
    )
    .unwrap();
    let report = CapacityReport {
        dir: report.parent().unwrap().to_path_buf(),
        _output: temp,
    };

    // The used link is unusable for the entry side only; its exit side saw no
    // traffic, so the zero ratio stays available there.
    let used = report.row("used", 0);
    assert_eq!(used["entry_vehicles"], "1");
    assert_eq!(used["entry_pce"], "");
    assert_eq!(used["entry_unresolved_pce"], "1");
    assert_eq!(used["entry_vc"], "");
    assert_eq!(used["entry_vc_status"], "unavailable:missing_pce");
    assert_eq!(used["exit_vehicles"], "0");
    assert_eq!(used["exit_vc"], "0.000000");
    assert_eq!(used["exit_vc_status"], "available");
    // A link that never carried anything keeps a real zero ratio on both sides.
    let idle = report.row("idle", 0);
    assert_eq!(idle["entry_vehicles"], "0");
    assert_eq!(idle["entry_vc"], "0.000000");
    assert_eq!(idle["entry_vc_status"], "available");
    // The used link is counted apart from the idle one, so a genuine zero flow is
    // never mixed up with a ratio that could not be computed.
    let histogram = report.read("vc_histogram.csv");
    assert!(histogram.contains("0,entry_vc,0,0.000,0.100,0,0,1,1"));
    assert!(histogram.contains("0,exit_vc,0,0.000,0.100,0,0,2,0"));
}

#[deterministic_id_test(rust_qsim)]
fn capacity_utilization_reports_zero_capacity_as_unavailable() {
    // A link that did carry traffic still reports the volumes it carried; only the
    // ratio, which divides by the capacity, becomes unavailable.
    let report = capacity_report(
        "zero_capacity",
        0.25,
        1800,
        0.0,
        &[("car", 1.0), ("truck", 3.0)],
        &[("car", "entered link", 60), ("truck", "entered link", 60)],
    );
    let row = report.row("link1", 0);
    assert_eq!(row["capacity_pce_per_hour"], "0.000000");
    assert_eq!(row["effective_capacity_pce"], "");
    assert_eq!(row["entry_vehicles"], "2");
    assert_eq!(row["entry_pce"], "4.000000");
    assert_eq!(row["entry_pce_scaled"], "16.000000");
    assert_eq!(row["entry_flow_pce_per_hour"], "32.000000");
    assert_eq!(row["entry_vc"], "");
    assert_eq!(row["entry_vc_status"], "unavailable:invalid_capacity");
    assert_eq!(row["exit_vc_status"], "unavailable:invalid_capacity");
    // A link that cannot be weighed is unavailable, not unused.
    let histogram = report.read("vc_histogram.csv");
    assert!(histogram.contains("0,entry_vc,0,0.000,0.100,0,0,0,1"));
}

#[deterministic_id_test(rust_qsim)]
fn capacity_utilization_credits_a_truncated_final_interval_only_for_its_window() {
    // A 1 h interval but 1.5 h of simulation, with one PCE-1 vehicle entering at
    // 4800 s. The final interval really covers only half an hour, so it may claim
    // 3600 * 0.5 PCE of capacity, not a full hour's worth. Crediting it a full hour
    // would halve both the reported flow and the V/C.
    let report = capacity_report_until(
        "truncated_interval",
        1.0,
        3600,
        5400,
        3600.0,
        &[("car", 1.0)],
        &[("car", "entered link", 4800)],
    );
    let last = report.row("link1", 3600);
    assert_eq!(last["interval_hours"], "0.500000");
    assert_eq!(last["effective_capacity_pce"], "1800.000000");
    assert_eq!(last["entry_pce"], "1.000000");
    assert_eq!(last["entry_flow_pce_per_hour"], "2.000000");
    assert_eq!(last["entry_vc"], "0.000556");
    assert_eq!(last["entry_vc_status"], "available");
    // The complete first interval keeps its full hour.
    let first = report.row("link1", 0);
    assert_eq!(first["interval_hours"], "1.000000");
    assert_eq!(first["effective_capacity_pce"], "3600.000000");
    assert_eq!(first["entry_vc"], "0.000000");
}

#[deterministic_id_test(rust_qsim)]
fn metric_catalog_names_match_the_exported_columns() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/3-links/3-links-config-1.yml",
    ));
    config.controller_mut().last_iteration = 1;
    config.output_mut().analysis.enabled = true;
    let output_dir = config.output().output_dir.clone();
    ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();

    let report_dir = output_dir.join("analysis");
    let catalog = fs::read_to_string(report_dir.join("metric_catalog.json")).unwrap();
    let names: Vec<String> = catalog
        .lines()
        .filter_map(|line| line.trim().strip_prefix("\"name\": \""))
        .map(|line| line.trim_end_matches("\",").to_owned())
        .collect();
    assert!(!names.is_empty());

    // Every catalogued name has to be findable in the file it describes, otherwise a
    // consumer cannot look the column up.
    const TABLES: [&str; 4] = [
        "link_hourly.csv",
        "coverage.csv",
        "link_capacity.csv",
        "vc_histogram.csv",
    ];
    let headers: Vec<Vec<String>> = TABLES
        .iter()
        .map(|file| {
            fs::read_to_string(report_dir.join(file))
                .unwrap()
                .lines()
                .next()
                .unwrap()
                .split(',')
                .map(str::to_owned)
                .collect()
        })
        .collect();
    // A name is catalogued once and described by one table, so it only has to appear
    // in at least one of them, not in each.
    for name in &names {
        assert!(
            headers
                .iter()
                .any(|header| header.iter().any(|column| column == name)),
            "{name} is catalogued but is not a column in any of {TABLES:?}"
        );
    }
    // The capacity denominators the acceptance criteria call for are catalogued.
    for expected in [
        "capacity_pce_per_hour",
        "effective_capacity_pce",
        "entry_flow_pce_per_hour",
        "exit_flow_pce_per_hour",
        "entry_vc",
        "exit_vc",
        "unused_links",
        "unavailable_links",
    ] {
        assert!(
            names.iter().any(|name| name == expected),
            "{expected} is not catalogued"
        );
    }
}

#[deterministic_id_test(rust_qsim)]
fn capacity_utilization_uses_effective_capacity_and_survives_event_order() {
    // The 900 PCE/h link with a half-hour interval has 450 PCE of effective
    // capacity, which is the denominator of the exported ratio.
    let report = capacity_report(
        "effective_capacity",
        1.0,
        1800,
        900.0,
        &[("a", 1.0), ("b", 1.0)],
        &[("a", "entered link", 60), ("b", "entered link", 60)],
    );
    let row = report.row("link1", 0);
    assert_eq!(row["capacity_pce_per_hour"], "900.000000");
    assert_eq!(row["interval_hours"], "0.500000");
    assert_eq!(row["effective_capacity_pce"], "450.000000");
    assert_eq!(row["entry_pce"], "2.000000");
    assert_eq!(row["entry_vc"], format!("{:.6}", 2.0 / 450.0));

    // Fractional PCEs whose binary sum depends on the order of the crossings. The
    // same vehicles must produce the same total, and therefore the same histogram
    // bin, no matter which partition replayed them first.
    // These three PCE values add up to exactly 0.5 PCE, which is 0.1 over 5 PCE/h of
    // capacity: a bin edge. Summed as binary floating point they reach 0.5 in some
    // orders and 0.49999999999999994 in others, which would put the ratio in bin 0 or
    // bin 1 depending only on the order the partitions were replayed. The exact sum
    // has to land on the edge, in the higher bin, either way.
    let forward = [("p", 0.1), ("q", 0.05), ("r", 0.35)];
    let reversed = [("r", 0.35), ("p", 0.1), ("q", 0.05)];
    let bins: Vec<_> = [forward, reversed]
        .iter()
        .enumerate()
        .map(|(index, vehicles)| {
            let report = capacity_report(
                &format!("order_{index}"),
                1.0,
                3600,
                5.0,
                vehicles,
                &vehicles
                    .iter()
                    .map(|(vehicle, _)| (*vehicle, "entered link", 60u32))
                    .collect::<Vec<_>>(),
            );
            let histogram = report.read("vc_histogram.csv");
            let row = report.row("link1", 0);
            assert_eq!(row["entry_pce"], "0.500000");
            assert_eq!(row["entry_vc"], "0.100000");
            // The `links` column is the sixth, and only the occupied bin is nonzero.
            let binned = histogram
                .lines()
                .find(|line| {
                    line.starts_with("0,entry_vc,")
                        && line.split(',').nth(5).is_some_and(|links| links != "0")
                })
                .expect("the link is binned somewhere")
                .to_owned();
            binned
        })
        .collect();
    assert_eq!(bins[0], bins[1]);
    // The naive binary sum is not reproducible, so the fixed-point accumulation is
    // what keeps the ratio on one side of the 0.1 bin edge.
    assert_ne!(0.1f64 + 0.05 + 0.35, 0.35f64 + 0.1 + 0.05);
    // A value exactly on an edge belongs to the higher bin.
    assert!(bins[0].starts_with("0,entry_vc,1,0.100,0.200,1,1,0,0"));
}

/// The `link_capacity.csv` row for one link and interval, keyed by column name.
fn capacity_row(csv: &str, link_id: &str, interval_start_seconds: u64) -> HashMap<String, String> {
    let expected_interval = interval_start_seconds.to_string();
    let expected_link = format!("\"{link_id}\"");
    let mut lines = csv.lines();
    let header: Vec<String> = lines.next().unwrap().split(',').map(String::from).collect();
    let row = lines
        .find(|line| {
            let fields: Vec<&str> = line.split(',').collect();
            fields[0] == expected_link && fields[1] == expected_interval
        })
        .unwrap_or_else(|| panic!("no capacity row for {link_id} at {interval_start_seconds}"));
    assert_eq!(
        header.len(),
        row.split(',').count(),
        "capacity row does not match the header"
    );
    header
        .into_iter()
        .zip(row.split(',').map(String::from))
        .collect()
}

/// Analysis metadata for a synthetic report, with the given sample size and vehicle PCEs.
///
/// The vehicles are what the analysis weights volumes with, so a test that wants a
/// weighted volume has to put the vehicle in this catalog.
fn run_metadata(
    random_seed: u64,
    sample_size: f64,
    garage: &Garage,
    pce_by_vehicle: &[(&str, f64)],
) -> AnalysisRunMetadata {
    let mut garage = garage.clone();
    for (vehicle_id, pce) in pce_by_vehicle {
        let type_id = Id::<InternalVehicleType>::create(&format!("{vehicle_id}-type"));
        garage.add_veh_type(InternalVehicleType {
            id: type_id.clone(),
            length: 5.0,
            width: 2.0,
            max_v: 10.0,
            pce: *pce,
            fef: 1.0,
            net_mode: Id::create("car"),
            attributes: Default::default(),
        });
        garage.add_veh(InternalVehicle {
            id: Id::create(vehicle_id),
            max_v: 10.0,
            pce: *pce,
            vehicle_type: type_id,
            attributes: Default::default(),
        });
    }
    AnalysisRunMetadata::from_run(
        random_seed,
        sample_size,
        &garage,
        Vec::new(),
        AnalysisInputPaths::default(),
    )
}

/// A network with `link1` carrying the given capacity and any extra links.
fn two_link_network(links: &[(&str, f64)]) -> Network {
    let from = Node::new(Id::create("from"), Coordinate::new_2d(0.0, 0.0), 0, 1);
    let to = Node::new(Id::create("to"), Coordinate::new_2d(1.0, 0.0), 0, 1);
    let mut network = Network::new();
    network.add_node(from.clone());
    network.add_node(to.clone());
    for (id, capacity) in links {
        let mut link = Link::new_with_default(Id::create(id), &from, &to);
        link.capacity = *capacity;
        link.permlanes = 2.0;
        network.add_link(link);
    }
    network
}

/// A published analysis directory that keeps its temporary output alive.
struct CapacityReport {
    dir: PathBuf,
    _output: TempDir,
}

impl CapacityReport {
    fn read(&self, name: &str) -> String {
        fs::read_to_string(self.dir.join(name)).unwrap()
    }

    fn row(&self, link_id: &str, interval_start_seconds: u64) -> HashMap<String, String> {
        capacity_row(
            &self.read("link_capacity.csv"),
            link_id,
            interval_start_seconds,
        )
    }
}

/// Run the analysis on synthetic events for a single `link1` network.
fn capacity_report(
    name: &str,
    sample_size: f64,
    interval_seconds: u32,
    capacity: f64,
    vehicles: &[(&str, f64)],
    events: &[(&str, &str, u32)],
) -> CapacityReport {
    capacity_report_until(
        name,
        sample_size,
        interval_seconds,
        interval_seconds,
        capacity,
        vehicles,
        events,
    )
}

/// As [`capacity_report`], but with an explicit simulation end time so a final
/// interval shorter than `interval_seconds` can be exercised.
fn capacity_report_until(
    name: &str,
    sample_size: f64,
    interval_seconds: u32,
    simulation_end_time: u32,
    capacity: f64,
    vehicles: &[(&str, f64)],
    events: &[(&str, &str, u32)],
) -> CapacityReport {
    let output_dir = TempDir::new().unwrap();
    let output = output_dir.path();
    let events_dir = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events_dir).unwrap();
    let xml = events
        .iter()
        .map(|(vehicle, kind, time)| {
            format!(
                "<event time=\"{time}\" type=\"{kind}\" link=\"link1\" vehicle=\"{vehicle}\" />"
            )
        })
        .collect::<String>();
    fs::write(
        events_dir.join("events.0.xml"),
        format!("<events>{xml}</events>"),
    )
    .unwrap();
    let network = two_link_network(&[("link1", capacity)]);
    let metadata = run_metadata(1, sample_size, &Garage::default(), vehicles);
    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        simulation_end_time,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds,
        },
    )
    .unwrap();
    let dir = report
        .parent()
        .expect("report path has no parent")
        .to_path_buf();
    assert!(
        dir.file_name()
            .is_some_and(|published| published == "analysis"),
        "{name} report was not published to {dir:?}"
    );
    CapacityReport {
        dir,
        _output: output_dir,
    }
}

#[deterministic_id_test(rust_qsim)]
fn protobuf_partition_replay_matches_compressed_xml_report() {
    let run = |config_path: &str, output_dir: &str| {
        let mut config = Config::from_args(CommandLineArgs::new_with_path(config_path));
        config.controller_mut().last_iteration = 1;
        config.output_mut().output_dir = output_dir.into();
        config.output_mut().analysis.enabled = true;
        let output = config.output().output_dir.clone();
        let controller = ControllerBuilder::default_with_scenario(Scenario::load(config))
            .build()
            .unwrap();
        controller.run();
        [
            fs::read_to_string(output.join("analysis/link_hourly.csv")).unwrap(),
            fs::read_to_string(output.join("analysis/link_capacity.csv")).unwrap(),
            fs::read_to_string(output.join("analysis/vc_histogram.csv")).unwrap(),
        ]
    };

    let xml = run(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/analysis_xml_equivalence",
    );
    let protobuf = run(
        "./tests/resources/3-links/3-links-config-2.yml",
        "./test_output/simulation/analysis_proto_equivalence",
    );
    assert_eq!(xml, protobuf);
}
