use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::capacity::VC_BIN_COUNT;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, AnalysisRuntimeMetadata, analyze_final_iteration,
};
use rust_qsim::simulation::config::{
    Accessibility, Analysis, CommandLineArgs, CompressionType, Config, LinkLabels, ServiceInputs,
};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::replanning::routing::teleportation::TeleportationRoutingModule;
use rust_qsim::simulation::replanning::routing::{RoutingModule, RoutingRequestBuilder};
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scenario::facilities::{ActivityFacility, Facility};
use rust_qsim::simulation::scenario::network::{Link, Network, Node};
use rust_qsim::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
    InternalPlanElement, InternalRoute, Population,
};
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
    let metadata = run_metadata(4711, 1.0, &garage, &[]).with_runtime(AnalysisRuntimeMetadata {
        simulation_seconds: Some(12.5),
        phase_seconds: [("mobsim".to_owned(), 8.0)].into(),
        worker_count: Some(2),
        operating_system: Some("test-os".to_owned()),
        architecture: Some("test-arch".to_owned()),
        cpu_model: Some("test-cpu".to_owned()),
        host_memory_bytes: Some(64000),
        software_name: Some("rust_qsim".to_owned()),
        software_version: Some("test-version".to_owned()),
        network_links: Some(200),
        population_persons: Some(11),
        vehicles: Some(3),
        expected_legs: Some(5),
        ..AnalysisRuntimeMetadata::default()
    });
    let observed_data = output.join("observed.csv");
    fs::write(
        &observed_data,
        "link_id,period_start_seconds,period_end_seconds,vehicle_class,metric,unit,value,split,source\n\
         used,3600,7200,all,count,vehicles,1,calibration,counter-west\n\
         used,3600,7200,all,speed,m/s,0.01,holdout,sensor-east\n\
         used,3600,7200,all,count,vehicles,0,holdout,counter-zero\n\
         used,3600,7200,bus,count,vehicles,1,calibration,counter-bus\n\
         used,3600,5400,all,count,vehicles,1,calibration,counter-period\n\
         missing,3600,7200,all,count,vehicles,1,holdout,counter-north\n",
    )
    .unwrap();
    let comparison_report = output.join("comparison/analysis");
    fs::create_dir_all(&comparison_report).unwrap();
    fs::write(
        comparison_report.join("manifest.json"),
        r#"{"status":"complete","iteration":3,"sample_size":0.5,"interval_seconds":3600}"#,
    )
    .unwrap();
    fs::write(
        comparison_report.join("link_hourly.csv"),
        "link_id,hour_start_seconds,entry_vehicles\nused,3600,5\n",
    )
    .unwrap();
    fs::write(
        comparison_report.join("link_speed_hourly.csv"),
        "link_id,hour_start_seconds,representative_speed_mps\nused,3600,10\n",
    )
    .unwrap();
    fs::write(
        comparison_report.join("link_hourly_by_class.csv"),
        "vehicle_class,link_id,hour_start_seconds,entry_vehicles\n",
    )
    .unwrap();
    fs::write(
        comparison_report.join("link_speed_by_class.csv"),
        "vehicle_class,link_id,hour_start_seconds,representative_speed_mps\n",
    )
    .unwrap();

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
            observed_data: Some(observed_data),
            journey_survey: None,
            comparison_runs: vec![PathBuf::from("comparison")],
            ..Analysis::default()
        },
    )
    .unwrap();

    assert!(report.is_file());
    let runtime_dir = report.parent().unwrap();
    let runtime_json: serde_json::Value =
        serde_json::from_slice(&fs::read(runtime_dir.join("runtime_metadata.json")).unwrap())
            .unwrap();
    assert_eq!(runtime_json["simulation_seconds"], 12.5);
    assert_eq!(runtime_json["phase_seconds"]["mobsim"], 8.0);
    assert_eq!(runtime_json["worker_count"], 2);
    assert_eq!(runtime_json["network_links"], 200);
    assert_eq!(runtime_json["cpu_model"], "test-cpu");
    assert_eq!(runtime_json["host_memory_bytes"], 64000);
    assert!(runtime_json["analysis_seconds"].as_f64().is_some());
    assert!(runtime_json["peak_memory_bytes"].is_null());
    let runtime_csv = fs::read_to_string(runtime_dir.join("runtime.csv")).unwrap();
    assert!(
        runtime_csv.contains("\"simulation_runtime\",\"12.5\",\"seconds\",\"measured wall clock\"")
    );
    assert!(runtime_csv.contains("\"worker_count\",\"2\",\"workers\",\"configured partitions\""));
    assert!(runtime_csv.contains("\"cpu_model\",\"test-cpu\",\"\",\"host query\""));
    assert!(runtime_csv.contains("\"host_memory\",\"64000\",\"bytes\",\"host query\""));
    assert!(!runtime_csv.contains("peak_memory"));
    let html = fs::read_to_string(&report).unwrap();
    assert!(html.contains("Execution context"));
    assert!(html.contains("simulation_runtime"));
    let hourly = fs::read_to_string(report.parent().unwrap().join("link_hourly.csv")).unwrap();
    assert!(hourly.contains("\"used\",3600,1,1"));
    assert!(hourly.contains("\"used\",86400,1,0"));
    assert!(hourly.contains("\"unused-5\",3600,0,0"));
    let coverage = fs::read_to_string(report.parent().unwrap().join("coverage.csv")).unwrap();
    assert!(coverage.contains("3600,200,5,195,2.500000"));
    let validation = report.parent().unwrap();
    let validation_matches = fs::read_to_string(validation.join("validation_matches.csv")).unwrap();
    assert!(validation_matches.contains("\"used\",3600,\"all\",\"count\",\"calibration\",1.000000,1.000000,1.000000,1.000000,0.000000,0.000000"));
    assert!(validation_matches.contains("\"used\",3600,\"all\",\"speed\",\"holdout\",0.010000,0.010000,1.000000,0.010000,0.000000,0.000000"));
    assert!(validation_matches.contains("counter-west"));
    let unmatched = fs::read_to_string(validation.join("validation_unmatched.csv")).unwrap();
    assert!(unmatched.contains("no_simulation_match"));
    assert!(unmatched.contains("counter-north"));
    assert!(unmatched.contains("vehicle_class_unavailable"));
    assert!(unmatched.contains("period_mismatch"));
    assert!(unmatched.contains("counter-bus"));
    assert!(unmatched.contains("counter-period"));
    let summary = fs::read_to_string(validation.join("validation_summary.csv")).unwrap();
    assert!(summary.contains("holdout,count,all,1,1.000000,1.000000,1.000000,1.414214,1,1,1"));
    let cross_run = fs::read_to_string(validation.join("cross_run_comparison.csv")).unwrap();
    assert!(cross_run.contains("comparison,3,entry_vehicles,used,3600,7200,all,5,0.5,10,vehicles"));
    assert!(
        cross_run
            .contains("comparison,3,representative_speed_mps,used,3600,7200,all,10,0.5,10,m/s")
    );
    for plot in [
        "validation_scatter_count_calibration.svg",
        "validation_scatter_count_holdout.svg",
        "validation_scatter_speed_calibration.svg",
        "validation_scatter_speed_holdout.svg",
    ] {
        assert!(validation.join(plot).is_file());
    }
    assert!(validation.join("validation_time_profiles.svg").is_file());
    assert!(validation.join("validation_residual_map.svg").is_file());
    assert!(
        validation
            .join("validation_time_profiles_calibration.svg")
            .is_file()
    );
    assert!(
        validation
            .join("validation_time_profiles_holdout.svg")
            .is_file()
    );
    assert!(
        validation
            .join("validation_residual_map_calibration.svg")
            .is_file()
    );
    assert!(
        validation
            .join("validation_residual_map_holdout.svg")
            .is_file()
    );
    let html = fs::read_to_string(&report).unwrap();
    // The report embeds the hourly rows and the coverage CSV verbatim; assert the
    // payload's columns and values rather than a bare variable declaration.
    assert!(html.contains(
        "\"link_id\":\"used\",\"hour_start_seconds\":3600,\"entry_vehicles\":1,\"exit_vehicles\":1"
    ));
    assert!(html.contains("id=\"cross-run\""));
    assert!(html.contains("csvTable('#cross-run'"));
    assert!(
        html.contains("Economic appraisal is unavailable: No economic input CSV is configured.")
    );
    assert!(!html.contains("Traveler utility is converted"));
    assert!(
        html.contains(
            "[\"hour_start_seconds,eligible_links,used_links,unused_links,used_percent\","
        )
    );
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
            ..Analysis::default()
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
            ..Analysis::default()
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
            ..Analysis::default()
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
            ..Analysis::default()
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
            ..Analysis::default()
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
            ..Analysis::default()
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
fn shared_analysis_reconstructs_staged_and_incomplete_journeys() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let events = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events).unwrap();
    fs::write(
        events.join("events.0.xml"),
        r#"<events>
          <event time="0" type="departure" person="p" link="l" legMode="walk" computationalRoutingMode="walk" />
          <event time="10" type="arrival" person="p" link="l" legMode="walk" />
          <event time="20" type="departure" person="p" link="l" legMode="pt" computationalRoutingMode="pt" />
          <event time="50" type="arrival" person="p" link="l" legMode="pt" />
          <event time="60" type="departure" person="p" link="l" legMode="pt" computationalRoutingMode="pt" />
          <event time="80" type="arrival" person="p" link="l" legMode="pt" />
          <event time="90" type="departure" person="p" link="l" legMode="walk" computationalRoutingMode="walk" />
          <event time="100" type="arrival" person="p" link="l" legMode="walk" />
          <event time="200" type="departure" person="p" link="l" legMode="car" computationalRoutingMode="car" />
          <event time="30" type="departure" person="walker" link="l" legMode="walk" computationalRoutingMode="walk" />
          <event time="40" type="arrival" person="walker" link="l" legMode="walk" />
          <event time="40" type="departure" person="teleporter" link="l" legMode="walk" computationalRoutingMode="walk" />
          <event time="50" type="arrival" person="teleporter" link="l" legMode="walk" />
        </events>"#,
    )
    .unwrap();

    let link = || Id::<Link>::create("l");
    let activity = |kind: &str| {
        InternalPlanElement::Activity(InternalActivity::new(None, kind, link(), None, None, None))
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
            attributes: Default::default(),
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
            leg("car", 7000.0),
            activity("home"),
        ],
        attributes: Default::default(),
    };
    let walking_plan = InternalPlan {
        score: None,
        selected: true,
        elements: vec![
            activity("home"),
            leg("walk", 400.0),
            activity("work"),
            leg("car", 7000.0),
            activity("home"),
        ],
        attributes: Default::default(),
    };
    let from = Facility::ActivityFacility(ActivityFacility {
        id: Id::create("from"),
        coord: Coordinate::new_2d(0.0, 0.0),
        link_id: link(),
        mode_to_link: Default::default(),
        desc: None,
        activities: Vec::new(),
        attributes: Default::default(),
    });
    let to = Facility::ActivityFacility(ActivityFacility {
        id: Id::create("to"),
        coord: Coordinate::new_2d(3.0, 4.0),
        link_id: link(),
        mode_to_link: Default::default(),
        desc: None,
        activities: Vec::new(),
        attributes: Default::default(),
    });
    let teleported_leg = TeleportationRoutingModule::new(Id::create("walk"), 1.3, 2.0)
        .calc_route(
            RoutingRequestBuilder::default()
                .from(&from)
                .to(&to)
                .build()
                .unwrap(),
        )
        .unwrap();
    let mut teleported_elements = vec![activity("home")];
    teleported_elements.extend(teleported_leg);
    teleported_elements.push(activity("work"));
    let teleported_plan = InternalPlan {
        score: None,
        selected: true,
        elements: teleported_elements,
        attributes: Default::default(),
    };
    let population = Population::from_persons(vec![
        InternalPerson::new(Id::create("p"), plan),
        InternalPerson::new(Id::create("walker"), walking_plan),
        InternalPerson::new(Id::create("teleporter"), teleported_plan),
    ]);
    let metadata = AnalysisRunMetadata::from_run(
        1,
        1.0,
        WINDOW_START_SECONDS,
        &Garage::default(),
        rust_qsim::simulation::analysis::capture_expected_travel(&population),
        AnalysisInputPaths::default(),
    );
    let survey = output.join("journey_survey.csv");
    fs::write(
        &survey,
        "study_population,journey_definition,split,mode,purpose,departure_seconds,duration_seconds,distance_meters,weight,uncertainty\n100,matsim-substantive-activities-v1,calibration,pt,work,0,100,6700,2,0.1\n100,matsim-substantive-activities-v1,calibration,bicycle,school,1200,900,5000,1,0.2\n",
    )
    .unwrap();
    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &metadata,
        &Network::new(),
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
            journey_survey: Some(survey),
            ..Analysis::default()
        },
    )
    .unwrap();
    let report_dir = report.parent().unwrap();
    let journeys = fs::read_to_string(report_dir.join("journeys.csv")).unwrap();
    assert!(journeys.contains(
        "\"p\",0,0.000000,0,\"home\",\"work\",\"l\",\"l\",\"work\",\"pt\",\"walk|pt|pt|walk\",\"1|3|5|7\",100.000000,completed,6700.000000,5_to_10_km,planned_route"
    ));
    assert!(journeys.contains(
        "\"p\",1,200.000000,0,\"work\",\"home\",\"l\",\"l\",\"home\",\"car\",\"car\",\"9\",,incomplete,7000.000000,5_to_10_km,planned_route"
    ));
    assert!(journeys.contains(
        "\"walker\",0,30.000000,0,\"home\",\"work\",\"l\",\"l\",\"work\",\"walk\",\"walk\",\"1\",10.000000,completed,400.000000,under_1_km,planned_route"
    ));
    assert!(
        journeys
            .lines()
            .any(|row| row.contains("walker") && row.contains(",not_departed,"))
    );
    assert!(journeys.contains(
        "\"teleporter\",0,40.000000,0,\"home\",\"work\",\"l\",\"l\",\"work\",\"walk\",\"walk\",\"1\",10.000000,completed,6.500000,under_1_km,planned_route"
    ), "{journeys}");
    let shares = fs::read_to_string(report_dir.join("journey_mode_share.csv")).unwrap();
    assert!(shares.contains("0,\"work\",5_to_10_km,\"pt\",1,1.000000"));
    assert!(shares.contains("0,\"work\",under_1_km,\"walk\",2,1.000000"));
    let summary = fs::read_to_string(report_dir.join("journey_summary.csv")).unwrap();
    assert!(summary.contains(
        "\"pt\",\"work\",1,1,100.000000,0.000000,100.000000,100.000000,6700.000000,0.000000,6700.000000,6700.000000"
    ));
    assert!(summary.contains(
        "\"walk\",\"work\",2,2,10.000000,0.000000,10.000000,10.000000,203.250000,196.750000,400.000000,400.000000"
    ));
    let html = fs::read_to_string(&report).unwrap();
    assert!(html.contains("journey_mode_share.csv"));
    assert!(html.contains("Journey duration and distance distributions"));
    assert!(html.contains("journey-summary"));
    assert!(html.contains("Journey mode share by hour, purpose, and distance"));
    assert!(html.contains("Travel survey comparison"));
    let survey_rows = fs::read_to_string(report_dir.join("journey_survey_comparison.csv")).unwrap();
    assert!(survey_rows.contains("calibration,mode,pt,2.000000,0.666667,3.000000,1,0.250000"));
    assert!(survey_rows.contains("calibration,mode,bicycle,1.000000,0.333333,3.000000,0,0.000000"));
    assert!(
        survey_rows.contains("matsim-substantive-activities-v1,0.200000,missing_simulation_group")
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
            ..Analysis::default()
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
    // Each metric is written as a pretty-printed object, so a name and the aggregation key
    // that describes it are the two lines that follow each other.
    let fields: Vec<&str> = catalog
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            line.strip_prefix("\"name\": \"")
                .or_else(|| line.strip_prefix("\"aggregation_key\": \""))
                // The name comes first and closes the object, so only the key's line can end in
                // a comma; drop either trailing quote and comma.
                .map(|value| value.trim_end_matches(['"', ',']))
        })
        .collect();
    assert!(!fields.is_empty());
    let entries: Vec<(&str, &str)> = fields
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect();
    assert_eq!(
        entries.len() * 2,
        fields.len(),
        "metric fields are not paired"
    );
    let names: Vec<&str> = entries.iter().map(|(name, _)| *name).collect();

    // A name does not have to be a column, because two tables can export the same column name
    // for different metrics. The aggregation key does: it names the columns that identify one
    // of the metric's rows, so a consumer can look the metric up in the table that exports them.
    const TABLES: &[&str] = &[
        "link_hourly.csv",
        "coverage.csv",
        "link_capacity.csv",
        "vc_histogram.csv",
        "group_coverage.csv",
        "link_speed_hourly.csv",
        "link_speed_summary.csv",
        "link_speed_histogram.csv",
        "link_speed_diagnostics.csv",
        "leg_hourly.csv",
        "journeys.csv",
        "journey_mode_share.csv",
        "journey_summary.csv",
        "person_daily.csv",
        "daily_summary.csv",
        "legs.csv",
        "validation_summary.csv",
        "link_hourly_by_class.csv",
        "link_speed_by_class.csv",
        "cross_run_comparison.csv",
        "activity_durations.csv",
        "activity_patterns.csv",
        "activity_type_summary.csv",
        "activity_pattern_summary.csv",
        "zone_od.csv",
        "zone_flows.csv",
        "zone_summary.csv",
        "urban_area_summary.csv",
        "person_demographics.csv",
        "group_burdens.csv",
        "group_module_outcomes.csv",
        "equity_comparison.csv",
        "service_summary.csv",
        "service_vehicles.csv",
        "service_occupancy.csv",
        "noise_summary.csv",
        "transit_trips.csv",
        "transit_stop_hourly.csv",
        "transit_line_summary.csv",
        "transit_outcomes.csv",
        "transit_occupancy.csv",
        "transit_journeys.csv",
        "transit_availability.csv",
        "transit_validation_summary.csv",
        "transit_validation_matches.csv",
        "economic_appraisal.csv",
        "emissions_hourly.csv",
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
    // One table describes a metric, so every column of its key only has to appear in at least
    // one of the tables, not in each.
    for (name, key) in entries {
        for column in key.split(',') {
            assert!(
                headers
                    .iter()
                    .any(|header| header.iter().any(|exported| exported == column)),
                "{name} is keyed on {column}, which no table of {TABLES:?} exports"
            );
        }
    }
    // The capacity denominators the acceptance criteria call for are catalogued, and each
    // one is the column of link_capacity.csv that carries it.
    let capacity_header = &headers[TABLES
        .iter()
        .position(|file| *file == "link_capacity.csv")
        .expect("link_capacity.csv is one of the tables")];
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
            names.iter().any(|name| *name == expected),
            "{expected} is not catalogued"
        );
    }
    for name in [
        "capacity_pce_per_hour",
        "effective_capacity_pce",
        "entry_flow_pce_per_hour",
        "exit_flow_pce_per_hour",
        "entry_vc",
        "exit_vc",
    ] {
        assert!(
            capacity_header.iter().any(|column| column == name),
            "{name} is catalogued but is not a column of link_capacity.csv"
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
/// The recorded window opens at the start of the day in these fixtures, which is what makes
/// the first observed activity of a person left-censored.
const WINDOW_START_SECONDS: u32 = 0;

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
            capacity: None,
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
        WINDOW_START_SECONDS,
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
            ..Analysis::default()
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

    // Link speeds are reconstructed from the replayed events, and the PCE volumes and V/C
    // histograms from the same replay, so all of them have to agree across the event formats
    // just like the vehicle counts they are derived from.
    for table in [
        "link_hourly.csv",
        "coverage.csv",
        "link_capacity.csv",
        "vc_histogram.csv",
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

#[deterministic_id_test(rust_qsim)]
fn boundary_classification_counts_arithmetic_edge_coordinates_as_inside() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let events_dir = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events_dir).unwrap();
    fs::write(
        events_dir.join("events.0.xml"),
        "<events><event time=\"1\" type=\"entered link\" link=\"edge-link\" vehicle=\"v1\"/></events>",
    )
    .unwrap();

    // 0.1 + 0.2 is the documented-but-not-exactly-representable 0.30000000000000004,
    // so the node sits on the polygon's x = 0.3 edge only up to rounding.
    let edge = 0.1 + 0.2;
    assert_ne!(
        edge, 0.3,
        "the fixture must rely on rounding to be meaningful"
    );
    let on_edge = Node::new(Id::create("on-edge"), Coordinate::new_2d(edge, 0.5), 0, 1);
    let outside = Node::new(Id::create("outside"), Coordinate::new_2d(0.8, 0.5), 0, 1);
    let mut network = Network::new();
    network.add_node(on_edge.clone());
    network.add_node(outside.clone());
    network.add_link(Link::new_with_default(
        Id::create("edge-link"),
        &on_edge,
        &outside,
    ));
    let garage = Garage::default();
    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &AnalysisRunMetadata::from_run(
            1,
            1.0,
            0,
            &garage,
            Vec::new(),
            AnalysisInputPaths::default(),
        ),
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
            urban_boundary: Some(vec![[0.0, 0.0], [0.3, 0.0], [0.3, 1.0], [0.0, 1.0]]),
            ..Analysis::default()
        },
    )
    .unwrap();

    let classifications =
        fs::read_to_string(report.parent().unwrap().join("link_classification.csv")).unwrap();
    assert!(
        classifications.contains("\"edge-link\",\"cross_boundary\""),
        "an endpoint on the polygon edge counts as inside, so the link crosses: {classifications}"
    );
}

