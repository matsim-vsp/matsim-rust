//! Seed uncertainty and parameter sensitivity across completed-run reports.
//!
//! The module groups the reports a set of runs already published by scenario, seed and parameter
//! setting, and describes the distribution of policy differences over the supplied seed
//! replicates. It never launches a simulation: each seed's baseline/alternative differences come
//! from [`compare_completed_runs`], the same interface a researcher calls for a single pair, and
//! everything else is read from the reports that call leaves behind.

use super::cross_run::compare_completed_runs;
use super::{
    ANALYSIS_DIR, AnalysisError, CSV_TABLE_SCRIPT, Manifest, REPORT_STYLE, STATUS_COMPLETE,
    csv_preview_for_script, escape_html, mean, quantile, read_json, write_json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// Module name recorded in the ensemble manifest.
const MODULE: &str = "ensemble";
const ENSEMBLE_DIR: &str = "ensemble";
const STAGING_DIR: &str = ".ensemble-staging";
const BACKUP_DIR: &str = ".ensemble-backup";

/// Label of a parameter setting that declares no parameters.
const DEFAULT_SETTING: &str = "default";

/// Supplied assumption: one difference per seed, so a seed's own noise cancels.
const PAIRED_BY_SEED: &str = "paired_by_seed";
/// Supplied assumption: the same differences, but the standard error stops assuming that two
/// runs with the same seed number share a random stream.
const DIFFERENCE_OF_MEANS: &str = "difference_of_means";
/// Both assumptions are always reported, in this order; the manifest picks the supplied one.
const ASSUMPTIONS: [&str; 2] = [PAIRED_BY_SEED, DIFFERENCE_OF_MEANS];

/// Confidence level of every exported interval.
const CI_CONFIDENCE: f64 = 0.95;

/// Two-sided 95% Student-t critical values by degrees of freedom. An ensemble has one degree of
/// freedom per seed replicate beyond the first, so the tabulated range covers the replicate
/// counts a researcher runs; past it the normal quantile is used and `ci_method` names it.
const T_CRITICAL_95: [f64; 30] = [
    12.706205, 4.302653, 3.182446, 2.776445, 2.570582, 2.446912, 2.364624, 2.306004, 2.262157,
    2.228139, 2.200985, 2.178813, 2.160369, 2.144787, 2.131450, 2.119905, 2.109816, 2.100922,
    2.093024, 2.085963, 2.079614, 2.073873, 2.068658, 2.063899, 2.059539, 2.055529, 2.051831,
    2.048407, 2.045230, 2.042272,
];
/// Normal quantile used in place of the tabulated t value above the largest tabulated df.
const NORMAL_CRITICAL_95: f64 = 1.959964;

/// Rows of each table embedded in the local report. The CSVs always hold every row.
const REPORT_PREVIEW_ROWS: usize = 200;

/// Researcher-supplied description of the ensemble: which completed reports belong to which arm.
#[derive(Debug, Deserialize)]
struct EnsembleInput {
    /// Scenario whose runs are the reference arm every alternative is compared against.
    baseline_scenario: String,
    /// Which pairing assumption the supplied view uses; both are always reported.
    #[serde(default)]
    pairing: Option<String>,
    /// Optional metric filter. Every comparable metric is used when it is absent.
    #[serde(default)]
    metrics: Vec<String>,
    runs: Vec<RunInput>,
}

#[derive(Debug, Deserialize)]
struct RunInput {
    run_dir: PathBuf,
    scenario: String,
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
}

/// One declared run with the provenance read from its own published report.
struct Member {
    run_dir: PathBuf,
    scenario: String,
    parameters: BTreeMap<String, String>,
    parameter_setting: String,
    /// Recorded random seed of the run: the only evidence available for pairing.
    seed: u64,
    iteration: u32,
    interval_seconds: u32,
    simulation_end_time: u32,
    sample_size: f64,
    baseline: bool,
}

/// A baseline run and the alternative run that recorded the same seed.
struct Pair<'a> {
    baseline: &'a Member,
    alternative: &'a Member,
    comparison_dir: PathBuf,
}

/// A (scenario, parameter setting, seed) cell that exists on only one side.
struct MissingPair<'a> {
    member: &'a Member,
    /// The arm the gap belongs to. A baseline seed with no run in an arm belongs to that arm's
    /// grouping rather than to the baseline's own parameters.
    arm: Arm<'a>,
    /// `baseline` when an alternative run has no baseline run with its seed, `alternative` when a
    /// baseline seed has no run in that arm.
    missing_role: &'static str,
    reason: &'static str,
}

/// One comparable metric cell, identified the way the comparison report identifies it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct GroupKey {
    scenario: String,
    parameter_setting: String,
    metric: String,
    table: String,
    key: String,
}

/// One seed's contribution to a group, exactly as the comparison report recorded it. A row
/// without both values is a gap in the cohort rather than a difference.
struct DifferenceRow {
    seed: u64,
    baseline_run: String,
    alternative_run: String,
    baseline_value: Option<f64>,
    alternative_value: Option<f64>,
    difference: Option<f64>,
    relative_difference_percent: Option<f64>,
    status: String,
}

impl DifferenceRow {
    fn usable(&self) -> bool {
        self.difference.is_some()
            && self.baseline_value.is_some()
            && self.alternative_value.is_some()
    }
}

/// Every seed that reported a group, with the arm size the group was expected to reach.
struct Group {
    key: GroupKey,
    unit: String,
    expected_runs: usize,
    rows: Vec<DifferenceRow>,
}

impl Group {
    fn differences(&self) -> Vec<f64> {
        self.rows
            .iter()
            .filter(|row| row.usable())
            .filter_map(|row| row.difference)
            .collect()
    }
}

/// One assumption's interval around a group's mean difference.
struct Interval {
    standard_error: Option<f64>,
    low: Option<f64>,
    high: Option<f64>,
    method: &'static str,
    conclusion: &'static str,
}

/// Group completed reports by scenario, seed and parameter setting, and export the distribution
/// of policy differences over the supplied seed replicates.
///
/// `manifest_path` names a JSON ensemble manifest; relative `run_dir` entries resolve from the
/// directory holding it. `output_dir` is the root the ensemble report is written to. Each
/// baseline report's own `analysis/comparison` directory is refreshed from the pairs declared for
/// it, which is where the per-seed differences come from.
pub fn analyze_run_ensemble(
    output_dir: &Path,
    manifest_path: &Path,
) -> Result<PathBuf, AnalysisError> {
    let input: EnsembleInput = read_json(manifest_path)?;
    let supplied = supplied_assumption(input.pairing.as_deref())?;
    let members = read_members(manifest_path, &input)?;
    let pairs = pair_by_seed(&members);
    let missing = missing_pairs(&members, &pairs);
    let groups = collect_differences(&members, &pairs, &input.metrics)?;
    let ensemble = Ensemble {
        supplied,
        baseline_scenario: &input.baseline_scenario,
        metrics: &input.metrics,
        members: &members,
        pairs: &pairs,
        missing: &missing,
        groups: &groups,
        baseline_reuse: baseline_reused_across_arms(&pairs),
    };

    let staging = output_dir.join(STAGING_DIR);
    super::reset_staging(&staging)?;
    if let Err(error) = write_tables(&staging, &ensemble) {
        // A half-written ensemble is never published; the previous report stays as it was.
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    super::publish(
        &staging,
        &output_dir.join(ENSEMBLE_DIR),
        &output_dir.join(BACKUP_DIR),
    )
    .map(|published| published.join("index.html"))
}

/// Everything the exported tables and the report are written from.
struct Ensemble<'a> {
    supplied: &'static str,
    baseline_scenario: &'a str,
    metrics: &'a [String],
    members: &'a [Member],
    pairs: &'a [Pair<'a>],
    missing: &'a [MissingPair<'a>],
    groups: &'a [Group],
    /// Whether one baseline replicate is compared against more than one alternative arm.
    baseline_reuse: bool,
}

