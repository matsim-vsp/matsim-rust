//! Demographic group distributions and cross-run equity comparisons.
//!
//! People are grouped by the person attributes a run supplies, so travel burdens can be read by
//! income, age, car availability or neighbourhood. The group labels, weights and costs are
//! captured from the population before the final iteration's mobsim and travel with the run
//! metadata, so a standalone rerun groups exactly the people the run simulated.
//!
//! Group statistics and the equity comparison both read `person_daily.csv`, the table the agent
//! travel module writes, so a person cannot be described one way by the group tables and another
//! way by the comparison.

use super::{
    Analysis, AnalysisError, AnalysisRunMetadata, UNKNOWN, csv, io_error, mean, number_opt,
    quantile, table_writer,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Module name in `module_status.json`.
pub(super) const MODULE: &str = "demographic_equity";

/// Equity criterion the winner and loser counts implement.
///
/// A person's daily travel burden is defined only when their day is `complete` or `no_travel`,
/// because an `incomplete` or `stuck` day reports the durations of the legs that happened rather
/// than the burden of the whole planned day. Within the comparable population a person is a
/// **winner** when the comparison run's burden is lower than the baseline's, a **loser** when it
/// is higher, and **unchanged** when the two agree. The criterion travels with every row so the
/// counts are never read without it.
pub(super) const EQUITY_CRITERION: &str = "lower_daily_completed_travel_time";

/// The same criterion in words, published with the report so a winner or loser count is never read
/// without the rule that produced it.
pub(super) const EQUITY_CRITERION_DESCRIPTION: &str = "winners are the people whose completed daily travel time is \
     lower in the comparison run than in this run, losers are the people whose completed daily travel time is higher, \
     and the counts cover only the people both runs group the same way and describe with a completed day.";

/// Burden differences at or below this are unchanged. The same day's legs summed in a different
/// order can differ in the last bits, and a run that reordered its events should not manufacture
/// winners out of that.
const UNCHANGED_TOLERANCE_SECONDS: f64 = 1e-6;

/// Dimension and group label of the whole-population row of the equity comparison.
const ALL: &str = "all";

/// Suffix of the per-group outcome tables other modules publish. A module that writes
/// `<module>_group_outcomes.csv` with the columns `dimension,group,metric,unit,value` into the
/// report directory has its outcomes folded into `group_module_outcomes.csv`. A module that does
/// not export that table contributes nothing and stays unavailable.
const GROUP_OUTCOME_SUFFIX: &str = "_group_outcomes.csv";

/// The metrics this module exports, as (name, unit, aggregation key) triples, so every column of
/// its tables can be looked up in `metric_catalog.json`.
pub(super) const METRICS: &[(&str, &str, &str)] = &[
    ("dimension", "person attribute", "person_id,dimension"),
    ("group", "attribute value", "person_id,dimension"),
    ("weight", "persons", "person_id,dimension"),
    ("weight_source", "category", "person_id,dimension"),
    ("monetary_cost", "currency", "person_id,dimension"),
    ("group_persons", "persons", "dimension,group"),
    ("weighted_persons", "persons", "dimension,group"),
    ("weighted_share", "proportion", "dimension,group"),
    ("persons_with_default_weight", "persons", "dimension,group"),
    ("travelling_persons", "persons", "dimension,group"),
    ("defined_persons", "persons", "dimension,group"),
    ("incomplete_persons", "persons", "dimension,group"),
    ("group_mean_travel_time", "seconds", "dimension,group"),
    ("group_median_travel_time", "seconds", "dimension,group"),
    ("group_p90_travel_time", "seconds", "dimension,group"),
    ("persons_with_cost", "persons", "dimension,group"),
    ("group_mean_monetary_cost", "currency", "dimension,group"),
    ("source", "module", "dimension,group,source,metric"),
    (
        "metric",
        "module metric name",
        "dimension,group,source,metric",
    ),
    (
        "value",
        "module metric unit",
        "dimension,group,source,metric",
    ),
    (
        "equity_criterion",
        "category",
        "comparison_run,dimension,group",
    ),
    (
        "comparable_persons",
        "persons",
        "comparison_run,dimension,group",
    ),
    ("winners", "persons", "comparison_run,dimension,group"),
    ("unchanged", "persons", "comparison_run,dimension,group"),
    ("losers", "persons", "comparison_run,dimension,group"),
    (
        "baseline_only_persons",
        "persons",
        "comparison_run,dimension,group",
    ),
    (
        "comparison_only_persons",
        "persons",
        "comparison_run,dimension,group",
    ),
    (
        "not_comparable_persons",
        "persons",
        "comparison_run,dimension,group",
    ),
    (
        "mean_baseline_travel_time",
        "seconds",
        "comparison_run,dimension,group",
    ),
    (
        "mean_comparison_travel_time",
        "seconds",
        "comparison_run,dimension,group",
    ),
    (
        "mean_travel_time_change",
        "seconds",
        "comparison_run,dimension,group",
    ),
];

/// One person's preserved grouping attributes, weight and cost.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonDemographic {
    person_id: String,
    /// Group label per configured attribute. A person who does not supply an attribute is
    /// reported as `unknown` rather than dropped, so group sizes stay comparable.
    groups: BTreeMap<String, String>,
    /// The supplied weight, or `None` when it is missing or not a finite non-negative number. An
    /// absent weight counts as one.
    weight: Option<f64>,
    /// The supplied monetary travel cost, or `None` when it is missing or not finite.
    cost: Option<f64>,
}