#[deterministic_id_test(rust_qsim)]
fn report_groups_coverage_by_explicit_labels_and_geographic_boundary() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let events_dir = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events_dir).unwrap();
    fs::write(
        events_dir.join("events.0.xml"),
        "<events><event time=\"1\" type=\"entered link\" link=\"inner-road\" vehicle=\"v1\"/><event time=\"1\" type=\"entered link\" link=\"outer-road\" vehicle=\"v2\"/><event time=\"1\" type=\"entered link\" link=\"outer-expressway\" vehicle=\"v3\"/></events>",
    )
    .unwrap();

    let nodes = [
        Node::new(Id::create("far"), Coordinate::new_2d(-2.0, 0.0), 0, 1),
        Node::new(Id::create("outside"), Coordinate::new_2d(-1.0, 0.0), 0, 1),
        Node::new(Id::create("inside-a"), Coordinate::new_2d(0.25, 0.25), 0, 1),
        Node::new(Id::create("inside-b"), Coordinate::new_2d(0.75, 0.75), 0, 1),
        Node::new(Id::create("farther"), Coordinate::new_2d(2.0, 0.0), 0, 1),
        Node::new(
            Id::create("left-cross"),
            Coordinate::new_2d(-1.0, 0.5),
            0,
            1,
        ),
        Node::new(
            Id::create("right-cross"),
            Coordinate::new_2d(2.0, 0.5),
            0,
            1,
        ),
        Node::new(Id::create("border"), Coordinate::new_2d(0.0, 0.5), 0, 1),
    ];
    let mut network = Network::new();
    for node in &nodes {
        network.add_node(node.clone());
    }
    for (id, from, to) in [
        ("outer-road", 0, 1),
        ("cross-road", 1, 2),
        ("inner-road", 2, 3),
        ("unknown-road", 3, 4),
        ("outer-expressway", 0, 1),
        ("through-expressway", 5, 6),
        ("border-road", 7, 2),
    ] {
        network.add_link(Link::new_with_default(
            Id::create(id),
            &nodes[from],
            &nodes[to],
        ));
    }
    let garage = Garage::default();
    let metadata = AnalysisRunMetadata::from_run(
        1,
        1.0,
        WINDOW_START_SECONDS,
        &garage,
        Vec::new(),
        AnalysisInputPaths::default(),
    );
    let mut labels: std::collections::BTreeMap<String, LinkLabels> = [
        ("outer-road", Some("other"), Some("__METRICS__")),
        ("cross-road", Some("expressway"), Some("large")),
        ("inner-road", Some("expressway"), Some("large")),
        ("outer-expressway", Some("expressway"), Some("large")),
        ("through-expressway", Some("expressway"), Some("large")),
        ("border-road", Some("other"), Some("medium")),
    ]
    .into_iter()
    .map(|(id, road_type, road_size)| {
        (
            id.to_owned(),
            LinkLabels {
                urban_area: None,
                road_type: road_type.map(str::to_owned),
                road_size: road_size.map(str::to_owned),
            },
        )
    })
    .collect();
    labels.insert(
        "unknown-road".to_owned(),
        LinkLabels {
            urban_area: Some("outer".to_owned()),
            road_type: Some(" ".to_owned()),
            road_size: Some("".to_owned()),
        },
    );

    let report = analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        // Two hourly intervals, so the fixed denominators can be compared across
        // hours while the used counts differ.
        7200,
        &metadata,
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
            link_labels: labels.clone(),
            urban_boundary: Some(vec![[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
            observed_data: None,
            journey_survey: None,
            comparison_runs: Vec::new(),
            service: None,
            transit_observed_data: None,
            economic_inputs: None,
            emissions: None,
            noise: None,
            excess_delay_clip_seconds: None,
            zone_system: Default::default(),
            person_group_attributes: Vec::new(),
            person_weight_attribute: None,
            person_cost_attribute: None,
            accessibility: Accessibility::default(),
        },
    )
    .unwrap();

    let report_dir = report.parent().unwrap();
    let classifications = fs::read_to_string(report_dir.join("link_classification.csv")).unwrap();
    assert!(classifications.contains("\"inner-road\",\"inner\",\"expressway\",\"large\""));
    assert!(classifications.contains("\"outer-road\",\"outer\",\"other\",\"__METRICS__\""));
    assert!(classifications.contains("\"cross-road\",\"cross_boundary\",\"expressway\",\"large\""));
    assert!(
        classifications
            .contains("\"through-expressway\",\"cross_boundary\",\"expressway\",\"large\"")
    );
    assert!(classifications.contains("\"border-road\",\"inner\",\"other\",\"medium\""));
    assert!(
        classifications.contains("\"unknown-road\",\"cross_boundary\",\"unknown\",\"unknown\"")
    );

    let groups = fs::read_to_string(report_dir.join("group_coverage.csv")).unwrap();
    assert!(groups.contains("\"urban_area\",\"inner\",0,2,1,1,50.000000"));
    assert!(groups.contains("\"urban_area\",\"outer\",0,2,2,0,100.000000"));
    assert!(groups.contains("\"urban_area\",\"cross_boundary\",0,3,0,3,0.000000"));
    assert!(groups.contains("\"road_type\",\"unknown\",0,1,0,1,0.000000"));
    assert!(groups.contains("\"road_type\",\"expressway\",0,4,2,2,50.000000"));
    // Eligible denominators stay fixed per category while the used count follows
    // the hourly events: the second interval saw no vehicle, so every group is
    // unused without its eligible count moving.
    for (category, eligible) in [
        ("\"urban_area\",\"inner\"", 2),
        ("\"urban_area\",\"outer\"", 2),
        ("\"urban_area\",\"cross_boundary\"", 3),
        ("\"road_type\",\"expressway\"", 4),
        ("\"road_type\",\"unknown\"", 1),
        ("\"road_size\",\"large\"", 4),
        ("\"road_size\",\"__METRICS__\"", 1),
    ] {
        assert!(
            groups.contains(&format!("{category},0,{eligible},")),
            "hour 0 group {category} should keep {eligible} eligible links"
        );
        assert!(
            groups.contains(&format!("{category},3600,{eligible},0,{eligible},0.000000")),
            "hour 3600 group {category} should keep the same {eligible} eligible links"
        );
    }
    let map = fs::read_to_string(report_dir.join("network_map.svg")).unwrap();
    assert!(map.contains("stroke=\"#287a3d\""));
    assert!(map.contains("stroke=\"#c8ccd0\""));
    assert!(map.contains("stroke-dasharray=\"8 3\""));
    // The report inlines the map so the classification filters can hide links.
    assert!(map.contains("data-road-type=\"expressway\""));
    assert!(map.contains("data-road-size=\"__METRICS__\""));
    let html = fs::read_to_string(report).unwrap();
    // The map is inlined, so this id reaches the report only if the SVG was
    // substituted in rather than merely written as the standalone export.
    assert!(html.contains("id=\"network-map\""));
    // FILTER_DIMENSIONS is the single dimension list, so the CSV columns, the
    // per-link map attributes and the report's own filter list must all agree.
    assert!(classifications.starts_with("link_id,\"urban_area\",\"road_type\",\"road_size\""));
    assert!(html.contains("[[\"urban_area\",\"Urban area\"],[\"road_type\",\"Road type\"],[\"road_size\",\"Road size\"]]"));
    for key in ["urban_area", "road_type", "road_size"] {
        let attribute = format!("data-{}=\"", key.replace('_', "-"));
        assert!(
            map.contains(&attribute),
            "map lines need one filter attribute per dimension: {attribute}"
        );
    }
    // Payload assertions: these strings exist only in generated data, so they fail
    // if substitution breaks or if a label collides with a template token.
    assert!(html.contains("group_used_link_percent"));
    assert!(html.contains("\"urban_area\":\"cross_boundary\""));
    assert!(html.contains("\"road_size\":\"__METRICS__\""));
    // The filter wiring is observable only as script source: the repo has no JS
    // runtime in the test harness, so these pin that the path stays connected.
    assert!(html.contains("row[key]===select.value"));
    assert!(html.contains("renderGroups(rows)"));
    assert!(html.contains("updateMap()"));
    // Each filter change re-renders these tables, so the helper must replace its
    // contents; appending would stack a new table under the previous one on every
    // interaction.
    assert!(
        html.contains("root.replaceChildren(t)"),
        "the table helper must replace, not append"
    );
    assert!(html.contains("document.querySelector('#metrics')"));
    assert!(html.contains("document.querySelector('#coverage')"));

    let explicitly_classified = analyze_final_iteration(
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
            link_labels: labels,
            urban_boundary: None,
            observed_data: None,
            journey_survey: None,
            comparison_runs: Vec::new(),
            service: None,
            transit_observed_data: None,
            economic_inputs: None,
            emissions: None,
            noise: None,
            excess_delay_clip_seconds: None,
            zone_system: Default::default(),
            person_group_attributes: Vec::new(),
            person_weight_attribute: None,
            person_cost_attribute: None,
            accessibility: Accessibility::default(),
        },
    )
    .unwrap();
    let classifications = fs::read_to_string(
        explicitly_classified
            .parent()
            .unwrap()
            .join("link_classification.csv"),
    )
    .unwrap();
    assert!(classifications.contains("\"unknown-road\",\"outer\",\"unknown\",\"unknown\""));

    for boundary in [
        vec![[0.0, 0.0], [1.0, 0.0]],
        vec![[0.0, 0.0], [1.0, 0.0], [f64::NAN, 1.0]],
    ] {
        let error = analyze_final_iteration(
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
                urban_boundary: Some(boundary),
                ..Analysis::default()
            },
        )
        .unwrap_err();
        assert!(error.to_string().contains("analysis.urban_boundary"));
    }
}