fn supplied_assumption(pairing: Option<&str>) -> Result<&'static str, AnalysisError> {
    match pairing {
        None => Ok(PAIRED_BY_SEED),
        Some(name) => ASSUMPTIONS
            .iter()
            .copied()
            .find(|assumption| *assumption == name)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "unknown pairing assumption {name}; supported assumptions are {}",
                    ASSUMPTIONS.join(", ")
                ))
            }),
    }
}

/// Read every declared run's published report. A run contributes its own seed and scale, and an
/// ensemble that mixes scales or interval widths is rejected rather than averaged.
fn read_members(manifest_path: &Path, input: &EnsembleInput) -> Result<Vec<Member>, AnalysisError> {
    if input.baseline_scenario.trim().is_empty() {
        return Err(AnalysisError::new(
            "ensemble manifest needs a non-empty baseline_scenario",
        ));
    }
    if input.runs.len() < 2 {
        return Err(AnalysisError::new(
            "ensemble manifest needs at least a baseline and an alternative run",
        ));
    }
    // Relative run directories resolve from the manifest, so one file can travel with the runs it
    // names and the ensemble never depends on the report's output directory.
    let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let mut members = Vec::with_capacity(input.runs.len());
    for run in &input.runs {
        if run.scenario.trim().is_empty() {
            return Err(AnalysisError::new(format!(
                "run {} has an empty scenario",
                run.run_dir.display()
            )));
        }
        let run_dir = if run.run_dir.is_absolute() {
            run.run_dir.clone()
        } else {
            root.join(&run.run_dir)
        };
        let report: Manifest = read_json(&run_dir.join(ANALYSIS_DIR).join("manifest.json"))?;
        if report.status != STATUS_COMPLETE {
            return Err(AnalysisError::new(format!(
                "run {} has no complete latest-iteration report",
                run_dir.display()
            )));
        }
        let baseline = run.scenario == input.baseline_scenario;
        let parameters = if baseline {
            // The baseline is the reference arm rather than a parameter setting of its own, so a
            // parameter declared on it would silently disappear from the grouping.
            if !run.parameters.is_empty() {
                return Err(AnalysisError::new(format!(
                    "baseline run {} must not declare parameters: the baseline is the reference arm",
                    run_dir.display()
                )));
            }
            BTreeMap::new()
        } else {
            run.parameters
                .iter()
                .map(|(name, value)| Ok((name.clone(), parameter_value(value)?)))
                .collect::<Result<_, AnalysisError>>()?
        };
        members.push(Member {
            parameter_setting: parameter_setting(&parameters),
            run_dir,
            scenario: run.scenario.clone(),
            parameters,
            seed: report.random_seed,
            iteration: report.iteration,
            interval_seconds: report.interval_seconds,
            simulation_end_time: report.simulation_end_time,
            sample_size: report.sample_size,
            baseline,
        });
    }
    validate_members(&members)?;
    Ok(members)
}

/// One scale and one window across the whole ensemble. Mixing them would average numbers that do
/// not describe the same quantity, and a per-pair comparison would not notice.
fn validate_members(members: &[Member]) -> Result<(), AnalysisError> {
    let mut seen: BTreeMap<(&str, &str, u64), &Path> = BTreeMap::new();
    let mut baseline_runs = 0;
    let mut alternative_runs = 0;
    for member in members {
        if let Some(previous) = seen.insert(
            (
                member.scenario.as_str(),
                member.parameter_setting.as_str(),
                member.seed,
            ),
            member.run_dir.as_path(),
        ) {
            return Err(AnalysisError::new(format!(
                "seed {} of scenario {} at parameter setting {} is declared by both {} and {}",
                member.seed,
                member.scenario,
                member.parameter_setting,
                previous.display(),
                member.run_dir.display()
            )));
        }
        if member.baseline {
            baseline_runs += 1;
        } else {
            alternative_runs += 1;
        }
    }
    if baseline_runs == 0 {
        return Err(AnalysisError::new(
            "ensemble manifest declares no run of the baseline scenario",
        ));
    }
    if alternative_runs == 0 {
        return Err(AnalysisError::new(
            "ensemble manifest declares no alternative run to compare with the baseline",
        ));
    }
    let first = &members[0];
    for member in &members[1..] {
        if member.interval_seconds != first.interval_seconds
            || member.simulation_end_time != first.simulation_end_time
        {
            return Err(AnalysisError::new(format!(
                "run {} uses interval width {} and simulation end time {}, but {} uses {} and {}; \
                 an ensemble needs one interval width and one covered window",
                member.run_dir.display(),
                member.interval_seconds,
                member.simulation_end_time,
                first.run_dir.display(),
                first.interval_seconds,
                first.simulation_end_time
            )));
        }
        if member.sample_size != first.sample_size {
            return Err(AnalysisError::new(format!(
                "run {} uses sample-size scale {}, but {} uses {}",
                member.run_dir.display(),
                member.sample_size,
                first.run_dir.display(),
                first.sample_size
            )));
        }
    }
    Ok(())
}

fn parameter_value(value: &Value) -> Result<String, AnalysisError> {
    match value {
        Value::String(text) => Ok(text.clone()),
        Value::Number(number) => Ok(number.to_string()),
        Value::Bool(flag) => Ok(flag.to_string()),
        other => Err(AnalysisError::new(format!(
            "parameter values must be a string, number or boolean, got {other}"
        ))),
    }
}

/// Canonical `key=value;key=value` label of a setting, so two settings that declare the same
/// parameters in a different JSON order stay one setting.
fn parameter_setting(parameters: &BTreeMap<String, String>) -> String {
    if parameters.is_empty() {
        return DEFAULT_SETTING.to_owned();
    }
    parameters
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// Pair each alternative run with the baseline run that recorded the same seed.
fn pair_by_seed(members: &[Member]) -> Vec<Pair<'_>> {
    members
        .iter()
        .filter(|member| !member.baseline)
        .filter_map(|alternative| {
            let baseline = members
                .iter()
                .find(|member| member.baseline && member.seed == alternative.seed)?;
            let comparison_dir = baseline
                .run_dir
                .join(ANALYSIS_DIR)
                .join("comparison")
                .join("metric_differences.csv");
            Some(Pair {
                baseline,
                alternative,
                comparison_dir,
            })
        })
        .collect()
}

