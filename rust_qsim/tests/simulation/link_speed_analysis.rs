use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::{
    AnalysisInputPaths, AnalysisRunMetadata, analyze_final_iteration,
};
use rust_qsim::simulation::config::{
    Accessibility, Analysis, CommandLineArgs, CompressionType, Config,
};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::id::Id;
use rust_qsim::simulation::scenario::Coordinate;
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scenario::network::{Link, Network, Node};
use rust_qsim::simulation::scenario::vehicles::Garage;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

fn link_event(time: f64, event_type: &str, link: &str, vehicle: &str) -> String {
    format!("<event time=\"{time}\" type=\"{event_type}\" link=\"{link}\" vehicle=\"{vehicle}\"/>")
}

fn traffic_event(
    time: f64,
    event_type: &str,
    link: &str,
    vehicle: &str,
    relative_position: f64,
) -> String {
    format!(
        "<event time=\"{time}\" type=\"{event_type}\" person=\"person-{vehicle}\" link=\"{link}\" vehicle=\"{vehicle}\" networkMode=\"car\" relativePosition=\"{relative_position}\"/>"
    )
}

fn entered(time: f64, link: &str, vehicle: &str) -> String {
    link_event(time, "entered link", link, vehicle)
}

fn left(time: f64, link: &str, vehicle: &str) -> String {
    link_event(time, "left link", link, vehicle)
}

fn enters_traffic(time: f64, link: &str, vehicle: &str, relative_position: f64) -> String {
    traffic_event(
        time,
        "vehicle enters traffic",
        link,
        vehicle,
        relative_position,
    )
}

fn leaves_traffic(time: f64, link: &str, vehicle: &str, relative_position: f64) -> String {
    traffic_event(
        time,
        "vehicle leaves traffic",
        link,
        vehicle,
        relative_position,
    )
}

fn network_with_links(lengths: &[(&str, f64)]) -> Network {
    network_with_link_specs(
        &lengths
            .iter()
            .map(|(id, length)| (*id, *length, 10.0))
            .collect::<Vec<_>>(),
    )
}

fn network_with_link_specs(specs: &[(&str, f64, f64)]) -> Network {
    let from = Node::new(Id::create("from"), Coordinate::new_2d(0.0, 0.0), 0, 1);
    let to = Node::new(Id::create("to"), Coordinate::new_2d(1.0, 0.0), 0, 1);
    let mut network = Network::new();
    network.add_node(from.clone());
    network.add_node(to.clone());
    for (id, length, freespeed) in specs {
        let mut link = Link::new_with_default(Id::create(id), &from, &to);
        link.length = *length;
        link.freespeed = *freespeed;
        network.add_link(link);
    }
    network
}

fn write_partitions(output: &Path, iteration: u32, partitions: &[Vec<String>]) {
    let events = output
        .join("ITERS")
        .join(format!("it.{iteration}"))
        .join("events");
    fs::create_dir_all(&events).unwrap();
    for (rank, partition) in partitions.iter().enumerate() {
        fs::write(
            events.join(format!("events.{rank}.xml")),
            format!("<events>{}</events>", partition.join("")),
        )
        .unwrap();
    }
}

/// Runs the shared analysis interface and returns the report tables by file name.
fn analyze(
    output: &Path,
    iteration: u32,
    partitions: u32,
    simulation_end_time: u32,
    interval_seconds: u32,
    network: &Network,
) -> BTreeMap<String, String> {
    analyze_with_clip(
        output,
        iteration,
        partitions,
        simulation_end_time,
        interval_seconds,
        network,
        None,
    )
}