/// Capture the grouping attributes, weights and costs of every person in the population.
///
/// The population is only available before the final mobsim, so this runs next to the
/// expected-travel capture and its result is recorded in the run metadata.
pub fn capture(population: &super::Population, settings: &Analysis) -> Vec<PersonDemographic> {
    let mut persons: Vec<_> = population.persons.values().collect();
    // The population is keyed by internal id, so the captured order is established explicitly to
    // keep the recorded metadata reproducible.
    persons.sort_by(|a, b| a.id().external().cmp(b.id().external()));
    persons
        .into_iter()
        .map(|person| {
            let groups = settings
                .person_group_attributes
                .iter()
                .map(|attribute| {
                    let label = person
                        .attributes()
                        .get::<Value>(attribute)
                        .map(|value| group_label(&value))
                        .unwrap_or_else(|| UNKNOWN.to_owned());
                    (attribute.clone(), label)
                })
                .collect();
            PersonDemographic {
                person_id: person.id().external().to_owned(),
                groups,
                weight: settings
                    .person_weight_attribute
                    .as_deref()
                    .and_then(|attribute| {
                        person
                            .attributes()
                            .get::<f64>(attribute)
                            .filter(|weight| weight.is_finite() && *weight >= 0.0)
                    }),
                cost: settings
                    .person_cost_attribute
                    .as_deref()
                    .and_then(|attribute| {
                        person
                            .attributes()
                            .get::<f64>(attribute)
                            .filter(|cost| cost.is_finite())
                    }),
            }
        })
        .collect()
}