/// Every declared run that no pair consumed, with the counterpart it is missing.
///
/// A gap belongs to an arm, not to a single pair: once seed 3 has no run in a scenario and
/// parameter setting, that cell is missing for that arm whichever other seeds the arm has.
fn missing_pairs<'a>(members: &'a [Member], pairs: &[Pair<'a>]) -> Vec<MissingPair<'a>> {
    let mut missing = Vec::new();
    for arm in arms(members) {
        let mut seeds = BTreeSet::new();
        for member in members.iter().filter(|member| in_arm(member, &arm)) {
            seeds.insert(member.seed);
        }
        for seed in &seeds {
            let baseline = members
                .iter()
                .find(|member| member.baseline && member.seed == *seed);
            let alternative = members
                .iter()
                .find(|member| !member.baseline && member.seed == *seed && in_arm(member, &arm));
            match (baseline, alternative) {
                (Some(baseline), Some(alternative))
                    if pairs.iter().any(|pair| {
                        std::ptr::eq(pair.baseline, baseline)
                            && std::ptr::eq(pair.alternative, alternative)
                    }) => {}
                (Some(baseline), None) => missing.push(MissingPair {
                    member: baseline,
                    arm,
                    missing_role: "alternative",
                    reason: "no_alternative_run_with_this_seed",
                }),
                (None, Some(alternative)) => missing.push(MissingPair {
                    member: alternative,
                    arm,
                    missing_role: "baseline",
                    reason: "no_baseline_run_with_this_seed",
                }),
                (None, None) => unreachable!("{seed} is a seed of a declared run"),
                (Some(_), Some(_)) => unreachable!("a declared pair is always compared"),
            }
        }
    }
    missing
}

/// An arm is one scenario at one parameter setting; the baseline is its own arm.
type Arm<'a> = (&'a str, &'a str);

fn arms(members: &[Member]) -> Vec<Arm<'_>> {
    members
        .iter()
        .filter(|member| !member.baseline)
        .map(|member| (member.scenario.as_str(), member.parameter_setting.as_str()))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn in_arm(member: &Member, arm: &Arm<'_>) -> bool {
    if member.baseline {
        return true;
    }
    (member.scenario.as_str(), member.parameter_setting.as_str()) == *arm
}

/// Whether one baseline replicate is compared against more than one alternative arm.
fn baseline_reused_across_arms(pairs: &[Pair<'_>]) -> bool {
    let mut arms: BTreeMap<&Path, BTreeSet<(&str, &str)>> = BTreeMap::new();
    for pair in pairs {
        arms.entry(pair.baseline.run_dir.as_path())
            .or_default()
            .insert((
                pair.alternative.scenario.as_str(),
                pair.alternative.parameter_setting.as_str(),
            ));
    }
    arms.values().any(|arms| arms.len() > 1)
}

/// Compare every pair through the shared comparison interface, then group the differences it
/// wrote by scenario, parameter setting, metric and key.
///
/// The comparison is the one a researcher would run for a single seed pair, so no difference is
/// defined twice: the ensemble only groups and summarises what that interface produced.
fn collect_differences(
    members: &[Member],
    pairs: &[Pair<'_>],
    metrics: &[String],
) -> Result<Vec<Group>, AnalysisError> {
    // A group is expected to reach every run its arm declares, including the seeds it could not
    // pair, so a shortfall is a count the reader can compare against the members table.
    let mut expected_runs: BTreeMap<(&str, &str), usize> = BTreeMap::new();
    for member in members.iter().filter(|member| !member.baseline) {
        *expected_runs
            .entry((member.scenario.as_str(), member.parameter_setting.as_str()))
            .or_default() += 1;
    }
    let mut alternatives_by_baseline: BTreeMap<&Path, BTreeSet<&Path>> = BTreeMap::new();
    for pair in pairs {
        alternatives_by_baseline
            .entry(pair.baseline.run_dir.as_path())
            .or_default()
            .insert(pair.alternative.run_dir.as_path());
    }
    let mut rows_by_alternative: BTreeMap<String, Vec<ComparisonRow>> = BTreeMap::new();
    for (baseline_dir, alternatives) in alternatives_by_baseline {
        let report = compare_completed_runs(
            baseline_dir,
            &alternatives
                .into_iter()
                .map(Path::to_path_buf)
                .collect::<Vec<_>>(),
        )?;
        let comparison_dir = report.parent().unwrap_or(baseline_dir);
        for row in read_comparison_rows(&comparison_dir.join("metric_differences.csv"))? {
            rows_by_alternative
                .entry(row.alternative.clone())
                .or_default()
                .push(row);
        }
    }

    let wanted = metrics.iter().cloned().collect::<BTreeSet<String>>();
    let mut seen_metrics: BTreeSet<String> = BTreeSet::new();
    let mut groups: BTreeMap<GroupKey, Group> = BTreeMap::new();
    for pair in pairs {
        let arm = (
            pair.alternative.scenario.as_str(),
            pair.alternative.parameter_setting.as_str(),
        );
        let baseline_run = pair.baseline.run_dir.display().to_string();
        let alternative_run = pair.alternative.run_dir.display().to_string();
        for row in rows_by_alternative
            .remove(&alternative_run)
            .unwrap_or_default()
        {
            if !wanted.is_empty() && !wanted.contains(&row.metric) {
                continue;
            }
            seen_metrics.insert(row.metric.clone());
            let key = GroupKey {
                scenario: pair.alternative.scenario.clone(),
                parameter_setting: pair.alternative.parameter_setting.clone(),
                metric: row.metric,
                table: row.table,
                key: row.key,
            };
            let group = groups.entry(key.clone()).or_insert_with(|| Group {
                key,
                unit: row.unit,
                expected_runs: expected_runs.get(&arm).copied().unwrap_or_default(),
                rows: Vec::new(),
            });
            group.rows.push(DifferenceRow {
                seed: pair.alternative.seed,
                baseline_run: baseline_run.clone(),
                alternative_run: alternative_run.clone(),
                baseline_value: row.baseline_value,
                alternative_value: row.alternative_value,
                difference: row.absolute_difference,
                relative_difference_percent: row.relative_difference_percent,
                status: row.status,
            });
        }
    }
    if !wanted.is_empty() {
        let unknown = wanted
            .iter()
            .filter(|metric| !seen_metrics.contains(*metric))
            .map(String::as_str)
            .collect::<Vec<_>>();
        if !unknown.is_empty() {
            return Err(AnalysisError::new(format!(
                "requested metrics are not comparable in any supplied pair: {}",
                unknown.join(", ")
            )));
        }
    }
    Ok(groups.into_values().collect())
}

/// One row of the comparison report the shared interface wrote.
struct ComparisonRow {
    alternative: String,
    table: String,
    metric: String,
    unit: String,
    key: String,
    baseline_value: Option<f64>,
    alternative_value: Option<f64>,
    absolute_difference: Option<f64>,
    relative_difference_percent: Option<f64>,
    status: String,
}

fn read_comparison_rows(path: &Path) -> Result<Vec<ComparisonRow>, AnalysisError> {
    let mut reader = csv::Reader::from_path(path).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| AnalysisError::new(format!("{} has no {name} column", path.display())))
    };
    let alternative = column("alternative")?;
    let table = column("table")?;
    let metric = column("metric")?;
    let unit = column("unit")?;
    let key = column("key")?;
    let baseline_value = column("baseline_value")?;
    let alternative_value = column("alternative_value")?;
    let absolute_difference = column("absolute_difference")?;
    let relative = column("relative_difference_percent")?;
    let status = column("status")?;
    let value = |record: &csv::StringRecord, index: usize| -> Result<Option<f64>, AnalysisError> {
        match record.get(index).map(str::trim) {
            None | Some("") => Ok(None),
            Some(field) => field
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(Some)
                .ok_or_else(|| {
                    AnalysisError::new(format!("invalid {field} in {}", path.display()))
                }),
        }
    };
    let mut rows = Vec::new();
    for record in reader.records() {
        let record = record.map_err(io_error)?;
        let text = |index: usize| record.get(index).unwrap_or_default().to_owned();
        rows.push(ComparisonRow {
            alternative: text(alternative),
            table: text(table),
            metric: text(metric),
            unit: text(unit),
            key: text(key),
            baseline_value: value(&record, baseline_value)?,
            alternative_value: value(&record, alternative_value)?,
            absolute_difference: value(&record, absolute_difference)?,
            relative_difference_percent: value(&record, relative)?,
            status: text(status),
        });
    }
    Ok(rows)
}

fn write_tables(path: &Path, ensemble: &Ensemble<'_>) -> Result<(), AnalysisError> {
    write_members(path, ensemble.members)?;
    write_pairs(path, ensemble.pairs)?;
    write_missing(path, ensemble.members, ensemble.missing)?;
    write_differences(path, ensemble.groups)?;
    write_uncertainty(path, ensemble)?;
    write_json(
        &path.join("manifest.json"),
        &EnsembleManifest {
            status: STATUS_COMPLETE,
            module: MODULE,
            baseline_scenario: ensemble.baseline_scenario,
            supplied_assumption: ensemble.supplied,
            assumptions: &ASSUMPTIONS,
            metrics: ensemble.metrics,
            ci_confidence: CI_CONFIDENCE,
            runs: ensemble
                .members
                .iter()
                .map(|member| MemberRecord {
                    run_dir: member.run_dir.display().to_string(),
                    scenario: &member.scenario,
                    role: if member.baseline {
                        "baseline"
                    } else {
                        "alternative"
                    },
                    parameters: &member.parameters,
                    parameter_setting: &member.parameter_setting,
                    seed: member.seed,
                    iteration: member.iteration,
                })
                .collect(),
            pairs: ensemble
                .pairs
                .iter()
                .map(|pair| PairRecord {
                    scenario: &pair.alternative.scenario,
                    parameter_setting: &pair.alternative.parameter_setting,
                    seed: pair.alternative.seed,
                    baseline_run_dir: pair.baseline.run_dir.display().to_string(),
                    alternative_run_dir: pair.alternative.run_dir.display().to_string(),
                    comparison_dir: pair.comparison_dir.display().to_string(),
                })
                .collect(),
        },
    )?;
    write_json(&path.join("metric_catalog.json"), &metric_catalog())?;
    write_report(path, ensemble)?;
    Ok(())
}

#[derive(Serialize)]
struct EnsembleManifest<'a> {
    status: &'static str,
    module: &'static str,
    baseline_scenario: &'a str,
    /// The pairing assumption the researcher supplied; both are always exported.
    supplied_assumption: &'static str,
    assumptions: &'a [&'static str; 2],
    /// Requested metric filter; empty means every comparable metric.
    metrics: &'a [String],
    ci_confidence: f64,
    runs: Vec<MemberRecord<'a>>,
    pairs: Vec<PairRecord<'a>>,
}