/// Publishes a report over an empty event stream, so every service table comes from the
/// supplied records alone. The network has one link inside the square service area, one that
/// leaves it, and one outside it.
fn service_report(output: &std::path::Path, service: Option<ServiceInputs>) -> PathBuf {
    let events = output.join("ITERS/it.0/events");
    fs::create_dir_all(&events).unwrap();
    fs::write(events.join("events.0.xml"), "<events></events>").unwrap();
    let node = |id: &str, x, y| Node::new(Id::create(id), Coordinate::new_2d(x, y), 0, 1);
    let (a, b, c, d) = (
        node("a", 0.0, 0.0),
        node("b", 1.0, 0.0),
        node("c", 10.0, 10.0),
        node("d", 11.0, 10.0),
    );
    let mut network = Network::new();
    for node in [&a, &b, &c, &d] {
        network.add_node(node.clone());
    }
    network.add_link(Link::new_with_default(Id::create("in"), &a, &b));
    network.add_link(Link::new_with_default(Id::create("leaves"), &b, &c));
    network.add_link(Link::new_with_default(Id::create("far"), &c, &d));
    analyze_final_iteration(
        output,
        0,
        1,
        CompressionType::None,
        3600,
        &run_metadata(1, 1.0, &Garage::default(), &[]),
        &network,
        &Analysis {
            enabled: true,
            interval_seconds: 3600,
            service,
            ..Analysis::default()
        },
    )
    .unwrap()
}