/// The group label a supplied attribute value produces. Only scalars name a group; a blank
/// string, a list or a nested object is reported as `unknown` like a missing attribute.
fn group_label(value: &Value) -> String {
    match value {
        Value::String(text) if !text.trim().is_empty() => text.trim().to_owned(),
        Value::String(_) | Value::Null | Value::Array(_) | Value::Object(_) => UNKNOWN.to_owned(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
    }
}

/// One person's observed day, as `person_daily.csv` records it.
#[derive(Clone)]
struct PersonOutcome {
    completed_legs: usize,
    travel_time_seconds: f64,
    /// `complete` and `no_travel` describe a whole day; `incomplete` and `stuck` do not.
    status: String,
}

impl PersonOutcome {
    /// Whether the day is complete enough for its travel-time burden to be comparable.
    fn has_defined_burden(&self) -> bool {
        matches!(self.status.as_str(), "complete" | "no_travel")
    }
}

/// A person as the group tables describe them.
struct GroupedPerson {
    person_id: String,
    groups: BTreeMap<String, String>,
    weight: f64,
    weight_source: &'static str,
    cost: Option<f64>,
    outcome: Option<PersonOutcome>,
}

/// Write the person groups, the group burdens, the fold-in of other modules' group outcomes and
/// the equity comparison.
///
/// Returns an error when a configured comparison run cannot be read; the caller turns that into a
/// failed module and leaves the rest of the report alone.
pub(super) fn write(
    report_dir: &Path,
    output_dir: &Path,
    settings: &Analysis,
    run_metadata: &AnalysisRunMetadata,
) -> Result<(), AnalysisError> {
    let dimensions = &settings.person_group_attributes;
    let outcomes = read_person_outcomes(report_dir)?;
    let demographics: BTreeMap<_, _> = run_metadata
        .person_demographics
        .iter()
        .map(|person| (person.person_id.as_str(), person))
        .collect();
    // The population is the group denominator. A person the events mention without a preserved
    // attribute is kept as `unknown` rather than leaving the groups, and a person with attributes
    // but no day in the report keeps its group size.
    let mut person_ids: BTreeSet<&str> = demographics.keys().copied().collect();
    person_ids.extend(outcomes.keys().map(String::as_str));
    let people: Vec<GroupedPerson> = person_ids
        .iter()
        .map(|person_id| {
            let demographic = demographics.get(person_id);
            GroupedPerson {
                person_id: (*person_id).to_owned(),
                groups: demographic.map_or_else(BTreeMap::new, |person| person.groups.clone()),
                weight: demographic.and_then(|person| person.weight).unwrap_or(1.0),
                weight_source: weight_source(
                    settings,
                    demographic.is_some_and(|person| person.weight.is_some()),
                ),
                cost: demographic.and_then(|person| person.cost),
                outcome: outcomes.get(*person_id).cloned(),
            }
        })
        .collect();

    write_person_demographics(report_dir, dimensions, &people)?;
    write_group_burdens(report_dir, dimensions, &people)?;
    write_module_outcomes(report_dir)?;
    write_equity_comparison(report_dir, output_dir, dimensions, &people, settings)
}

/// The header-only tables of an unavailable module, so the report can always render them.
pub(super) fn write_empty(report_dir: &Path) -> Result<(), AnalysisError> {
    for (name, header) in [
        (
            "person_demographics.csv",
            "person_id,dimension,group,weight,weight_source,monetary_cost\n",
        ),
        (
            "group_burdens.csv",
            "dimension,group,persons,weighted_persons,weighted_share,persons_with_default_weight,travelling_persons,defined_persons,incomplete_persons,mean_travel_time_seconds,median_travel_time_seconds,p90_travel_time_seconds,persons_with_cost,mean_monetary_cost\n",
        ),
        (
            "group_module_outcomes.csv",
            "dimension,group,source,metric,unit,value\n",
        ),
        (
            "equity_comparison.csv",
            "comparison_run,dimension,group,equity_criterion,baseline_persons,comparison_persons,comparable_persons,winners,unchanged,losers,baseline_only_persons,comparison_only_persons,not_comparable_persons,mean_baseline_travel_time_seconds,mean_comparison_travel_time_seconds,mean_travel_time_change_seconds\n",
        ),
    ] {
        std::fs::write(report_dir.join(name), header).map_err(io_error)?;
    }
    Ok(())
}

/// Why a person carries the weight the group tables report.
fn weight_source(settings: &Analysis, supplied: bool) -> &'static str {
    if settings.person_weight_attribute.is_none() {
        "unconfigured"
    } else if supplied {
        "supplied"
    } else {
        "default"
    }
}

/// One row per person and configured group dimension, with the weight and cost the run supplied.
fn write_person_demographics(
    report_dir: &Path,
    dimensions: &[String],
    people: &[GroupedPerson],
) -> Result<(), AnalysisError> {
    let mut writer = table_writer(report_dir, "person_demographics.csv")?;
    writeln!(
        writer,
        "person_id,dimension,group,weight,weight_source,monetary_cost"
    )
    .map_err(io_error)?;
    for person in people {
        for dimension in dimensions {
            let group = person.groups.get(dimension).map_or(UNKNOWN, String::as_str);
            writeln!(
                writer,
                "{},{},{},{:.6},{},{}",
                csv(&person.person_id),
                csv(dimension),
                csv(group),
                person.weight,
                person.weight_source,
                number_opt(person.cost)
            )
            .map_err(io_error)?;
        }
    }
    Ok(())
}