#[derive(Serialize)]
struct MemberRecord<'a> {
    run_dir: String,
    scenario: &'a str,
    role: &'static str,
    parameters: &'a BTreeMap<String, String>,
    parameter_setting: &'a str,
    seed: u64,
    iteration: u32,
}

#[derive(Serialize)]
struct PairRecord<'a> {
    scenario: &'a str,
    parameter_setting: &'a str,
    seed: u64,
    baseline_run_dir: String,
    alternative_run_dir: String,
    comparison_dir: String,
}

/// Every measure the ensemble exports. `metric_unit` is the unit of the compared metric, which
/// the tables carry next to each row; `label` is a categorical label rather than a measurement.
fn metric_catalog() -> Vec<EnsembleMetric> {
    const KEY: &str = "scenario,parameter_setting,metric,table,key";
    ["runs", "expected_runs", "missing_runs"]
        .into_iter()
        .map(|name| EnsembleMetric {
            name,
            unit: "seed_pairs",
            aggregation_key: KEY,
        })
        .chain(
            [
                "mean_difference",
                "std_difference",
                "standard_error",
                "min_difference",
                "p10_difference",
                "median_difference",
                "p90_difference",
                "max_difference",
                "ci_low",
                "ci_high",
                "absolute_difference",
                "baseline_value",
                "alternative_value",
            ]
            .into_iter()
            .map(|name| EnsembleMetric {
                name,
                unit: "metric_unit",
                aggregation_key: KEY,
            }),
        )
        .chain([
            EnsembleMetric {
                name: "relative_difference_percent",
                unit: "percent",
                aggregation_key: KEY,
            },
            EnsembleMetric {
                name: "ci_method",
                unit: "label",
                aggregation_key: "scenario,parameter_setting,metric,table,key,assumption",
            },
        ])
        .collect()
}

#[derive(Serialize)]
struct EnsembleMetric {
    name: &'static str,
    unit: &'static str,
    aggregation_key: &'static str,
}

fn write_members(path: &Path, members: &[Member]) -> Result<(), AnalysisError> {
    let mut writer = table(path, "ensemble_members.csv")?;
    writer
        .write_record([
            "run_dir",
            "scenario",
            "role",
            "parameters",
            "parameter_setting",
            "seed",
            "iteration",
        ])
        .map_err(io_error)?;
    for member in members {
        writer
            .write_record([
                member.run_dir.display().to_string(),
                member.scenario.clone(),
                if member.baseline {
                    "baseline"
                } else {
                    "alternative"
                }
                .to_owned(),
                parameters_json(&member.parameters),
                member.parameter_setting.clone(),
                member.seed.to_string(),
                member.iteration.to_string(),
            ])
            .map_err(io_error)?;
    }
    writer.flush().map_err(io_error)
}

fn parameters_json(parameters: &BTreeMap<String, String>) -> String {
    serde_json::to_string(parameters).unwrap_or_else(|_| "{}".to_owned())
}