fn analyze_with_clip(
    output: &Path,
    iteration: u32,
    partitions: u32,
    simulation_end_time: u32,
    interval_seconds: u32,
    network: &Network,
    clip_delay: Option<f64>,
) -> BTreeMap<String, String> {
    let garage = Garage::default();
    let metadata = AnalysisRunMetadata::from_run(
        4711,
        // An unsampled run; the speed tables do not depend on the fraction.
        1.0,
        &garage,
        Vec::new(),
        AnalysisInputPaths {
            network: None,
            network_file: None,
            population: None,
            vehicles: None,
        },
    );
    let report = analyze_final_iteration(
        output,
        iteration,
        partitions,
        CompressionType::None,
        simulation_end_time,
        &metadata,
        network,
        &Analysis {
            enabled: true,
            interval_seconds,
            link_labels: BTreeMap::new(),
            urban_boundary: None,
            observed_data: None,
            comparison_runs: Vec::new(),
            excess_delay_clip_seconds: clip_delay,
            accessibility: Accessibility::default(),
        },
    )
    .unwrap();
    let report = report.parent().unwrap().to_owned();
    [
        "link_hourly.csv",
        "link_speed_hourly.csv",
        "link_speed_summary.csv",
        "link_speed_histogram.csv",
        "link_speed_diagnostics.csv",
        "network_distance_time.csv",
        "network_distance_time_summary.csv",
        "network_distance_time_diagnostics.csv",
        "en_route_agents.csv",
        "metric_catalog.json",
        "module_status.json",
        "index.html",
    ]
    .iter()
    .map(|name| {
        (
            (*name).to_owned(),
            fs::read_to_string(report.join(name)).unwrap(),
        )
    })
    .collect()
}

#[deterministic_id_test(rust_qsim)]
fn network_distance_time_conserves_partial_and_cross_interval_traversals() {
    let temp = tempfile::tempdir().unwrap();
    let network = network_with_link_specs(&[
        ("alpha", 100.0, 10.0),
        ("beta", 100.0, 10.0),
        ("instant", 50.0, 10.0),
        ("broken", 40.0, f64::NAN),
    ]);
    write_partitions(
        temp.path(),
        0,
        &[vec![
            "<event time=\"55\" type=\"departure\" person=\"traveler\" link=\"alpha\" legMode=\"walk\" computationalRoutingMode=\"walk\"/>".to_owned(),
            enters_traffic(55.0, "alpha", "partial", 0.5),
            "<event time=\"65\" type=\"arrival\" person=\"traveler\" link=\"alpha\" legMode=\"walk\"/>".to_owned(),
            leaves_traffic(65.0, "alpha", "partial", 0.75),
            entered(70.0, "alpha", "full"),
            enters_traffic(70.0, "broken", "broken", 0.0),
            left(75.0, "alpha", "full"),
            entered(75.0, "alpha", "same-link"),
            leaves_traffic(75.0, "broken", "broken", 1.0),
            left(80.0, "alpha", "same-link"),
            entered(80.0, "alpha", "slow"),
            entered(80.0, "alpha", "unfinished"),
            entered(80.0, "beta", "slow-beta"),
            left(95.0, "alpha", "slow"),
            left(95.0, "beta", "slow-beta"),
            entered(96.0, "instant", "instant"),
            left(96.0, "instant", "instant"),
        ]],
    );
    let tables = analyze_with_clip(temp.path(), 0, 1, 120, 60, &network, Some(2.0));
    let link_rows = rows(
        &tables["network_distance_time.csv"],
        "link_id,hour_start_seconds,vehicle_traversals,partial_traversals,vehicle_distance_meters,vehicle_time_seconds,free_flow_relative_delay_seconds,relative_speed_ratio,clipped_excess_delay_seconds,passenger_distance_meters,passenger_time_seconds,passenger_data_status",
    );
    assert_row(
        &link_rows,
        "\"alpha\",0,1,1,25.000000,10.000000,7.500000,0.250000,2.000000,,,unavailable",
    );
    assert_row(
        &link_rows,
        "\"alpha\",60,3,0,300.000000,25.000000,-5.000000,1.200000,2.000000,,,unavailable",
    );
    assert_row(
        &link_rows,
        "\"beta\",60,1,0,100.000000,15.000000,5.000000,0.666667,2.000000,,,unavailable",
    );
    assert_row(
        &link_rows,
        "\"broken\",60,1,0,40.000000,5.000000,,,,,,unavailable",
    );
    assert_row(
        &link_rows,
        "\"instant\",60,1,0,50.000000,0.000000,-5.000000,,0.000000,,,unavailable",
    );
    let summary_rows = rows(
        &tables["network_distance_time_summary.csv"],
        "hour_start_seconds,vehicle_traversals,vehicle_distance_meters,vehicle_time_seconds,free_flow_relative_delay_seconds,relative_speed_ratio,network_clipped_excess_delay_seconds,passenger_distance_meters,passenger_time_seconds,passenger_data_status",
    );
    assert_row(
        &summary_rows,
        "0,1,25.000000,10.000000,7.500000,0.250000,2.000000,,,unavailable",
    );
    assert_row(
        &summary_rows,
        "60,6,490.000000,45.000000,-5.000000,1.000000,4.000000,,,unavailable",
    );
    assert!(tables["network_distance_time_diagnostics.csv"].contains("unfinished_traversals,1"));
    assert!(tables["network_distance_time_diagnostics.csv"].contains("invalid_reference_speeds,1"));
    assert!(tables["network_distance_time_diagnostics.csv"].contains("non_positive_durations,1"));
    assert!(tables["en_route_agents.csv"].contains("0,1,0,0,0,1,5.000000"));
    assert!(tables["en_route_agents.csv"].contains("60,0,1,0,1,1,5.000000"));
    assert!(tables["index.html"].contains("Network distance, time and congestion"));
    assert!(tables["index.html"].contains("Peak interval by total signed free-flow delay"));
    assert!(tables["index.html"].contains("En-route agent profile"));
    assert!(tables["index.html"].contains("Traversal exclusions"));
    assert!(tables["index.html"].contains("Lowest relative-speed interval"));
    assert!(tables["module_status.json"].contains("network_distance_time"));
    assert!(tables["metric_catalog.json"].contains("relative_speed_ratio"));
    assert!(tables["metric_catalog.json"].contains("network_clipped_excess_delay_seconds"));
    assert!(tables["metric_catalog.json"].contains("passenger_distance_meters"));
    assert!(tables["metric_catalog.json"].contains("invalid_reference_speeds"));
}