/// Group sizes and travel burdens per configured dimension.
fn write_group_burdens(
    report_dir: &Path,
    dimensions: &[String],
    people: &[GroupedPerson],
) -> Result<(), AnalysisError> {
    let mut writer = table_writer(report_dir, "group_burdens.csv")?;
    writeln!(
        writer,
        "dimension,group,persons,weighted_persons,weighted_share,persons_with_default_weight,travelling_persons,defined_persons,incomplete_persons,mean_travel_time_seconds,median_travel_time_seconds,p90_travel_time_seconds,persons_with_cost,mean_monetary_cost"
    )
    .map_err(io_error)?;
    for dimension in dimensions {
        let mut groups: BTreeMap<String, GroupTotals> = BTreeMap::new();
        for person in people {
            let group = person
                .groups
                .get(dimension)
                .cloned()
                .unwrap_or_else(|| UNKNOWN.to_owned());
            groups.entry(group).or_default().observe(person);
        }
        // The weighted share is taken over every person of the dimension, so the groups of one
        // dimension add up to one even when some of them hold no comparable outcome.
        let total_weight: f64 = groups.values().map(|totals| totals.weighted_persons).sum();
        for (group, totals) in groups {
            let share = if total_weight > 0.0 {
                totals.weighted_persons / total_weight
            } else {
                0.0
            };
            let mut travel_times = sorted(&totals.travel_times);
            let median = quantile(&travel_times, 0.5);
            let p90 = quantile(&travel_times, 0.9);
            travel_times.clear();
            writeln!(
                writer,
                "{},{},{},{:.6},{:.6},{},{},{},{},{},{},{},{},{}",
                csv(dimension),
                csv(&group),
                totals.persons,
                totals.weighted_persons,
                share,
                totals.persons_with_default_weight,
                totals.travelling_persons,
                totals.defined_persons,
                totals.incomplete_persons,
                number_opt(mean(&totals.travel_times)),
                number_opt(median),
                number_opt(p90),
                totals.costs.len(),
                number_opt(mean(&totals.costs)),
            )
            .map_err(io_error)?;
        }
    }
    Ok(())
}

/// Running totals of one group.
#[derive(Default)]
struct GroupTotals {
    persons: usize,
    weighted_persons: f64,
    persons_with_default_weight: usize,
    travelling_persons: usize,
    defined_persons: usize,
    incomplete_persons: usize,
    travel_times: Vec<f64>,
    costs: Vec<f64>,
}

impl GroupTotals {
    fn observe(&mut self, person: &GroupedPerson) {
        self.persons += 1;
        self.weighted_persons += person.weight;
        self.persons_with_default_weight += usize::from(person.weight_source != "supplied");
        match person
            .outcome
            .as_ref()
            .filter(|outcome| outcome.has_defined_burden())
        {
            Some(outcome) => {
                self.defined_persons += 1;
                self.travelling_persons += usize::from(outcome.completed_legs > 0);
                self.travel_times.push(outcome.travel_time_seconds);
            }
            // A person without a day in the report, and a person whose day did not finish, are
            // both retained in the group but contribute no burden mean.
            None => self.incomplete_persons += 1,
        }
        if let Some(cost) = person.cost {
            self.costs.push(cost);
        }
    }
}

fn sorted(values: &[f64]) -> Vec<f64> {
    let mut sorted = values.to_vec();
    sorted.sort_by(f64::total_cmp);
    sorted
}

