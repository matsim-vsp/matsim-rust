use macros::deterministic_id_test;
use rust_qsim::simulation::analysis::reanalyze_completed_run;
use rust_qsim::simulation::config::{CommandLineArgs, Config};
use rust_qsim::simulation::controller::controller::ControllerBuilder;
use rust_qsim::simulation::id::{self, Id};
use rust_qsim::simulation::scenario::Scenario;
use rust_qsim::simulation::scenario::network::Link;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Run the real simulation command so the recorded analysis artifacts exist on disk.
fn run_simulation(config_path: &str, output_dir: &str) -> std::path::PathBuf {
    let mut config = Config::from_args(CommandLineArgs::new_with_path(config_path));
    config.controller_mut().last_iteration = 1;
    config.output_mut().output_dir = output_dir.into();
    config.output_mut().analysis.enabled = true;
    let output = config.output().output_dir.clone();
    let controller = ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap();
    controller.run();
    output
}

/// Every file below `dir` paired with its bytes, so a rerun can prove nothing else moved.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, files: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(&path, root, files);
            } else {
                files.push((
                    path.strip_prefix(root).unwrap().display().to_string(),
                    fs::read(&path).unwrap(),
                ));
            }
        }
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files);
    files
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_matches_automatic_metrics_and_preserves_raw_outputs() {
    // A sampled run, so the capacity table actually depends on the recorded sample size.
    // At a full sample the scaling is the identity and a lost value would go unnoticed.
    let mut config = Config::from_args(CommandLineArgs::new_with_path(
        "./tests/resources/3-links/3-links-config-1.yml",
    ));
    config.controller_mut().last_iteration = 1;
    config.output_mut().output_dir = "./test_output/simulation/standalone_equivalence".into();
    config.output_mut().analysis.enabled = true;
    config.qsim_mut().sample_size = 0.25;
    let output = config.output().output_dir.clone();
    ControllerBuilder::default_with_scenario(Scenario::load(config))
        .build()
        .unwrap()
        .run();
    let report_dir = output.join("analysis");
    // The capacity tables are included because they depend on the recorded sample size
    // and vehicle/PCE catalog, so a rerun that lost either would silently differ.
    let tables = [
        "link_hourly.csv",
        "coverage.csv",
        "link_capacity.csv",
        "vc_histogram.csv",
    ];
    let automatic: Vec<String> = tables
        .iter()
        .map(|table| fs::read_to_string(report_dir.join(table)).unwrap())
        .collect();

    let raw_before = snapshot(&output.join("ITERS"));
    let network_before = fs::read(output.join("output_network.xml.zst")).unwrap();
    let runtime_file = report_dir.join("runtime_metadata.json");
    let runtime_before: serde_json::Value =
        serde_json::from_slice(&fs::read(&runtime_file).unwrap()).unwrap();
    let report = reanalyze_completed_run(&output, None).unwrap();
    assert_eq!(report, report_dir.join("index.html"));
    // The same recorded iteration, metadata and interval produce the automatic run's metrics.
    for (table, expected) in tables.iter().zip(&automatic) {
        assert_eq!(
            &fs::read_to_string(report_dir.join(table)).unwrap(),
            expected,
            "{table} differs after a standalone rerun"
        );
    }
    // The recorded sample size really is what the capacity table scaled by, so the
    // equivalence above is not passing by accident on a full sample.
    let capacity = fs::read_to_string(report_dir.join("link_capacity.csv")).unwrap();
    assert!(
        capacity.contains(",0.250000,"),
        "sample size is not exported"
    );
    let scaled = capacity
        .lines()
        .skip(1)
        .filter(|line| line.contains(",0.250000,"))
        .count();
    assert!(scaled > 0, "no interval carries the sampled scaling");
    assert_eq!(snapshot(&output.join("ITERS")), raw_before);
    assert_eq!(
        fs::read(output.join("output_network.xml.zst")).unwrap(),
        network_before
    );
    let runtime_after: serde_json::Value =
        serde_json::from_slice(&fs::read(&runtime_file).unwrap()).unwrap();
    assert_eq!(
        runtime_after["simulation_seconds"],
        runtime_before["simulation_seconds"]
    );
    assert_eq!(
        runtime_after["worker_count"],
        runtime_before["worker_count"]
    );
    assert_eq!(
        runtime_after["peak_memory_bytes"],
        runtime_before["peak_memory_bytes"]
    );
    assert!(runtime_after["analysis_seconds"].as_f64().is_some());
    let runtime_csv = fs::read_to_string(report_dir.join("runtime.csv")).unwrap();
    assert!(runtime_csv.contains("\"analysis_runtime\""));
    assert_eq!(
        runtime_csv.contains("\"peak_memory\""),
        runtime_after["peak_memory_bytes"].as_u64().is_some()
    );
    assert!(
        fs::read_to_string(&report)
            .unwrap()
            .contains("analysis_runtime")
    );

    // Older reports have no measured simulation context; reanalysis must leave it unknown.
    fs::remove_file(&runtime_file).unwrap();
    reanalyze_completed_run(&output, None).unwrap();
    let legacy_runtime: serde_json::Value =
        serde_json::from_slice(&fs::read(&runtime_file).unwrap()).unwrap();
    assert!(legacy_runtime["network_links"].is_null());
    assert!(legacy_runtime["software_version"].is_null());
    assert!(
        !fs::read_to_string(report_dir.join("runtime.csv"))
            .unwrap()
            .contains("network_links")
    );
    assert!(!output.join(".analysis-staging").exists());
    assert!(!output.join(".analysis-backup").exists());
    assert!(!output.join("analysis-failure").exists());
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_applies_changed_settings_without_rerunning_qsim() {
    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_settings",
    );
    let report_dir = output.join("analysis");
    assert!(
        fs::read_to_string(report_dir.join("coverage.csv"))
            .unwrap()
            .contains("\n3600,")
    );

    reanalyze_completed_run(&output, Some(900)).unwrap();
    let manifest = fs::read_to_string(report_dir.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"interval_seconds\": 900"));
    assert!(manifest.contains("\"iteration\": 1"));
    let coverage = fs::read_to_string(report_dir.join("coverage.csv")).unwrap();
    assert!(coverage.contains("\n900,"));
    assert!(coverage.contains("\n1800,"));
    // Halving the interval keeps every link in every interval and adds intervals.
    let hourly = fs::read_to_string(report_dir.join("link_hourly.csv")).unwrap();
    for hour in [0, 900, 1800] {
        assert!(coverage.contains(&format!("\n{hour},")), "{hour}");
        for link in ["link1", "link2", "link3"] {
            assert!(
                hourly.contains(&format!("\"{link}\",{hour},")),
                "{link} at {hour}"
            );
        }
    }
    assert!(coverage.lines().count() > 3);
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_records_failure_without_publishing_a_completed_index() {
    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_failure",
    );
    let report_dir = output.join("analysis");
    let automatic = fs::read_to_string(report_dir.join("manifest.json")).unwrap();
    let events = output.join("ITERS/it.1/events/events.0.xml.zst");
    let recorded_events = fs::read(&events).unwrap();
    fs::write(&events, b"not a zstd stream").unwrap();

    let error = reanalyze_completed_run(&output, None).unwrap_err();
    assert!(error.to_string().contains("failed to parse"), "{error}");

    // The completed report survives, and the failure is reported by its own artifacts.
    assert_eq!(
        fs::read_to_string(report_dir.join("manifest.json")).unwrap(),
        automatic
    );
    assert!(report_dir.join("index.html").is_file());
    let failure_manifest =
        fs::read_to_string(output.join("analysis-failure/manifest.json")).unwrap();
    assert!(failure_manifest.contains("\"status\": \"failed\""));
    assert!(failure_manifest.contains("failed to parse"));
    let failure_status =
        fs::read_to_string(output.join("analysis-failure/module_status.json")).unwrap();
    assert!(failure_status.contains("\"status\": \"unavailable\""));

    // Restoring the recording and rerunning recovers the report without rerunning QSim.
    fs::write(&events, recorded_events).unwrap();
    reanalyze_completed_run(&output, None).unwrap();
    assert!(!output.join("analysis-failure").exists());
    assert!(report_dir.join("index.html").is_file());
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_clears_abandoned_staging_artifacts() {
    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_staging",
    );
    // An interrupted attempt leaves a partial staging directory and an unclaimed backup.
    let staging = output.join(".analysis-staging");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join("link_hourly.csv"), "partial\n").unwrap();
    fs::create_dir_all(output.join(".analysis-backup")).unwrap();
    fs::write(output.join(".analysis-backup/stale.txt"), "stale\n").unwrap();

    reanalyze_completed_run(&output, None).unwrap();
    assert!(!staging.exists());
    assert!(!output.join(".analysis-backup").exists());
    assert!(output.join("analysis/link_hourly.csv").is_file());
    assert!(output.join("analysis/index.html").is_file());
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_reuses_the_recorded_id_store_of_a_protobuf_run() {
    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-2.yml",
        "./test_output/simulation/standalone_proto",
    );
    let report_dir = output.join("analysis");
    // Protobuf runs persist their ID mapping. Resetting the store first is what makes the rerun
    // prove it restores that mapping instead of reusing one the simulation left in memory.
    assert!(output.join("output_ids.binpb").is_file());
    let manifest = fs::read_to_string(report_dir.join("manifest.json")).unwrap();
    assert!(manifest.contains("\"input_format\": \"binpb\""));
    let automatic_hourly = fs::read_to_string(report_dir.join("link_hourly.csv")).unwrap();
    let automatic_coverage = fs::read_to_string(report_dir.join("coverage.csv")).unwrap();
    let run_link_id = Id::<Link>::get_from_ext("link2").internal();

    id::reset_store();
    reanalyze_completed_run(&output, None).unwrap();
    assert_eq!(Id::<Link>::get_from_ext("link2").internal(), run_link_id);
    assert_eq!(
        fs::read_to_string(report_dir.join("link_hourly.csv")).unwrap(),
        automatic_hourly
    );
    assert_eq!(
        fs::read_to_string(report_dir.join("coverage.csv")).unwrap(),
        automatic_coverage
    );
    assert!(report_dir.join("index.html").is_file());
}