fn write_pairs(path: &Path, pairs: &[Pair<'_>]) -> Result<(), AnalysisError> {
    let mut writer = table(path, "ensemble_pairs.csv")?;
    writer
        .write_record([
            "scenario",
            "parameter_setting",
            "seed",
            "baseline_run_dir",
            "alternative_run_dir",
            "comparison_dir",
        ])
        .map_err(io_error)?;
    for pair in pairs {
        writer
            .write_record([
                pair.alternative.scenario.clone(),
                pair.alternative.parameter_setting.clone(),
                pair.alternative.seed.to_string(),
                pair.baseline.run_dir.display().to_string(),
                pair.alternative.run_dir.display().to_string(),
                pair.comparison_dir.display().to_string(),
            ])
            .map_err(io_error)?;
    }
    writer.flush().map_err(io_error)
}

fn write_missing(
    path: &Path,
    members: &[Member],
    missing: &[MissingPair<'_>],
) -> Result<(), AnalysisError> {
    let mut writer = table(path, "ensemble_missing_pairs.csv")?;
    writer
        .write_record([
            "scenario",
            "parameter_setting",
            "seed",
            "run_dir",
            "role",
            "missing_role",
            "reason",
            "baseline_runs",
            "alternative_runs",
        ])
        .map_err(io_error)?;
    for gap in missing {
        writer
            .write_record([
                gap.arm.0.to_owned(),
                gap.arm.1.to_owned(),
                gap.member.seed.to_string(),
                gap.member.run_dir.display().to_string(),
                if gap.member.baseline {
                    "baseline"
                } else {
                    "alternative"
                }
                .to_owned(),
                gap.missing_role.to_owned(),
                gap.reason.to_owned(),
                arm_size(members, true).to_string(),
                arm_size(members, false).to_string(),
            ])
            .map_err(io_error)?;
    }
    writer.flush().map_err(io_error)
}

fn arm_size(members: &[Member], baseline: bool) -> usize {
    members
        .iter()
        .filter(|member| member.baseline == baseline)
        .count()
}

/// The per-seed distribution every group statistic is computed from.
fn write_differences(path: &Path, groups: &[Group]) -> Result<(), AnalysisError> {
    let mut writer = table(path, "ensemble_differences.csv")?;
    writer
        .write_record([
            "scenario",
            "parameter_setting",
            "seed",
            "metric",
            "unit",
            "table",
            "key",
            "baseline_run_dir",
            "alternative_run_dir",
            "baseline_value",
            "alternative_value",
            "absolute_difference",
            "relative_difference_percent",
            "status",
        ])
        .map_err(io_error)?;
    for group in groups {
        for row in &group.rows {
            writer
                .write_record([
                    group.key.scenario.clone(),
                    group.key.parameter_setting.clone(),
                    row.seed.to_string(),
                    group.key.metric.clone(),
                    group.unit.clone(),
                    group.key.table.clone(),
                    group.key.key.clone(),
                    row.baseline_run.clone(),
                    row.alternative_run.clone(),
                    number(row.baseline_value),
                    number(row.alternative_value),
                    number(row.difference),
                    number(row.relative_difference_percent),
                    row.status.clone(),
                ])
                .map_err(io_error)?;
        }
    }
    writer.flush().map_err(io_error)
}

/// One row per metric cell and assumption: the distribution of the policy differences, the
/// interval that assumption supports, and whether the conclusion survives the other one.
fn write_uncertainty(path: &Path, ensemble: &Ensemble<'_>) -> Result<(), AnalysisError> {
    let mut writer = table(path, "ensemble_uncertainty.csv")?;
    writer
        .write_record([
            "scenario",
            "parameter_setting",
            "metric",
            "unit",
            "table",
            "key",
            "assumption",
            "supplied",
            "runs",
            "expected_runs",
            "missing_runs",
            "mean_difference",
            "std_difference",
            "standard_error",
            "min_difference",
            "p10_difference",
            "median_difference",
            "p90_difference",
            "max_difference",
            "ci_low",
            "ci_high",
            "ci_method",
            "sign",
            "conclusion",
            "agrees_with_supplied_assumption",
        ])
        .map_err(io_error)?;
    for group in ensemble.groups {
        let statistics = statistics(group);
        let intervals = intervals(group, &statistics);
        for assumption in ASSUMPTIONS {
            let interval = intervals
                .get(assumption)
                .expect("both assumptions are reported");
            let agrees = if assumption == ensemble.supplied {
                "supplied"
            } else if interval.conclusion == intervals[ensemble.supplied].conclusion {
                "yes"
            } else {
                "no"
            };
            writer
                .write_record([
                    group.key.scenario.clone(),
                    group.key.parameter_setting.clone(),
                    group.key.metric.clone(),
                    group.unit.clone(),
                    group.key.table.clone(),
                    group.key.key.clone(),
                    assumption.to_owned(),
                    (assumption == ensemble.supplied).to_string(),
                    statistics.runs.to_string(),
                    group.expected_runs.to_string(),
                    group
                        .expected_runs
                        .saturating_sub(statistics.runs)
                        .to_string(),
                    number(statistics.mean_difference),
                    number(statistics.std_difference),
                    number(interval.standard_error),
                    number(statistics.min_difference),
                    number(statistics.p10_difference),
                    number(statistics.median_difference),
                    number(statistics.p90_difference),
                    number(statistics.max_difference),
                    number(interval.low),
                    number(interval.high),
                    interval.method.to_owned(),
                    statistics.sign.to_owned(),
                    interval.conclusion.to_owned(),
                    agrees.to_owned(),
                ])
                .map_err(io_error)?;
        }
    }
    writer.flush().map_err(io_error)
}

/// The distribution of a group's per-seed differences. Both assumptions share it; they differ
/// only in the interval, so the sensitivity table shows what the pairing assumption contributed.
struct Statistics {
    runs: usize,
    mean_difference: Option<f64>,
    std_difference: Option<f64>,
    min_difference: Option<f64>,
    p10_difference: Option<f64>,
    median_difference: Option<f64>,
    p90_difference: Option<f64>,
    max_difference: Option<f64>,
    sign: &'static str,
}

fn statistics(group: &Group) -> Statistics {
    let mut differences = group.differences();
    let runs = differences.len();
    let mean_difference = mean(&differences);
    // The quantiles use the same nearest-rank convention as the journey distributions.
    differences.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    Statistics {
        runs,
        mean_difference,
        std_difference: sample_std_dev(&differences),
        min_difference: differences.first().copied(),
        p10_difference: quantile(&differences, 0.1),
        median_difference: quantile(&differences, 0.5),
        p90_difference: quantile(&differences, 0.9),
        max_difference: differences.last().copied(),
        sign: sign(mean_difference),
    }
}

fn intervals(group: &Group, statistics: &Statistics) -> BTreeMap<&'static str, Interval> {
    let runs = statistics.runs;
    let usable = group
        .rows
        .iter()
        .filter(|row| row.usable())
        .collect::<Vec<_>>();
    let alternative = usable
        .iter()
        .filter_map(|row| row.alternative_value)
        .collect::<Vec<_>>();
    let baseline = usable
        .iter()
        .filter_map(|row| row.baseline_value)
        .collect::<Vec<_>>();
    let paired_error = statistics
        .std_difference
        .map(|std| std / (runs as f64).sqrt());
    // Dropping the pairing replaces the spread of the differences with the spread of each arm,
    // which is at least as wide whenever the seeds do not share a random stream.
    let unpaired_error = match (sample_std_dev(&alternative), sample_std_dev(&baseline)) {
        (Some(alternative_deviation), Some(baseline_deviation)) => Some(
            (alternative_deviation.powi(2) / alternative.len() as f64
                + baseline_deviation.powi(2) / baseline.len() as f64)
                .sqrt(),
        ),
        _ => None,
    };
    let mut intervals = BTreeMap::new();
    for (assumption, standard_error, method, degrees) in [
        (
            PAIRED_BY_SEED,
            paired_error,
            "paired_student_t_95",
            runs.saturating_sub(1),
        ),
        (
            DIFFERENCE_OF_MEANS,
            unpaired_error,
            "unpaired_student_t_min_df_95",
            alternative.len().min(baseline.len()).saturating_sub(1),
        ),
    ] {
        let (low, high) = match (statistics.mean_difference, standard_error) {
            (Some(estimate), Some(standard_error)) if runs >= 2 => {
                let margin = t_critical_95(degrees) * standard_error;
                (Some(estimate - margin), Some(estimate + margin))
            }
            _ => (None, None),
        };
        intervals.insert(
            assumption,
            Interval {
                standard_error,
                low,
                high,
                method,
                conclusion: conclusion(runs, low, high),
            },
        );
    }
    intervals
}

/// Sample standard deviation with the n - 1 denominator, the spread a Student-t interval needs.
fn sample_std_dev(values: &[f64]) -> Option<f64> {
    if values.len() < 2 {
        return None;
    }
    let average = mean(values)?;
    let sum_of_squares = values
        .iter()
        .map(|value| (value - average).powi(2))
        .sum::<f64>();
    // The sum of squares cannot be negative, but a rounded sum can land just below zero.
    Some((sum_of_squares / (values.len() - 1) as f64).max(0.0).sqrt())
}

fn t_critical_95(degrees_of_freedom: usize) -> f64 {
    T_CRITICAL_95
        .get(degrees_of_freedom.saturating_sub(1))
        .copied()
        .unwrap_or(NORMAL_CRITICAL_95)
}

fn sign(estimate: Option<f64>) -> &'static str {
    match estimate {
        None => "unavailable",
        Some(value) if value > 0.0 => "positive",
        Some(value) if value < 0.0 => "negative",
        Some(_) => "zero",
    }
}

/// What an interval supports: a side of zero, or no detectable difference. One replicate cannot
/// support an interval, and no comparable run says nothing at all.
fn conclusion(runs: usize, low: Option<f64>, high: Option<f64>) -> &'static str {
    match (runs, low, high) {
        (0, _, _) => "no_comparable_runs",
        (_, None, _) => "single_run_no_interval",
        (_, Some(low), Some(_)) if low > 0.0 => "alternative_higher",
        (_, Some(_), Some(high)) if high < 0.0 => "alternative_lower",
        _ => "no_detectable_difference",
    }
}

/// Render a table as a script payload, truncated to the preview budget, and report whether the CSV
/// held more rows than the report shows.
fn embed(path: &Path, name: &str) -> Result<(String, bool), AnalysisError> {
    let (rows, truncated) = csv_preview_for_script(&path.join(name), REPORT_PREVIEW_ROWS + 1)?;
    Ok((rows, truncated))
}