/// Fold in the per-group outcomes other modules published, if any of them did.
fn write_module_outcomes(report_dir: &Path) -> Result<(), AnalysisError> {
    let mut writer = table_writer(report_dir, "group_module_outcomes.csv")?;
    writeln!(writer, "dimension,group,source,metric,unit,value").map_err(io_error)?;
    let mut sources: Vec<(String, PathBuf)> = std::fs::read_dir(report_dir)
        .map_err(io_error)?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let source = name.strip_suffix(GROUP_OUTCOME_SUFFIX)?;
            Some((source.to_owned(), entry.path()))
        })
        .collect();
    // The directory is read in whatever order the file system hands it back, so the fold-in is
    // sorted by module name to keep the table reproducible.
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    for (source, path) in sources {
        append_group_outcomes(&mut writer, &source, &path)?;
    }
    Ok(())
}

fn append_group_outcomes(
    writer: &mut impl Write,
    source: &str,
    path: &Path,
) -> Result<(), AnalysisError> {
    let mut reader = csv::Reader::from_path(path).map_err(|error| {
        AnalysisError::new(format!("could not read {}: {error}", path.display()))
    })?;
    let headers = reader
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .clone();
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "group outcome table {} is missing the {name} column",
                    path.display()
                ))
            })
    };
    let (dimension, group, metric, unit, value) = (
        index("dimension")?,
        index("group")?,
        index("metric")?,
        index("unit")?,
        index("value")?,
    );
    for row in reader.records() {
        let row = row.map_err(|error| AnalysisError::new(error.to_string()))?;
        writeln!(
            writer,
            "{},{},{},{},{},{}",
            csv(&row[dimension]),
            csv(&row[group]),
            csv(source),
            csv(&row[metric]),
            csv(&row[unit]),
            csv(&row[value])
        )
        .map_err(io_error)?;
    }
    Ok(())
}

