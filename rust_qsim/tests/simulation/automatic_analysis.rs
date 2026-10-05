use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration,
};
use rust_qsim::simulation::config::{
    Analysis, CommandLineArgs, CompressionType, Config, LinkLabels,
};
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
            ..Analysis::default()
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
    // The report embeds the hourly rows and the coverage CSV verbatim; assert the
    // payload's columns and values rather than a bare variable declaration.
    assert!(html.contains(
        "\"link_id\":\"used\",\"hour_start_seconds\":3600,\"entry_vehicles\":1,\"exit_vehicles\":1"
    ));
    assert!(
        html.contains(
            "[\"hour_start_seconds,eligible_links,used_links,unused_links,used_percent\","
        )
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
        fs::read_to_string(output.join("analysis/link_hourly.csv")).unwrap()
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
        &AnalysisRunMetadata::from_run(1, &garage, Vec::new(), AnalysisInputPaths::default()),
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
    let metadata =
        AnalysisRunMetadata::from_run(1, &garage, Vec::new(), AnalysisInputPaths::default());
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