#[deterministic_id_test(rust_qsim)]
fn standalone_command_regenerates_a_report_and_reports_failures_by_exit_code() {
    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_command",
    );
    let report_dir = output.join("analysis");
    let automatic_coverage = fs::read_to_string(report_dir.join("coverage.csv")).unwrap();

    // A rerun with changed settings exits successfully and applies them.
    let output_of = Command::new(env!("CARGO_BIN_EXE_analyze"))
        .arg("--run-dir")
        .arg(&output)
        .arg("--interval-seconds")
        .arg("1800")
        .output()
        .unwrap();
    assert!(
        output_of.status.success(),
        "{}",
        String::from_utf8_lossy(&output_of.stderr)
    );
    let coverage = fs::read_to_string(report_dir.join("coverage.csv")).unwrap();
    assert!(coverage.contains("\n1800,"));
    assert_ne!(coverage, automatic_coverage);

    // A required-module failure exits non-zero and records diagnostics, keeping the last report.
    let events = output.join("ITERS/it.1/events/events.0.xml.zst");
    fs::write(&events, b"not a zstd stream").unwrap();
    let failed = Command::new(env!("CARGO_BIN_EXE_analyze"))
        .arg("--run-dir")
        .arg(&output)
        .output()
        .unwrap();
    assert_eq!(failed.status.code(), Some(1));
    // The diagnostic goes through the logger on stdout; stderr stays empty because nothing panicked.
    assert!(
        String::from_utf8_lossy(&failed.stdout).contains("failed to parse"),
        "{}",
        String::from_utf8_lossy(&failed.stdout)
    );
    assert!(failed.stderr.is_empty());
    assert!(report_dir.join("index.html").is_file());
    assert!(output.join("analysis-failure/manifest.json").is_file());

    // An invalid run directory exits non-zero with a diagnostic instead of panicking.
    let temp = tempfile::tempdir().unwrap();
    let invalid = Command::new(env!("CARGO_BIN_EXE_analyze"))
        .arg("--run-dir")
        .arg(temp.path())
        .output()
        .unwrap();
    assert_eq!(invalid.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&invalid.stdout).contains("no recorded analysis manifest"),
        "{}",
        String::from_utf8_lossy(&invalid.stdout)
    );
    assert!(invalid.stderr.is_empty());
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_reclaims_a_report_stranded_by_an_interrupted_publish() {
    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_stranded",
    );
    // A crash between the two renames of a publish leaves the report only in the backup. The
    // rerun must reclaim it even when it cannot finish, rather than stranding it.
    let stranded = fs::read_to_string(output.join("analysis/manifest.json")).unwrap();
    fs::rename(output.join("analysis"), output.join(".analysis-backup")).unwrap();
    fs::write(
        output.join("ITERS/it.1/events/events.0.xml.zst"),
        b"not a zstd stream",
    )
    .unwrap();

    reanalyze_completed_run(&output, None).unwrap_err();
    assert!(!output.join(".analysis-backup").exists());
    assert_eq!(
        fs::read_to_string(output.join("analysis/manifest.json")).unwrap(),
        stranded
    );
    assert!(output.join("analysis/index.html").is_file());
    assert!(output.join("analysis-failure/manifest.json").is_file());
}

#[deterministic_id_test(rust_qsim)]
fn standalone_rerun_rejects_invalid_inputs_with_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    let missing = reanalyze_completed_run(temp.path(), None).unwrap_err();
    assert!(
        missing
            .to_string()
            .contains("no recorded analysis manifest"),
        "{missing}"
    );
    assert!(!temp.path().join("analysis-failure").exists());

    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_invalid",
    );
    fs::write(output.join("analysis/manifest.json"), "{ not json").unwrap();
    let corrupt = reanalyze_completed_run(&output, None).unwrap_err();
    assert!(corrupt.to_string().contains("cannot parse"), "{corrupt}");

    let output = run_simulation(
        "./tests/resources/3-links/3-links-config-1.yml",
        "./test_output/simulation/standalone_missing_network",
    );
    fs::remove_file(output.join("output_network.xml.zst")).unwrap();
    let no_network = reanalyze_completed_run(&output, None).unwrap_err();
    assert!(
        no_network
            .to_string()
            .contains("missing recorded output network"),
        "{no_network}"
    );
    let failure = fs::read_to_string(output.join("analysis-failure/manifest.json")).unwrap();
    assert!(failure.contains("\"status\": \"failed\""));
    assert!(failure.contains("missing recorded output network"));
}