fn write_report(path: &Path, ensemble: &Ensemble<'_>) -> Result<(), AnalysisError> {
    let (members, members_truncated) = embed(path, "ensemble_members.csv")?;
    let (pairs, pairs_truncated) = embed(path, "ensemble_pairs.csv")?;
    let (missing, missing_truncated) = embed(path, "ensemble_missing_pairs.csv")?;
    let (differences, differences_truncated) = embed(path, "ensemble_differences.csv")?;
    let (uncertainty, uncertainty_truncated) = embed(path, "ensemble_uncertainty.csv")?;
    let note = |truncated: bool, name: &str| {
        if truncated {
            format!(
                "Showing the first {REPORT_PREVIEW_ROWS} rows; <a href=\"{name}\">{name}</a> holds every row."
            )
        } else {
            format!("Every row of <a href=\"{name}\">{name}</a> is shown.")
        }
    };
    let baseline_runs = arm_size(ensemble.members, true);
    let alternative_runs = arm_size(ensemble.members, false);
    let scope = if ensemble.metrics.is_empty() {
        "every metric the comparisons found comparable".to_owned()
    } else {
        format!(
            "only the requested metrics ({})",
            ensemble.metrics.join(", ")
        )
    };
    let html = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Seed uncertainty and parameter sensitivity</title><style>{REPORT_STYLE}</style></head><body><h1>Seed uncertainty and parameter sensitivity</h1><p>{baseline_runs} baseline replicates of scenario <code>{baseline}</code> and {alternative_runs} alternative replicates were grouped by scenario, seed and parameter setting. Every value comes from the latest completed iteration report of a run that already exists: this module launches no simulation, and it compares the runs through the same interface a single pair uses. Scope: {scope}.</p><h2>Assumptions and disclosures</h2><ul><li>Seeds are the <code>random_seed</code> each run recorded. Equal seed numbers alone do not guarantee comparable random streams: a stream also depends on the purpose and stream identifiers QSim derives per person and iteration, on the population, and on the scenario, so two runs sharing a seed number may still draw different numbers.</li><li>The supplied pairing assumption is <code>{supplied}</code>. Both assumptions summarise the same per-seed differences and differ only in the standard error: <code>{paired}</code> uses the spread of the differences, <code>{unpaired}</code> uses the spread of each arm and stops assuming the seeds share randomness. Where the two rows of <code>ensemble_uncertainty.csv</code> disagree on <code>conclusion</code>, the conclusion depends on that assumption.</li><li>Intervals are two-sided 95% Student-t intervals. <code>paired_student_t_95</code> has one degree of freedom per seed replicate beyond the first; <code>unpaired_student_t_min_df_95</code> uses the smaller arm's degrees of freedom, which is at least as conservative as the Welch&ndash;Satterthwaite choice. A single replicate supports no interval, and quantiles use the nearest-rank convention of the journey distributions.</li><li>Every seed without a counterpart is listed in <code>ensemble_missing_pairs.csv</code>, and a group reports <code>expected_runs</code> beside <code>runs</code> so shortfalls stay visible. Nothing is imputed for a missing run.</li><li>{reuse}</li></ul><h2>Ensemble members</h2><p>{members_note}</p><div id=\"members\"></div><h2>Compared pairs</h2><p>{pairs_note}</p><div id=\"pairs\"></div><h2>Missing pairs</h2><p>{missing_note}</p><div id=\"missing\"></div><h2>Policy difference distribution</h2><p>One row per metric cell and assumption: the number of seed pairs, the mean, standard deviation and quantiles of the differences, the interval, the sign of the mean difference, and the conclusion that sign and interval support.</p><p>{uncertainty_note}</p><div id=\"uncertainty\"></div><h2>Per-seed differences</h2><p>{differences_note}</p><div id=\"differences\"></div><p>Machine-readable data: <a href=\"ensemble_members.csv\">members (CSV)</a>, <a href=\"ensemble_pairs.csv\">compared pairs (CSV)</a>, <a href=\"ensemble_missing_pairs.csv\">missing pairs (CSV)</a>, <a href=\"ensemble_differences.csv\">per-seed differences (CSV)</a>, <a href=\"ensemble_uncertainty.csv\">difference distribution (CSV)</a>, <a href=\"manifest.json\">ensemble manifest</a>, <a href=\"metric_catalog.json\">metric catalog</a>.</p><script>{CSV_TABLE_SCRIPT}csvTable('#members',{members});csvTable('#pairs',{pairs});csvTable('#missing',{missing});csvTable('#uncertainty',{uncertainty});csvTable('#differences',{differences});</script></body></html>",
        baseline = escape_html(ensemble.baseline_scenario),
        supplied = ensemble.supplied,
        paired = PAIRED_BY_SEED,
        unpaired = DIFFERENCE_OF_MEANS,
        reuse = if ensemble.baseline_reuse {
            "One baseline replicate is compared against more than one alternative arm, so those arms are correlated with each other and a difference between two arms is not an independent comparison."
        } else {
            "Every baseline replicate is used by at most one alternative arm, so the arms use independent baseline replicates."
        },
        members_note = note(members_truncated, "ensemble_members.csv"),
        pairs_note = note(pairs_truncated, "ensemble_pairs.csv"),
        missing_note = note(missing_truncated, "ensemble_missing_pairs.csv"),
        uncertainty_note = note(uncertainty_truncated, "ensemble_uncertainty.csv"),
        differences_note = note(differences_truncated, "ensemble_differences.csv"),
    );
    fs::write(path.join("index.html"), html).map_err(io_error)
}

fn table(path: &Path, name: &str) -> Result<csv::Writer<fs::File>, AnalysisError> {
    csv::Writer::from_path(path.join(name)).map_err(io_error)
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(String::new, |value| format!("{value:.6}"))
}