fn module_status(report_dir: &std::path::Path, module: &str) -> serde_json::Value {
    let statuses: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(report_dir.join("module_status.json")).unwrap())
            .unwrap();
    statuses
        .as_array()
        .unwrap()
        .iter()
        .find(|status| status["module"] == module)
        .unwrap()
        .clone()
}

#[deterministic_id_test(rust_qsim)]
fn service_performance_reports_outcomes_distance_and_constraints() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    fs::write(
        output.join("requests.csv"),
        "request_id,person_id,submission_seconds,origin_link,destination_link,status,group,direct_travel_seconds,party_size\n\
         r1,p1,0,in,in,,young,100,1\n\
         r2,p2,0,in,in,submitted,old,200,2\n\
         r3,p3,50,in,far,rejected,young,,\n\
         r4,p4,60,in,in,,old,,\n\
         r5,p5,70,far,far,,,,\n",
    )
    .unwrap();
    fs::write(
        output.join("passengers.csv"),
        "request_id,vehicle_id,pickup_seconds,dropoff_seconds\n\
         r1,v1,100,400\n\
         r2,v1,100,500\n\
         r5,v2,770,1000\n\
         r3,v2,300,400\n\
         ghost,v2,300,400\n",
    )
    .unwrap();
    fs::write(
        output.join("fleet.csv"),
        "vehicle_id,capacity,service_start_seconds,service_end_seconds\n\
         v1,3,0,1000\nv2,4,0,1000\nv3,2,0,1000\n",
    )
    .unwrap();
    // v1 relocates empty first and carries a three-passenger load in a shared ride.
    fs::write(
        output.join("schedule.csv"),
        "vehicle_id,task_type,start_seconds,end_seconds,distance_meters\n\
         v1,stay,0,50,\n\
         v1,drive,50,100,500\n\
         v1,stop,100,120,\n\
         v1,drive,120,400,3000\n\
         v1,stop,400,420,\n\
         v1,drive,420,500,1000\n\
         v1,drive,500,600,200\n\
         v2,drive,100,770,1000\n\
         v2,stop,770,780,\n\
         v2,drive,780,1000,2000\n\
         v3,stay,0,900,\n\
         v3,drive,900,950,\n\
         v3,drive,990,980,10\n",
    )
    .unwrap();

    let report = service_report(
        output,
        Some(ServiceInputs {
            requests: PathBuf::from("requests.csv"),
            passengers: Some(PathBuf::from("passengers.csv")),
            fleet: Some(PathBuf::from("fleet.csv")),
            schedule: Some(PathBuf::from("schedule.csv")),
            service_area: Some(vec![[-1.0, -1.0], [2.0, -1.0], [2.0, 2.0], [-1.0, 2.0]]),
            max_wait_seconds: Some(600.0),
        }),
    );
    let dir = report.parent().unwrap();
    let table = |name: &str| fs::read_to_string(dir.join(name)).unwrap();
    assert_eq!(
        module_status(dir, "service_performance")["status"],
        "complete"
    );

    let summary = table("service_summary.csv");
    // Only the request record rejects; the unmatched request is merely unserved.
    assert!(summary.contains("total,\"\",5,3,1,1,0.600000,0.200000,4,300.000000,282.842712,100.000000,700.000000,2.500000,0.500000,3.000000,3.000000,1,3,2,0,0.600000"));
    assert!(summary.contains("group,\"young\",2,1,1,0,0.500000,0.500000,1,100.000000,0.000000,100.000000,100.000000,3.000000,0.000000,3.000000,3.000000,0,1,1,0,0.500000"));
    assert!(summary.contains("group,\"old\",2,1,0,1,"));
    // A request without a label is kept visible as its own group.
    assert!(summary.contains("group,\"unknown\",1,1,0,0,1.000000,0.000000,1,700.000000,0.000000,700.000000,700.000000,,,,,1,0,1,0,0.000000"));

    let requests = table("service_requests.csv");
    assert!(requests.contains("\"r3\",\"p3\",\"young\",rejected,,50.000000,,,,,,,outside,"));
    assert!(requests.contains("\"r4\",\"p4\",\"old\",unserved,,60.000000,,,,,,,inside,"));
    assert!(requests.contains("\"r5\",\"p5\",\"unknown\",served,\"v2\",70.000000,770.000000,1000.000000,700.000000,230.000000,,,outside,true"));

    let vehicles = table("service_vehicles.csv");
    // Fleet totals come first. Empty relocation is the part of the drive with nobody on board.
    assert!(vehicles.contains("fleet,\"\",,3000.000000,1500.000000,0.500000,7700.000000,6000.000000,1700.000000,0.220779,13000.000000,1.688312,0.498084,0,3"));
    assert!(vehicles.contains("vehicle,\"v1\",3,1000.000000,550.000000,0.550000,4700.000000,4000.000000,700.000000,0.148936,11000.000000,2.340426,0.780142,0,2"));
    assert!(vehicles.contains("vehicle,\"v2\",4,1000.000000,900.000000,0.900000,3000.000000,2000.000000,1000.000000,0.333333,2000.000000,0.666667,0.166667,0,1"));
    // An idle vehicle has no distance, so the distance ratios stay blank rather than zero.
    assert!(vehicles.contains(
        "vehicle,\"v3\",2,1000.000000,50.000000,0.050000,0.000000,0.000000,0.000000,,0.000000,,,0,0"
    ));

    let occupancy = table("service_occupancy.csv");
    assert!(occupancy.contains("0,1700.000000,0.220779"));
    assert!(occupancy.contains("3,3000.000000,0.389610"));

    let constraints = table("service_constraints.csv");
    assert!(constraints.contains("service_area_polygon,\"-1 -1;2 -1;2 2;-1 2\""));
    assert!(constraints.contains("max_wait_seconds,\"600\""));
    assert!(constraints.contains("capacity_max,\"4\""));
    assert!(constraints.contains("capacity_total,\"9\""));

    let diagnostics = table("service_diagnostics.csv");
    assert!(diagnostics.contains("\"passengers\",5,\"rejected_request_has_passenger\",\"r3\""));
    assert!(diagnostics.contains("\"passengers\",6,\"unknown_request\",\"ghost\""));
    assert!(diagnostics.contains("\"schedule\",13,\"drive_distance_unavailable\",\"v3\""));
    assert!(diagnostics.contains("\"schedule\",14,\"end_before_start\",\"v3\""));
    assert!(
        table("service_availability.csv")
            .lines()
            .all(|line| !line.contains("unavailable"))
    );

    // The settings are recorded so a standalone reanalysis reads the same records.
    let manifest = table("manifest.json");
    assert!(manifest.contains("requests.csv") && manifest.contains("max_wait_seconds"));
    // The local report presents the tables, and each metric is catalogued.
    let html = fs::read_to_string(&report).unwrap();
    assert!(html.contains("id=\"service-summary\""));
    assert!(html.contains("group,\\\"young\\\",2,1,1,0,0.500000"));
    let catalog = table("metric_catalog.json");
    for metric in [
        "served_share",
        "wait_p90_seconds",
        "detour_mean_ratio",
        "empty_meters",
        "utilization",
        "load_factor",
        "coverage_share",
    ] {
        assert!(catalog.contains(metric), "missing {metric}");
    }
}