/// Winner and loser counts per group against every configured comparison run.
fn write_equity_comparison(
    report_dir: &Path,
    output_dir: &Path,
    dimensions: &[String],
    people: &[GroupedPerson],
    settings: &Analysis,
) -> Result<(), AnalysisError> {
    let mut writer = table_writer(report_dir, "equity_comparison.csv")?;
    writeln!(
        writer,
        "comparison_run,dimension,group,equity_criterion,baseline_persons,comparison_persons,comparable_persons,winners,unchanged,losers,baseline_only_persons,comparison_only_persons,not_comparable_persons,mean_baseline_travel_time_seconds,mean_comparison_travel_time_seconds,mean_travel_time_change_seconds"
    )
    .map_err(io_error)?;
    for supplied in &settings.comparison_runs {
        let run_dir = if supplied.is_absolute() {
            supplied.clone()
        } else {
            output_dir.join(supplied)
        };
        let run_report = run_dir.join("analysis");
        let manifest_path = run_report.join("manifest.json");
        let manifest: Value =
            serde_json::from_reader(std::fs::File::open(&manifest_path).map_err(io_error)?)
                .map_err(|error| {
                    AnalysisError::new(format!(
                        "invalid run manifest {}: {error}",
                        manifest_path.display()
                    ))
                })?;
        if manifest.get("status").and_then(Value::as_str) != Some("complete") {
            return Err(AnalysisError::new(format!(
                "comparison run has no completed latest-iteration report: {}",
                run_report.display()
            )));
        }
        let comparison_outcomes = read_person_outcomes(&run_report)?;
        let comparison_groups = read_person_groups(&run_report)?;
        // Grouping has to mean the same thing in both runs, or a winner count would describe two
        // different populations. A comparison run that never grouped by a dimension the baseline
        // uses is rejected instead of being counted as `unknown`.
        for dimension in dimensions {
            if !comparison_groups
                .values()
                .any(|groups| groups.contains_key(dimension))
            {
                return Err(AnalysisError::new(format!(
                    "comparison run {} was not analyzed with the person group attribute '{dimension}'",
                    run_dir.display()
                )));
            }
        }
        let baseline_ids: BTreeSet<&str> = people
            .iter()
            .map(|person| person.person_id.as_str())
            .collect();
        let mut rows: BTreeMap<(String, String), ComparisonTotals> = BTreeMap::new();
        // Each side's population is counted on the rows that side groups it by, so a person who
        // changed group between the runs still appears in the size of both groups. Counting only
        // the baseline's grouping would make `comparison_persons` understate a group the
        // comparison run really has people in.
        for person in people {
            for key in group_keys(dimensions, &person.groups) {
                comparison_totals(&mut rows, key).baseline_persons += 1;
            }
        }
        for (person_id, groups) in &comparison_groups {
            let comparison_only = !baseline_ids.contains(person_id.as_str());
            for key in group_keys(dimensions, groups) {
                let row = comparison_totals(&mut rows, key);
                row.comparison_persons += 1;
                // A person the baseline does not have cannot be a winner or a loser; the count
                // keeps the population difference visible instead of shrinking the row.
                row.comparison_only_persons += usize::from(comparison_only);
            }
        }
        for person in people {
            // The person the baseline has and the comparison run does not cannot be compared
            // either, and is reported as a population difference rather than dropped.
            let Some(comparison_groups) = comparison_groups.get(&person.person_id) else {
                for key in group_keys(dimensions, &person.groups) {
                    comparison_totals(&mut rows, key).baseline_only_persons += 1;
                }
                continue;
            };
            let comparison_outcome = comparison_outcomes.get(&person.person_id);
            let baseline_defined = person
                .outcome
                .as_ref()
                .filter(|outcome| outcome.has_defined_burden());
            let comparison_defined =
                comparison_outcome.filter(|outcome| outcome.has_defined_burden());
            for (dimension, group) in group_keys(dimensions, &person.groups) {
                // A person grouped differently by the two runs is compared under neither group:
                // the group would not describe the same people on either side. Both rows count the
                // person as not comparable, so each row's exclusions add up to its own population.
                // The whole-population group describes everyone, so it always agrees.
                let comparison_group = if dimension == ALL {
                    group.clone()
                } else {
                    comparison_groups
                        .get(&dimension)
                        .cloned()
                        .unwrap_or_else(|| UNKNOWN.to_owned())
                };
                if comparison_group != group {
                    comparison_totals(&mut rows, (dimension.clone(), comparison_group))
                        .not_comparable_persons += 1;
                    comparison_totals(&mut rows, (dimension, group)).not_comparable_persons += 1;
                    continue;
                }
                let (Some(baseline), Some(comparison)) = (baseline_defined, comparison_defined)
                else {
                    comparison_totals(&mut rows, (dimension, group)).not_comparable_persons += 1;
                    continue;
                };
                comparison_totals(&mut rows, (dimension, group))
                    .observe(baseline.travel_time_seconds, comparison.travel_time_seconds);
            }
        }
        for ((dimension, group), totals) in rows {
            totals.write(&mut writer, supplied, &dimension, &group)?;
        }
    }
    Ok(())
}

fn comparison_totals(
    rows: &mut BTreeMap<(String, String), ComparisonTotals>,
    key: (String, String),
) -> &mut ComparisonTotals {
    rows.entry(key).or_default()
}

/// The group keys a person belongs to: one per configured dimension, plus the whole population.
fn group_keys(dimensions: &[String], groups: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut keys: Vec<(String, String)> = dimensions
        .iter()
        .map(|dimension| {
            (
                dimension.clone(),
                groups
                    .get(dimension)
                    .cloned()
                    .unwrap_or_else(|| UNKNOWN.to_owned()),
            )
        })
        .collect();
    keys.push((ALL.to_owned(), ALL.to_owned()));
    keys
}

/// Running totals of one group's comparison against one run.
#[derive(Default)]
struct ComparisonTotals {
    baseline_persons: usize,
    comparison_persons: usize,
    comparable_persons: usize,
    winners: usize,
    unchanged: usize,
    losers: usize,
    baseline_only_persons: usize,
    comparison_only_persons: usize,
    not_comparable_persons: usize,
    baseline_times: Vec<f64>,
    comparison_times: Vec<f64>,
}

impl ComparisonTotals {
    /// Classify one person both runs describe with a defined burden.
    fn observe(&mut self, baseline: f64, comparison: f64) {
        self.comparable_persons += 1;
        self.baseline_times.push(baseline);
        self.comparison_times.push(comparison);
        match (comparison - baseline).abs() <= UNCHANGED_TOLERANCE_SECONDS {
            true => self.unchanged += 1,
            false if comparison < baseline => self.winners += 1,
            false => self.losers += 1,
        }
    }