fn io_error(error: impl std::fmt::Display) -> AnalysisError {
    AnalysisError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn detects_baseline_reuse_across_scenarios_with_the_same_setting() {
        let baseline = member_for_test("baseline", "baseline", true);
        let alternative_a = member_for_test("alternative-a", "policy-a", false);
        let alternative_b = member_for_test("alternative-b", "policy-b", false);
        let pairs = [
            Pair {
                baseline: &baseline,
                alternative: &alternative_a,
                comparison_dir: PathBuf::new(),
            },
            Pair {
                baseline: &baseline,
                alternative: &alternative_b,
                comparison_dir: PathBuf::new(),
            },
        ];

        assert!(baseline_reused_across_arms(&pairs));
    }

    fn member_for_test(run_dir: &str, scenario: &str, baseline: bool) -> Member {
        Member {
            run_dir: PathBuf::from(run_dir),
            scenario: scenario.to_owned(),
            parameters: BTreeMap::new(),
            parameter_setting: DEFAULT_SETTING.to_owned(),
            seed: 1,
            iteration: 1,
            interval_seconds: 3600,
            simulation_end_time: 3600,
            sample_size: 1.0,
            baseline,
        }
    }

    /// A completed report with one comparable metric, so a hand-computed ensemble is possible.
    fn run(root: &Path, name: &str, seed: u64, entries: f64) -> PathBuf {
        let output = root.join(name);
        let analysis = output.join("analysis");
        fs::create_dir_all(&analysis).unwrap();
        fs::write(
            analysis.join("manifest.json"),
            format!(
                r#"{{"status":"complete","failure":null,"iteration":3,"interval_seconds":3600,"simulation_end_time":3600,"partitions":[0],"input_format":"xml","eligible_links":1,"random_seed":{seed},"sample_size":1.0,"network_input":null,"population_input":null,"software_version":"test"}}"#
            ),
        )
        .unwrap();
        fs::write(
            analysis.join("metric_catalog.json"),
            r#"[{"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"}]"#,
        )
        .unwrap();
        fs::write(
            analysis.join("link_hourly.csv"),
            format!("link_id,hour_start_seconds,entry_vehicles,exit_vehicles\nl1,0,{entries},0\n"),
        )
        .unwrap();
        output
    }

    fn manifest(root: &Path, body: &str) -> PathBuf {
        let path = root.join("ensemble.json");
        fs::write(&path, body).unwrap();
        path
    }

    /// One seed's difference of `delta` on link l1, for both arms.
    fn pair(root: &Path, seed: u64, baseline: f64, alternative: f64) {
        run(root, &format!("base-{seed}"), seed, baseline);
        run(root, &format!("alt-{seed}"), seed, alternative);
    }

    /// Three seeds whose differences are 1, 2 and 3: mean 2, sample standard deviation 1, and a
    /// paired interval of 2 +/- 4.302653 / sqrt(3) that contains zero.
    fn three_seed_manifest(root: &Path) -> PathBuf {
        for seed in 1..=3u64 {
            pair(
                root,
                seed,
                10.0 * seed as f64,
                10.0 * seed as f64 + seed as f64,
            );
        }
        manifest(
            root,
            r#"{"baseline_scenario":"baseline","runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"base-2","scenario":"baseline"},
                {"run_dir":"base-3","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"},
                {"run_dir":"alt-2","scenario":"policy"},
                {"run_dir":"alt-3","scenario":"policy"}]}"#,
        )
    }

    /// One uncertainty table as header-keyed rows, so a test can ask for a column by name.
    fn uncertainty(root: &Path) -> Vec<BTreeMap<String, String>> {
        let mut reader =
            csv::Reader::from_path(root.join("ensemble/ensemble_uncertainty.csv")).unwrap();
        let headers = reader.headers().unwrap().clone();
        reader
            .records()
            .map(|record| {
                let record = record.unwrap();
                headers
                    .iter()
                    .zip(record.iter())
                    .map(|(header, value)| (header.to_owned(), value.to_owned()))
                    .collect()
            })
            .collect()
    }

    fn row<'a>(
        rows: &'a [BTreeMap<String, String>],
        setting: &str,
        assumption: &str,
    ) -> &'a BTreeMap<String, String> {
        rows.iter()
            .find(|row| row["parameter_setting"] == setting && row["assumption"] == assumption)
            .unwrap_or_else(|| panic!("no {setting} {assumption} row"))
    }

    #[test]
    fn reports_a_hand_computable_difference_distribution() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let report = analyze_run_ensemble(root, &three_seed_manifest(root)).unwrap();
        assert_eq!(report, root.join("ensemble/index.html"));

        let rows = uncertainty(root);
        assert_eq!(rows.len(), 2, "one cell, both assumptions");
        let paired = row(&rows, DEFAULT_SETTING, PAIRED_BY_SEED);
        assert_eq!(paired["runs"], "3");
        assert_eq!(paired["expected_runs"], "3");
        assert_eq!(paired["missing_runs"], "0");
        assert_eq!(paired["mean_difference"], "2.000000");
        assert_eq!(paired["std_difference"], "1.000000");
        assert_eq!(paired["standard_error"], "0.577350");
        assert_eq!(paired["min_difference"], "1.000000");
        // Nearest rank over 1, 2, 3: the 10th and 90th percentiles are the second and third
        // values, the median the second.
        assert_eq!(paired["p10_difference"], "2.000000");
        assert_eq!(paired["median_difference"], "2.000000");
        assert_eq!(paired["p90_difference"], "3.000000");
        assert_eq!(paired["max_difference"], "3.000000");
        // 2 +/- 4.302653 * 1 / sqrt(3) = 2 +/- 2.484138.
        assert_eq!(paired["ci_low"], "-0.484138");
        assert_eq!(paired["ci_high"], "4.484138");
        assert_eq!(paired["ci_method"], "paired_student_t_95");
        assert_eq!(paired["sign"], "positive");
        assert_eq!(paired["conclusion"], "no_detectable_difference");
        assert_eq!(paired["supplied"], "true");
        assert_eq!(paired["agrees_with_supplied_assumption"], "supplied");
        // The key is the comparison report's own JSON key tuple for the metric's aggregation.
        assert_eq!(paired["key"], r#"["l1","0"]"#);
        assert_eq!(paired["unit"], "vehicles");
        assert_eq!(paired["metric"], "entry_vehicles");
        assert_eq!(paired["table"], "link_hourly.csv");

        // Members carry the seed their own report recorded, and the setting they belong to.
        let members = fs::read_to_string(root.join("ensemble/ensemble_members.csv")).unwrap();
        assert!(
            members.contains("/base-2,baseline,baseline,{},default,2,3"),
            "{members}"
        );
        assert!(
            members.contains("/alt-2,policy,alternative,{},default,2,3"),
            "{members}"
        );
        let pairs = fs::read_to_string(root.join("ensemble/ensemble_pairs.csv")).unwrap();
        assert!(pairs.contains("policy,default,2,"), "{pairs}");
        assert!(
            pairs.contains("analysis/comparison/metric_differences.csv"),
            "{pairs}"
        );
        let differences =
            fs::read_to_string(root.join("ensemble/ensemble_differences.csv")).unwrap();
        assert_eq!(differences.lines().count(), 4, "header plus three seeds");
        assert!(differences.contains("10.000000,11.000000,1.000000,10.000000,comparable"));
        let manifest = fs::read_to_string(root.join("ensemble/manifest.json")).unwrap();
        assert!(manifest.contains("\"supplied_assumption\": \"paired_by_seed\""));
        assert!(manifest.contains("\"difference_of_means\""));
        assert!(manifest.contains("\"seed\": 3"));
        assert!(manifest.contains("\"ci_confidence\": 0.95"));
        let catalog = fs::read_to_string(root.join("ensemble/metric_catalog.json")).unwrap();
        assert!(catalog.contains("\"mean_difference\""));
        assert!(catalog.contains("\"ci_method\""));

        let html = fs::read_to_string(&report).unwrap();
        assert!(
            html.contains("Equal seed numbers alone do not guarantee comparable random streams"),
            "{html}"
        );
        assert!(html.contains("ensemble_uncertainty.csv"));
        assert!(html.contains("ensemble_missing_pairs.csv"));
    }

    #[test]
    fn discloses_missing_pairs_and_unequal_run_counts() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        for seed in 1..=3u64 {
            pair(
                root,
                seed,
                10.0 * seed as f64,
                10.0 * seed as f64 + seed as f64,
            );
        }
        // Seed 3 lost its alternative run, and seed 4 has an alternative with no baseline.
        fs::remove_dir_all(root.join("alt-3")).unwrap();
        run(root, "alt-4", 4, 44.0);
        let path = manifest(
            root,
            r#"{"baseline_scenario":"baseline","runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"base-2","scenario":"baseline"},
                {"run_dir":"base-3","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"},
                {"run_dir":"alt-2","scenario":"policy"},
                {"run_dir":"alt-4","scenario":"policy"}]}"#,
        );
        analyze_run_ensemble(root, &path).unwrap();

        // Run directories are recorded in full, so a row is named by the seed it is missing.
        let missing = fs::read_to_string(root.join("ensemble/ensemble_missing_pairs.csv")).unwrap();
        assert!(
            missing.contains("policy,default,3,")
                && missing.contains("/base-3,")
                && missing.contains("baseline,alternative,no_alternative_run_with_this_seed"),
            "{missing}"
        );
        assert!(
            missing.contains("policy,default,4,")
                && missing.contains("/alt-4,")
                && missing.contains("alternative,baseline,no_baseline_run_with_this_seed"),
            "{missing}"
        );
        assert!(missing.contains("3,3"), "arm sizes are exported: {missing}");

        let rows = uncertainty(root);
        let paired = row(&rows, DEFAULT_SETTING, PAIRED_BY_SEED);
        // Two of the three declared alternative runs paired; the shortfall is reported, never
        // imputed.
        assert_eq!(paired["runs"], "2");
        assert_eq!(paired["expected_runs"], "3");
        assert_eq!(paired["missing_runs"], "1");
        let unpaired = row(&rows, DEFAULT_SETTING, DIFFERENCE_OF_MEANS);
        assert_eq!(unpaired["runs"], "2");
        assert_eq!(unpaired["mean_difference"], "1.500000");
        assert_eq!(unpaired["conclusion"], "no_detectable_difference");
    }

    #[test]
    fn shows_conclusion_sensitivity_across_parameter_settings_and_assumptions() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        // The baseline arm alone varies by 50 vehicles between seeds, so a setting whose effect is
        // only visible because the seed pairing cancels that variation reads as significant under
        // one assumption and not the other.
        for (index, seed) in [1u64, 2, 3].into_iter().enumerate() {
            let baseline = 50.0 * index as f64;
            run(root, &format!("base-{seed}"), seed, baseline);
            // A tight, consistently positive effect: detectable once the seeds are paired.
            run(
                root,
                &format!("alt-high-{seed}"),
                seed,
                baseline + [0.10, 0.12, 0.11][index],
            );
            // An effect that wanders around zero: not detectable under any assumption.
            run(
                root,
                &format!("alt-low-{seed}"),
                seed,
                baseline + [-1.0, 2.0, -1.5][index],
            );
        }
        let path = manifest(
            root,
            r#"{"baseline_scenario":"baseline","pairing":"paired_by_seed","metrics":["entry_vehicles"],"runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"base-2","scenario":"baseline"},
                {"run_dir":"base-3","scenario":"baseline"},
                {"run_dir":"alt-low-1","scenario":"policy","parameters":{"charge":2}},
                {"run_dir":"alt-low-2","scenario":"policy","parameters":{"charge":2}},
                {"run_dir":"alt-low-3","scenario":"policy","parameters":{"charge":2}},
                {"run_dir":"alt-high-1","scenario":"policy","parameters":{"charge":5}},
                {"run_dir":"alt-high-2","scenario":"policy","parameters":{"charge":5}},
                {"run_dir":"alt-high-3","scenario":"policy","parameters":{"charge":5}}]}"#,
        );
        analyze_run_ensemble(root, &path).unwrap();

        let members = fs::read_to_string(root.join("ensemble/ensemble_members.csv")).unwrap();
        // The parameter map is CSV-quoted, so the JSON braces arrive doubled.
        assert!(members.contains(r#"{""charge"":""2""}"#), "{members}");
        assert!(members.contains("charge=2"), "{members}");
        assert!(members.contains("charge=5"), "{members}");

        let rows = uncertainty(root);
        assert_eq!(rows.len(), 4, "two settings, both assumptions");
        // Five euros holds its effect at the same seeds; two euros does not.
        assert_eq!(
            row(&rows, "charge=5", PAIRED_BY_SEED)["conclusion"],
            "alternative_higher"
        );
        assert_eq!(
            row(&rows, "charge=2", PAIRED_BY_SEED)["conclusion"],
            "no_detectable_difference"
        );
        // The sign moves with the setting even though the seeds do not.
        let high = row(&rows, "charge=5", PAIRED_BY_SEED);
        let low = row(&rows, "charge=2", PAIRED_BY_SEED);
        assert_eq!(high["sign"], "positive");
        assert_eq!(low["sign"], "negative");
        assert_eq!(low["mean_difference"], "-0.166667");
        // The unpaired view of the five-euro differences cannot separate the arms, and the row
        // says the conclusion is assumption dependent.
        let unpaired = row(&rows, "charge=5", DIFFERENCE_OF_MEANS);
        assert_eq!(unpaired["conclusion"], "no_detectable_difference");
        assert_eq!(unpaired["agrees_with_supplied_assumption"], "no");
        assert_eq!(unpaired["ci_method"], "unpaired_student_t_min_df_95");
        let html = fs::read_to_string(root.join("ensemble/index.html")).unwrap();
        assert!(html.contains("charge=2"), "settings are presented: {html}");
    }

    #[test]
    fn rejects_manifests_that_cannot_produce_an_ensemble() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        pair(root, 1, 10.0, 11.0);
        let arms = r#"{"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"}"#;

        let unknown = manifest(
            root,
            &format!(
                r#"{{"baseline_scenario":"baseline","pairing":"common_random_numbers","runs":[{arms}]}}"#
            ),
        );
        let error = analyze_run_ensemble(root, &unknown).unwrap_err();
        assert!(
            error.to_string().contains("unknown pairing assumption"),
            "{error}"
        );

        let no_baseline = manifest(
            root,
            r#"{"baseline_scenario":"reference","runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"}]}"#,
        );
        let error = analyze_run_ensemble(root, &no_baseline).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no run of the baseline scenario"),
            "{error}"
        );

        let duplicate = manifest(
            root,
            r#"{"baseline_scenario":"baseline","runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"},
                {"run_dir":"alt-1","scenario":"policy"}]}"#,
        );
        let error = analyze_run_ensemble(root, &duplicate).unwrap_err();
        assert!(error.to_string().contains("is declared by both"), "{error}");

        let baseline_parameters = manifest(
            root,
            r#"{"baseline_scenario":"baseline","runs":[
                {"run_dir":"base-1","scenario":"baseline","parameters":{"charge":2}},
                {"run_dir":"alt-1","scenario":"policy"}]}"#,
        );
        let error = analyze_run_ensemble(root, &baseline_parameters).unwrap_err();
        assert!(
            error.to_string().contains("must not declare parameters"),
            "{error}"
        );

        let unknown_metric = manifest(
            root,
            r#"{"baseline_scenario":"baseline","metrics":["leg_departures"],"runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"}]}"#,
        );
        let error = analyze_run_ensemble(root, &unknown_metric).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("not comparable in any supplied pair"),
            "{error}"
        );
        assert!(!root.join("ensemble").exists());

        // One seed on each side produces a difference but no interval, and says so.
        let single = manifest(
            root,
            &format!(r#"{{"baseline_scenario":"baseline","runs":[{arms}]}}"#),
        );
        analyze_run_ensemble(root, &single).unwrap();
        let rows = uncertainty(root);
        assert_eq!(rows[0]["runs"], "1");
        assert_eq!(rows[0]["conclusion"], "single_run_no_interval");
        assert_eq!(rows[0]["ci_low"], "");
        assert_eq!(rows[0]["mean_difference"], "1.000000");

        // A run without a complete report cannot join an ensemble.
        let manifest_path = root.join("alt-1/analysis/manifest.json");
        fs::write(
            &manifest_path,
            fs::read_to_string(&manifest_path)
                .unwrap()
                .replace("\"status\":\"complete\"", "\"status\":\"failed\""),
        )
        .unwrap();
        let error = analyze_run_ensemble(root, &single).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("no complete latest-iteration report"),
            "{error}"
        );
    }

    #[test]
    fn rejects_an_ensemble_that_mixes_scales() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        for seed in 1..=2u64 {
            pair(root, seed, 10.0 * seed as f64, 10.0 * seed as f64 + 1.0);
        }
        let wide = root.join("alt-2/analysis/manifest.json");
        fs::write(
            &wide,
            fs::read_to_string(&wide)
                .unwrap()
                .replace("\"interval_seconds\":3600", "\"interval_seconds\":900"),
        )
        .unwrap();
        let path = manifest(
            root,
            r#"{"baseline_scenario":"baseline","runs":[
                {"run_dir":"base-1","scenario":"baseline"},
                {"run_dir":"base-2","scenario":"baseline"},
                {"run_dir":"alt-1","scenario":"policy"},
                {"run_dir":"alt-2","scenario":"policy"}]}"#,
        );
        let error = analyze_run_ensemble(root, &path).unwrap_err();
        assert!(error.to_string().contains("one interval width"), "{error}");
        assert!(!root.join("ensemble").exists());
    }
}