fn rows<'a>(table: &'a str, header: &str) -> Vec<&'a str> {
    let mut lines = table.lines();
    assert_eq!(lines.next(), Some(header), "unexpected table header");
    lines.collect()
}

fn assert_row(table: &[&str], expected: &str) {
    assert!(
        table.contains(&expected),
        "missing row {expected}; rows: {table:?}"
    );
}

#[deterministic_id_test(rust_qsim)]
fn link_speed_reports_representative_and_vehicle_speed_metrics() {
    let temp = tempfile::tempdir().unwrap();
    let network = network_with_links(&[
        ("alpha", 100.0),
        ("beta", 300.0),
        // A link with a length that cannot produce a speed.
        ("broken", f64::NAN),
        ("delta", 1_000.0),
        ("gamma", 60.0),
        // A link that is long enough to make the speed overflow for a very short traversal.
        ("huge", f64::MAX),
    ]);

    let events = vec![
        // Two vehicles enter alpha at the same time, so 100 m in 10 s and in 20 s give single
        // traversal speeds of 10 m/s and 5 m/s, a mean of 7.5 m/s and a population deviation of
        // 2.5 m/s, while 200 m in 30 s is a representative speed of 6.666667 m/s.
        entered(10.0, "alpha", "veh-1"),
        entered(10.0, "alpha", "veh-2"),
        left(20.0, "alpha", "veh-1"),
        left(30.0, "alpha", "veh-2"),
        // A traversal that crosses the hour boundary is reported in its entry hour.
        entered(3590.0, "beta", "veh-3"),
        entered(3600.0, "gamma", "veh-4"),
        entered(3600.0, "gamma", "veh-5"),
        left(3610.0, "beta", "veh-3"),
        left(3620.0, "gamma", "veh-5"),
        left(3630.0, "gamma", "veh-4"),
        // 1000 m in 10 s is 100 m/s, which is above the last histogram bin.
        entered(3600.0, "delta", "veh-6"),
        left(3610.0, "delta", "veh-6"),
        // A vehicle that visits the same link twice, with another link in between, contributes
        // two observations to both links.
        entered(7300.0, "beta", "veh-1"),
        left(7310.0, "beta", "veh-1"),
        entered(7310.0, "gamma", "veh-1"),
        left(7340.0, "gamma", "veh-1"),
        entered(7340.0, "beta", "veh-1"),
        left(7380.0, "beta", "veh-1"),
        // A leg that starts and ends on the same link reports neither `entered link` nor
        // `left link`, so the departure and the arrival have to bound the traversal. 60 m in 30 s
        // is again 2 m/s, while a departure at the end of that link covers no distance at all.
        enters_traffic(7400.0, "gamma", "veh-28", 0.0),
        leaves_traffic(7430.0, "gamma", "veh-28", 1.0),
        enters_traffic(7440.0, "gamma", "veh-29", 1.0),
        leaves_traffic(7450.0, "gamma", "veh-29", 1.0),
        // A speed of exactly 50 m/s belongs to the overflow bin, a speed of exactly 5 m/s to the
        // bin that starts at 5 m/s.
        entered(10800.0, "gamma", "veh-7"),
        left(10812.0, "gamma", "veh-7"),
        entered(10801.0, "delta", "veh-8"),
        left(10821.0, "delta", "veh-8"),
        entered(10840.0, "delta", "veh-9"),
        left(10841.0, "delta", "veh-9"),
        // A departure in the middle of a link, an arrival in the middle of a link and a
        // departure at the end of the start link all cover less than a full link.
        enters_traffic(11500.0, "alpha", "veh-20", 0.5),
        left(11520.0, "alpha", "veh-20"),
        entered(11530.0, "beta", "veh-21"),
        leaves_traffic(11550.0, "beta", "veh-21", 0.5),
        enters_traffic(11600.0, "gamma", "veh-22", 1.0),
        left(11610.0, "gamma", "veh-22"),
        // A traversal that is still open when the events end, a leave without an entry and a
        // traversal without a positive duration.
        entered(11630.0, "delta", "veh-23"),
        left(11700.0, "alpha", "veh-24"),
        entered(11730.0, "beta", "veh-25"),
        left(11730.0, "beta", "veh-25"),
        // A link length that is not finite and a speed that is not finite.
        entered(11740.0, "broken", "veh-26"),
        left(11750.0, "broken", "veh-26"),
        entered(11800.0, "huge", "veh-27"),
        left(11800.5, "huge", "veh-27"),
    ];
    write_partitions(temp.path(), 4, &[events]);
    let tables = analyze(temp.path(), 4, 1, 18_000, 3600, &network);
    let distance_rows = rows(
        &tables["network_distance_time.csv"],
        "link_id,hour_start_seconds,vehicle_traversals,partial_traversals,vehicle_distance_meters,vehicle_time_seconds,free_flow_relative_delay_seconds,relative_speed_ratio,passenger_distance_meters,passenger_time_seconds,passenger_data_status",
    );
    // Includes one ordinary traversal and two visits that start and end on gamma.
    assert_row(
        &distance_rows,
        "\"gamma\",7200,3,1,120.000000,70.000000,58.000000,0.171429,,,unavailable",
    );
    assert!(
        !tables["network_distance_time.csv"]
            .lines()
            .next()
            .unwrap()
            .contains("clipped_excess_delay_seconds")
    );
    assert!(!tables["metric_catalog.json"].contains("clipped_excess_delay_seconds"));

    let speeds = rows(
        &tables["link_speed_hourly.csv"],
        "link_id,hour_start_seconds,observations,total_distance_meters,total_duration_seconds,representative_speed_mps,vehicle_speed_mean_mps,vehicle_speed_population_std_mps",
    );
    assert_row(
        &speeds,
        "\"alpha\",0,2,200.000000,30.000000,6.666667,7.500000,2.500000",
    );
    assert_row(
        &speeds,
        "\"beta\",0,1,300.000000,20.000000,15.000000,15.000000,0.000000",
    );
    // The traversal that entered beta in hour 0 is not repeated in hour 3600.
    assert_row(&speeds, "\"beta\",3600,0,0.000000,0.000000,,,");
    assert_row(
        &speeds,
        "\"gamma\",3600,2,120.000000,50.000000,2.400000,2.500000,0.500000",
    );
    assert_row(
        &speeds,
        "\"delta\",3600,1,1000.000000,10.000000,100.000000,100.000000,0.000000",
    );
    // A group with a single observation has a population standard deviation of zero.
    assert_row(
        &speeds,
        "\"beta\",7200,2,600.000000,50.000000,12.000000,18.750000,11.250000",
    );
    assert_row(
        &speeds,
        "\"gamma\",7200,2,120.000000,60.000000,2.000000,2.000000,0.000000",
    );
    // 50 m/s and 1000 m/s average to 95.238095 m/s, the two single speeds deviate by 475 m/s.
    assert_row(
        &speeds,
        "\"delta\",10800,2,2000.000000,21.000000,95.238095,525.000000,475.000000",
    );
    // A link without a full-link traversal keeps its speed unavailable.
    assert_row(&speeds, "\"broken\",0,0,0.000000,0.000000,,,");
    assert_row(&speeds, "\"huge\",0,0,0.000000,0.000000,,,");
    assert_row(&speeds, "\"alpha\",10800,0,0.000000,0.000000,,,");
    assert!(
        !speeds
            .iter()
            .any(|row| row.contains("NaN") || row.contains("inf"))
    );

    let summary = rows(
        &tables["link_speed_summary.csv"],
        "hour_start_seconds,links_with_speed,observations,mean_link_speed_mps,population_std_link_speed_mps",
    );
    // 6.666667 and 15 m/s average to 10.833333 m/s and deviate by 4.166667 m/s.
    assert_row(&summary, "0,2,3,10.833333,4.166667");
    assert_row(&summary, "3600,2,3,51.200000,48.800000");
    assert_row(&summary, "7200,2,4,7.000000,5.000000");
    assert_row(&summary, "10800,2,3,50.119048,45.119048");
    // Every interval of the simulated day is present, and an interval without a full-link
    // traversal has no across-link speed.
    assert_eq!(summary.len(), 5);
    assert_row(&summary, "14400,0,0,,");

    let histogram = rows(
        &tables["link_speed_histogram.csv"],
        "hour_start_seconds,bin_index,bin_lower_mps,bin_upper_mps,link_count,observation_count",
    );
    assert_row(&histogram, "0,1,5.000000,10.000000,1,2");
    assert_row(&histogram, "0,3,15.000000,20.000000,1,1");
    assert_row(&histogram, "0,10,50.000000,,0,0");
    assert_row(&histogram, "3600,0,0.000000,5.000000,1,2");
    assert_row(&histogram, "3600,10,50.000000,,1,1");
    assert_row(&histogram, "7200,0,0.000000,5.000000,1,2");
    assert_row(&histogram, "7200,2,10.000000,15.000000,1,2");
    assert_row(&histogram, "10800,1,5.000000,10.000000,1,1");
    assert_row(&histogram, "10800,10,50.000000,,1,2");
    // Every interval reports every fixed bin plus the overflow bin.
    assert_eq!(histogram.len(), 5 * 11);
    assert_row(&histogram, "14400,0,0.000000,5.000000,0,0");

    let diagnostics = rows(&tables["link_speed_diagnostics.csv"], "metric,count");
    assert_row(&diagnostics, "full_link_traversals,13");
    assert_row(&diagnostics, "partial_link_traversals,4");
    assert_row(&diagnostics, "unfinished_traversals,1");
    assert_row(&diagnostics, "unmatched_leave_events,1");
    assert_row(&diagnostics, "non_positive_duration_traversals,1");
    assert_row(&diagnostics, "invalid_link_length_traversals,1");
    assert_row(&diagnostics, "non_finite_speed_traversals,1");

    let status = &tables["module_status.json"];
    assert!(status.contains("\"module\": \"link_speed\""), "{status}");
    assert!(status.contains("\"status\": \"complete\""), "{status}");
    let catalog = &tables["metric_catalog.json"];
    assert!(
        catalog.contains("\"name\": \"link_representative_speed\""),
        "{catalog}"
    );
    assert!(catalog.contains("\"unit\": \"m/s\""), "{catalog}");
    let html = &tables["index.html"];
    assert!(html.contains("Interval link speeds"), "{html}");
    assert!(
        html.contains("const linkSpeeds=[\"link_id,hour_start_seconds,observations"),
        "{html}"
    );
    assert!(html.contains("link_speed_histogram.csv"), "{html}");
}