    fn write(
        &self,
        writer: &mut impl Write,
        run: &Path,
        dimension: &str,
        group: &str,
    ) -> Result<(), AnalysisError> {
        let baseline_mean = mean(&self.baseline_times);
        let comparison_mean = mean(&self.comparison_times);
        let change = baseline_mean
            .zip(comparison_mean)
            .map(|(baseline, comparison)| comparison - baseline);
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv(&run.display().to_string()),
            csv(dimension),
            csv(group),
            csv(EQUITY_CRITERION),
            self.baseline_persons,
            self.comparison_persons,
            self.comparable_persons,
            self.winners,
            self.unchanged,
            self.losers,
            self.baseline_only_persons,
            self.comparison_only_persons,
            self.not_comparable_persons,
            number_opt(baseline_mean),
            number_opt(comparison_mean),
            number_opt(change),
        )
        .map_err(io_error)
    }
}

/// Read the person outcomes the agent travel module published.
fn read_person_outcomes(
    report_dir: &Path,
) -> Result<BTreeMap<String, PersonOutcome>, AnalysisError> {
    let path = report_dir.join("person_daily.csv");
    let mut reader = csv::Reader::from_path(&path).map_err(|error| {
        AnalysisError::new(format!("could not read {}: {error}", path.display()))
    })?;
    let headers = reader
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .clone();
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| {
                AnalysisError::new(format!("missing {name} column in {}", path.display()))
            })
    };
    let person = index("person_id")?;
    let completed = index("completed_legs")?;
    let duration = index("completed_duration_sum_seconds")?;
    let status = index("completion_status")?;
    let mut outcomes = BTreeMap::new();
    for row in reader.records() {
        let row = row.map_err(|error| AnalysisError::new(error.to_string()))?;
        let travel_time_seconds = row[duration].parse::<f64>().map_err(|error| {
            AnalysisError::new(format!(
                "invalid completed_duration_sum_seconds in {}: {error}",
                path.display()
            ))
        })?;
        outcomes.insert(
            row[person].to_owned(),
            PersonOutcome {
                completed_legs: row[completed].parse().unwrap_or_default(),
                travel_time_seconds,
                status: row[status].to_owned(),
            },
        );
    }
    Ok(outcomes)
}