#[deterministic_id_test(rust_qsim)]
fn service_performance_leaves_metrics_without_inputs_unavailable() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();

    // Nothing configured: the module is unavailable and the tables are header-only.
    let dir = service_report(output, None);
    let dir = dir.parent().unwrap();
    let status = module_status(dir, "service_performance");
    assert_eq!(status["status"], "unavailable");
    assert_eq!(
        fs::read_to_string(dir.join("service_requests.csv"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    // Requests alone give rejections, but served counts need the passenger records and
    // nothing is guessed from them.
    fs::write(
        output.join("requests.csv"),
        "request_id,submission_seconds,origin_link,destination_link,status\n\
         r1,0,in,in,rejected\nr2,0,in,in,\n",
    )
    .unwrap();
    let service = ServiceInputs {
        requests: PathBuf::from("requests.csv"),
        passengers: None,
        fleet: None,
        schedule: None,
        service_area: None,
        max_wait_seconds: None,
    };
    let dir = service_report(output, Some(service.clone()));
    let dir = dir.parent().unwrap();
    assert_eq!(
        module_status(dir, "service_performance")["status"],
        "complete"
    );
    let summary = fs::read_to_string(dir.join("service_summary.csv")).unwrap();
    assert!(summary.contains("total,\"\",2,,1,,,0.500000,,,,,,,,,,,,,,"));
    let availability = fs::read_to_string(dir.join("service_availability.csv")).unwrap();
    assert!(availability.contains("request_outcomes,unavailable,\"missing input: passengers\""));
    assert!(
        availability
            .contains("utilization,unavailable,\"missing input: fleet service windows, schedule\"")
    );
    assert!(availability.contains("coverage,unavailable,\"missing input: service_area\""));

    // A missing request file fails only this module; the rest of the report is intact.
    let dir = service_report(
        output,
        Some(ServiceInputs {
            requests: PathBuf::from("absent.csv"),
            ..service
        }),
    );
    let dir = dir.parent().unwrap();
    let status = module_status(dir, "service_performance");
    assert_eq!(status["status"], "failed");
    assert!(status["reason"].as_str().unwrap().contains("absent.csv"));
    assert_eq!(module_status(dir, "link_coverage")["status"], "complete");
}

#[deterministic_id_test(rust_qsim)]
fn service_performance_rejects_invalid_inputs_in_its_own_module() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    let service = |requests: &str, fleet: Option<&str>| {
        fs::write(output.join("requests.csv"), requests).unwrap();
        fleet.map(|fleet| fs::write(output.join("fleet.csv"), fleet).unwrap());
        ServiceInputs {
            requests: PathBuf::from("requests.csv"),
            passengers: None,
            fleet: fleet.map(|_| PathBuf::from("fleet.csv")),
            schedule: None,
            service_area: None,
            max_wait_seconds: None,
        }
    };
    let header = "request_id,submission_seconds,origin_link,destination_link,status,party_size\n";
    let failed_reason = |service: ServiceInputs| {
        let dir = service_report(output, Some(service));
        let status = module_status(dir.parent().unwrap(), "service_performance");
        assert_eq!(status["status"], "failed");
        status["reason"].as_str().unwrap().to_owned()
    };
    assert!(
        failed_reason(service(&format!("{header}r1,NaN,in,in,,\n"), None)).contains("non-finite")
    );
    assert!(
        failed_reason(service(&format!("{header}r1,0,in,in,lost,\n"), None))
            .contains("unknown status")
    );
    assert!(
        failed_reason(service(
            &format!("{header}r1,0,in,in,,\nr1,1,in,in,,\n"),
            None
        ))
        .contains("repeats request_id")
    );
    assert!(
        failed_reason(service(&format!("{header}r1,0,in,in,,0\n"), None)).contains("party_size 0")
    );
    let ok = format!("{header}r1,0,in,in,,\n");
    assert!(
        failed_reason(service(
            &ok,
            Some("vehicle_id,capacity,service_start_seconds,service_end_seconds\nv1,2,0,inf\n")
        ))
        .contains("fleet row 2")
    );
    let mut bad_area = service(&ok, None);
    bad_area.service_area = Some(vec![[0.0, 0.0], [1.0, 0.0]]);
    assert!(failed_reason(bad_area).contains("service_area"));
    let mut bad_wait = service(&ok, None);
    bad_wait.max_wait_seconds = Some(-1.0);
    assert!(failed_reason(bad_wait).contains("max_wait_seconds"));
}

#[deterministic_id_test(rust_qsim)]
fn service_performance_flags_inconsistent_records_and_overloaded_vehicles() {
    let temp = tempfile::tempdir().unwrap();
    let output = temp.path();
    fs::write(
        output.join("requests.csv"),
        "request_id,submission_seconds,origin_link,destination_link,party_size\n\
         q1,100,in,in,2\nq2,0,in,missing-link,\nq3,0,in,in,\nq4,500,in,in,\nq5,0,in,in,\n",
    )
    .unwrap();
    fs::write(
        output.join("passengers.csv"),
        "request_id,vehicle_id,pickup_seconds,dropoff_seconds\n\
         q1,v1,200,600\nq3,v1,10,20\nq3,v1,30,40\nq4,v1,400,500\nq5,v1,300,200\nq2,vx,10,20\n",
    )
    .unwrap();
    fs::write(
        output.join("fleet.csv"),
        "vehicle_id,capacity,service_start_seconds,service_end_seconds\nv1,1,100,700\nv2,2,500,500\n",
    )
    .unwrap();
    // The second drive of v1 carries a party of two in a one-seat vehicle, and only half of the
    // first drive's busy time lies inside the service window.
    fs::write(
        output.join("schedule.csv"),
        "vehicle_id,task_type,start_seconds,end_seconds,distance_meters\n\
         v1,drive,0,300,1000\nv1,drive,250,550,2000\nv9,stop,0,10,\nv9,stop,20,30,\n",
    )
    .unwrap();
    let service = ServiceInputs {
        requests: PathBuf::from("requests.csv"),
        passengers: Some(PathBuf::from("passengers.csv")),
        fleet: Some(PathBuf::from("fleet.csv")),
        schedule: Some(PathBuf::from("schedule.csv")),
        service_area: Some(vec![[-1.0, -1.0], [2.0, -1.0], [2.0, 2.0], [-1.0, 2.0]]),
        max_wait_seconds: None,
    };
    let report = service_report(output, Some(service.clone()));
    let dir = report.parent().unwrap();
    let table = |name: &str| fs::read_to_string(dir.join(name)).unwrap();
    assert_eq!(
        module_status(dir, "service_performance")["status"],
        "complete"
    );

    let diagnostics = table("service_diagnostics.csv");
    for expected in [
        "\"passengers\",4,\"duplicate_association\",\"q3\"",
        "\"passengers\",5,\"pickup_before_submission\",\"q4\"",
        "\"passengers\",6,\"dropoff_before_pickup\",\"q5\"",
        "\"passengers\",7,\"unknown_vehicle\",\"vx\"",
        "\"fleet\",3,\"invalid_service_window\",\"v2\"",
        "\"schedule\",4,\"unknown_vehicle\",\"v9\"",
    ] {
        assert!(diagnostics.contains(expected), "missing {expected}");
    }
    // The unknown vehicle is reported once, not once per stop.
    assert_eq!(diagnostics.matches("\"v9\"").count(), 1);

    // A link missing from the network leaves the request outside any verdict, not outside.
    let summary = table("service_summary.csv");
    assert!(summary.contains(",4,0,1,0.800000"), "{summary}");
    assert!(table("service_requests.csv").contains("\"q2\",\"\",\"unknown\",served,\"vx\""));
    // Rejected-looking records stay unserved: only q1, q2 and q3 hold a valid association.
    assert!(summary.contains("total,\"\",5,3,0,2,"));

    // The party of two is 4000 passenger-metres in a one-seat vehicle, and only the window
    // overlap counts: 200 s of the first drive plus all 300 s of the second.
    let vehicles = table("service_vehicles.csv");
    assert!(vehicles.contains("vehicle,\"v1\",1,600.000000,600.000000,0.833333,3000.000000,2000.000000,1000.000000,0.333333,4000.000000,1.333333,1.333333,1,2"), "{vehicles}");
    let occupancy = table("service_occupancy.csv");
    assert!(occupancy.contains("0,1000.000000,0.333333"));
    assert!(occupancy.contains("2,2000.000000,0.666667"));

    // Without rider records every task would look empty, so no occupancy is reported at all.
    let without_passengers = service_report(
        output,
        Some(ServiceInputs {
            passengers: None,
            ..service.clone()
        }),
    );
    let dir = without_passengers.parent().unwrap();
    assert_eq!(
        fs::read_to_string(dir.join("service_occupancy.csv"))
            .unwrap()
            .lines()
            .count(),
        1
    );
    let availability = fs::read_to_string(dir.join("service_availability.csv")).unwrap();
    assert!(availability.contains("occupancy_distance,unavailable"));
    assert!(availability.contains("utilization,available,"));

    // Fleet records without any service window leave utilization unavailable.
    fs::write(output.join("fleet.csv"), "vehicle_id,capacity\nv1,1\n").unwrap();
    let dir = service_report(output, Some(service.clone()));
    let availability =
        fs::read_to_string(dir.parent().unwrap().join("service_availability.csv")).unwrap();
    assert!(
        availability.contains("utilization,unavailable,\"missing input: fleet service windows\"")
    );

    // A repeated vehicle or an unknown task type is an input error, not a silent guess.
    fs::write(
        output.join("fleet.csv"),
        "vehicle_id,capacity\nv1,1\nv1,2\n",
    )
    .unwrap();
    let dir = service_report(output, Some(service.clone()));
    let status = module_status(dir.parent().unwrap(), "service_performance");
    assert_eq!(status["status"], "failed");
    assert!(
        status["reason"]
            .as_str()
            .unwrap()
            .contains("repeats vehicle_id v1")
    );
    fs::write(
        output.join("schedule.csv"),
        "vehicle_id,task_type,start_seconds,end_seconds,distance_meters\nv1,fly,0,1,\n",
    )
    .unwrap();
    let dir = service_report(
        output,
        Some(ServiceInputs {
            fleet: None,
            ..service
        }),
    );
    let status = module_status(dir.parent().unwrap(), "service_performance");
    assert!(
        status["reason"]
            .as_str()
            .unwrap()
            .contains("unknown task_type")
    );
}