#[deterministic_id_test(rust_qsim)]
fn link_speed_follows_the_configured_analysis_interval() {
    let temp = tempfile::tempdir().unwrap();
    let network = network_with_links(&[("a", 100.0), ("b", 50.0)]);
    let events = vec![
        // 100 m in 20 s is 5 m/s, reported in the interval that contains the entry.
        entered(1750.0, "a", "veh-1"),
        // A traversal that crosses an interval boundary stays in the interval it entered.
        entered(1790.0, "b", "veh-1"),
        left(1770.0, "a", "veh-1"),
        left(1810.0, "b", "veh-1"),
        // 50 m in 10 s is 5 m/s.
        entered(3559.0, "b", "veh-2"),
        left(3569.0, "b", "veh-2"),
    ];
    write_partitions(temp.path(), 1, &[events]);
    // 15-minute intervals: the traversal that entered at 1790 belongs to the interval starting at
    // 900 even though it leaves in the next one, and the one that entered at 3559 belongs to 2700.
    let tables = analyze(temp.path(), 1, 1, 3600, 900, &network);

    let speeds = rows(
        &tables["link_speed_hourly.csv"],
        "link_id,hour_start_seconds,observations,total_distance_meters,total_duration_seconds,representative_speed_mps,vehicle_speed_mean_mps,vehicle_speed_population_std_mps",
    );
    assert_row(
        &speeds,
        "\"a\",900,1,100.000000,20.000000,5.000000,5.000000,0.000000",
    );
    assert_row(
        &speeds,
        "\"b\",900,1,50.000000,20.000000,2.500000,2.500000,0.000000",
    );
    assert_row(&speeds, "\"b\",1800,0,0.000000,0.000000,,,");
    assert_row(
        &speeds,
        "\"b\",2700,1,50.000000,10.000000,5.000000,5.000000,0.000000",
    );
    // The intervals of the simulated day are reported, including the ones without an observation.
    let summary = rows(
        &tables["link_speed_summary.csv"],
        "hour_start_seconds,links_with_speed,observations,mean_link_speed_mps,population_std_link_speed_mps",
    );
    assert_eq!(summary.len(), 4);
    assert_row(&summary, "0,0,0,,");
    assert_row(&summary, "900,2,2,3.750000,1.250000");
    assert_row(&summary, "1800,0,0,,");
    assert_row(&summary, "2700,1,1,5.000000,0.000000");
}