/// Read the person groups a comparison run published.
fn read_person_groups(
    report_dir: &Path,
) -> Result<BTreeMap<String, BTreeMap<String, String>>, AnalysisError> {
    let path = report_dir.join("person_demographics.csv");
    let mut reader = csv::Reader::from_path(&path).map_err(|error| {
        AnalysisError::new(format!("could not read {}: {error}", path.display()))
    })?;
    let headers = reader
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .clone();
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| {
                AnalysisError::new(format!("missing {name} column in {}", path.display()))
            })
    };
    let (person, dimension, group) = (index("person_id")?, index("dimension")?, index("group")?);
    let mut groups: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
    for row in reader.records() {
        let row = row.map_err(|error| AnalysisError::new(error.to_string()))?;
        groups
            .entry(row[person].to_owned())
            .or_default()
            .insert(row[dimension].to_owned(), row[group].to_owned());
    }
    Ok(groups)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
    use crate::simulation::io::xml::population::{IOPerson, IOPlan};
    use crate::simulation::scenario::population::Population;
    use macros::deterministic_id_test;
    use serde_json::json;

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

    /// A person carrying the given attributes, built through the same conversion the population
    /// readers use.
    fn person(id: &str, attributes: &[(&str, &str, &str)]) -> IOPerson {
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
                selected: true,
                score: None,
                elements: Vec::new(),
            }],
        }
    }

    fn population(persons: Vec<IOPerson>) -> Population {
        Population::from_persons(persons.into_iter().map(Into::into).collect())
    }

    #[deterministic_id_test]
    fn captures_supplied_attributes_and_defaults_unusable_weights() {
        let captured = capture(
            &population(vec![
                person(
                    "weighted",
                    &[
                        ("income", "java.lang.String", "low"),
                        ("age", "java.lang.Integer", "42"),
                        ("weight", "java.lang.Double", "2.5"),
                        ("cost", "java.lang.Double", "12.5"),
                    ],
                ),
                person("missing", &[("income", "java.lang.String", "high")]),
                person(
                    "blank",
                    &[
                        ("income", "java.lang.String", "  "),
                        ("weight", "java.lang.Double", "-1.0"),
                    ],
                ),
                person("textual", &[("weight", "java.lang.String", "heavy")]),
            ]),
            &settings(),
        );
        let by_id: BTreeMap<_, _> = captured
            .iter()
            .map(|person| (person.person_id.as_str(), person))
            .collect();
        assert_eq!(by_id["weighted"].groups["income"], "low");
        assert_eq!(by_id["weighted"].groups["age"], "42");
        assert_eq!(by_id["weighted"].weight, Some(2.5));
        assert_eq!(by_id["weighted"].cost, Some(12.5));
        // A person without the attribute is reported as unknown rather than dropped.
        assert_eq!(by_id["missing"].groups["age"], UNKNOWN);
        assert_eq!(by_id["missing"].weight, None);
        assert_eq!(by_id["missing"].cost, None);
        // Blank and unusable weights fall back to the default weight of one.
        assert_eq!(by_id["blank"].groups["income"], UNKNOWN);
        assert_eq!(by_id["blank"].weight, None);
        assert_eq!(by_id["textual"].weight, None);
        // Persons are captured in identifier order, so the recorded metadata is reproducible.
        let ids: Vec<_> = captured
            .iter()
            .map(|person| person.person_id.as_str())
            .collect();
        assert_eq!(ids, ["blank", "missing", "textual", "weighted"]);
    }

    #[test]
    fn group_labels_keep_scalars_and_report_everything_else_as_unknown() {
        assert_eq!(group_label(&json!("low")), "low");
        assert_eq!(group_label(&json!(7)), "7");
        assert_eq!(group_label(&json!(true)), "true");
        assert_eq!(group_label(&json!("")), UNKNOWN);
        assert_eq!(group_label(&json!("   ")), UNKNOWN);
        assert_eq!(group_label(&json!(null)), UNKNOWN);
        assert_eq!(group_label(&json!([1, 2])), UNKNOWN);
    }

    #[test]
    fn another_modules_group_outcomes_are_folded_in_when_it_publishes_them() {
        let dir = tempfile::tempdir().unwrap();
        // The scenario and accessibility modules publish their own group outcomes; a run without
        // them contributes nothing rather than an empty source.
        std::fs::write(
            dir.path().join("accessibility_group_outcomes.csv"),
            "dimension,group,metric,unit,value\n\"income\",\"low\",\"opportunities\",\"count\",1200\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("scenario_group_outcomes.csv"),
            "dimension,group,metric,unit,value\n\"income\",\"low\",\"fares\",\"currency\",4.5\n",
        )
        .unwrap();
        write_module_outcomes(dir.path()).unwrap();
        let folded = std::fs::read_to_string(dir.path().join("group_module_outcomes.csv")).unwrap();
        // Sources are folded in by module name, so the table does not depend on read order.
        assert_eq!(
            folded,
            "dimension,group,source,metric,unit,value\n\
             \"income\",\"low\",\"accessibility\",\"opportunities\",\"count\",\"1200\"\n\
             \"income\",\"low\",\"scenario\",\"fares\",\"currency\",\"4.5\"\n"
        );
    }

    #[test]
    fn a_group_outcome_table_without_its_columns_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("accessibility_group_outcomes.csv"),
            "dimension,group,opportunities\n\"income\",\"low\",1200\n",
        )
        .unwrap();
        let error = write_module_outcomes(dir.path())
            .err()
            .expect("a table without the agreed columns cannot be folded in");
        assert!(
            error.to_string().contains("missing the metric column"),
            "{error}"
        );
    }
}