#[deterministic_id_test(rust_qsim)]
fn link_speed_survives_partition_handovers_at_the_same_timestamp() {
    let temp = tempfile::tempdir().unwrap();
    let network = network_with_links(&[("a", 120.0), ("b", 40.0)]);
    // A hand-over at the same timestamp: the enter of b and the leave of a carry the same time, so
    // the rank order decides which one the replay sees first.
    let in_order = vec![
        entered(100.0, "a", "veh-1"),
        left(110.0, "a", "veh-1"),
        entered(110.0, "b", "veh-1"),
        left(130.0, "b", "veh-1"),
        entered(200.0, "a", "veh-2"),
        left(260.0, "a", "veh-2"),
    ];
    write_partitions(temp.path(), 2, std::slice::from_ref(&in_order));
    let single = analyze(temp.path(), 2, 1, 3600, 3600, &network);

    // The receiving partition reports the enter of b first and the departing partition the leave
    // of a afterwards, which reverses the order of the two simultaneous events.
    let split = vec![
        vec![
            in_order[0].clone(),
            in_order[2].clone(),
            in_order[3].clone(),
        ],
        vec![
            in_order[1].clone(),
            in_order[4].clone(),
            in_order[5].clone(),
        ],
    ];
    write_partitions(temp.path(), 3, &split);
    let two_partitions = analyze(temp.path(), 3, 2, 3600, 3600, &network);

    for name in [
        "link_hourly.csv",
        "link_speed_hourly.csv",
        "link_speed_summary.csv",
        "link_speed_histogram.csv",
        "link_speed_diagnostics.csv",
    ] {
        assert_eq!(single[name], two_partitions[name], "table {name} differs");
    }
    // 120 m in 10 s and in 60 s give 12 m/s and 2 m/s, so the mean is 7 m/s, the population
    // deviation 5 m/s and the representative speed of 240 m in 70 s is 3.428571 m/s.
    assert!(
        single["link_speed_hourly.csv"]
            .contains("\"a\",0,2,240.000000,70.000000,3.428571,7.000000,5.000000")
    );
    assert!(
        single["link_speed_hourly.csv"]
            .contains("\"b\",0,1,40.000000,20.000000,2.000000,2.000000,0.000000")
    );
}

#[deterministic_id_test(rust_qsim)]
fn simulation_reports_link_speeds_of_the_final_iteration() {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/3-links/3-links-config-1.yml",
    ));
    config.output_mut().output_dir = "./test_output/simulation/link_speed_report".into();
    config.output_mut().analysis.enabled = true;
    let output = config.output().output_dir.clone();
    let controller = ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap();
    controller.run();

    let report = output.join("analysis");
    let hour = 32_400;
    let speeds = fs::read_to_string(report.join("link_speed_hourly.csv")).unwrap();
    // The vehicle enters link1 at the end of the link, so the first link has no full traversal,
    // while link2 (1000 m in 101 s) and link3 (100 m in 10 s) are traversed completely.
    assert!(
        speeds.contains(&format!("\"link1\",{hour},0,0.000000,0.000000,,,")),
        "{speeds}"
    );
    assert!(
        speeds.contains(&format!(
            "\"link2\",{hour},1,1000.000000,101.000000,9.900990,9.900990,0.000000"
        )),
        "{speeds}"
    );
    assert!(
        speeds.contains(&format!(
            "\"link3\",{hour},1,100.000000,10.000000,10.000000,10.000000,0.000000"
        )),
        "{speeds}"
    );

    let summary = fs::read_to_string(report.join("link_speed_summary.csv")).unwrap();
    // 9.900990 and 10 m/s average to 9.950495 m/s and deviate by 0.049505 m/s.
    assert!(
        summary.contains(&format!("{hour},2,2,9.950495,0.049505")),
        "{summary}"
    );

    let diagnostics = fs::read_to_string(report.join("link_speed_diagnostics.csv")).unwrap();
    assert!(
        diagnostics.contains("full_link_traversals,2"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("partial_link_traversals,1"),
        "{diagnostics}"
    );
    assert!(
        diagnostics.contains("unfinished_traversals,0"),
        "{diagnostics}"
    );
}
