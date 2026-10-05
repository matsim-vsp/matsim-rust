//! Comparisons between reports from completed runs.

use super::{AnalysisError, Manifest, csv, escape_html, read_json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

#[derive(serde::Deserialize)]
struct CatalogMetric {
    name: String,
    unit: String,
    aggregation_key: String,
}

struct Run {
    path: PathBuf,
    manifest: Manifest,
    catalog: BTreeMap<String, CatalogMetric>,
}

/// Compare latest-iteration reports using an explicit baseline.
///
/// The inputs are run output directories, each of which must contain a complete `analysis`
/// report. The comparison is written to `<baseline>/analysis/comparison`; source reports are
/// left intact. Link rows use the common external link IDs, while group and person rows use the
/// aggregation keys advertised by each run's metric catalog.
pub fn compare_completed_runs(
    baseline_dir: &Path,
    alternative_dirs: &[PathBuf],
) -> Result<PathBuf, AnalysisError> {
    if alternative_dirs.is_empty() {
        return Err(AnalysisError::new(
            "comparison requires at least one alternative run",
        ));
    }
    let staging = baseline_dir.join("analysis/.comparison-staging");
    let published = baseline_dir.join("analysis/comparison");
    let backup = baseline_dir.join("analysis/.comparison-backup");
    super::reclaim_backup(&published, &backup)?;
    let baseline = read_run(baseline_dir)?;
    let runs = alternative_dirs
        .iter()
        .map(|path| read_run(path))
        .collect::<Result<Vec<_>, _>>()?;
    super::reset_staging(&staging)?;
    let result = write_comparison(&staging, &baseline, &runs);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    super::publish(&staging, &published, &backup).map(|published| published.join("index.html"))
}

fn read_run(path: &Path) -> Result<Run, AnalysisError> {
    let report = path.join("analysis");
    let manifest: Manifest = read_json(&report.join("manifest.json"))?;
    if manifest.status != "complete" {
        return Err(AnalysisError::new(format!(
            "run {} has no complete latest-iteration report",
            path.display()
        )));
    }
    let catalog: Vec<CatalogMetric> = read_json(&report.join("metric_catalog.json"))?;
    let catalog = catalog
        .into_iter()
        .map(|metric| (metric.name.clone(), metric))
        .collect();
    Ok(Run {
        path: path.to_path_buf(),
        manifest,
        catalog,
    })
}

fn write_comparison(
    path: &Path,
    baseline: &Run,
    alternatives: &[Run],
) -> Result<(), AnalysisError> {
    let mut file =
        BufWriter::new(File::create(path.join("metric_differences.csv")).map_err(io_error)?);
    writeln!(file, "alternative,table,metric,unit,aggregation_key,key,baseline_value,alternative_value,absolute_difference,relative_difference_percent,relative_baseline_denominator,baseline_aggregation_denominator,alternative_aggregation_denominator,status").map_err(io_error)?;
    let mut summary = Vec::new();
    for alternative in alternatives {
        validate_compatible(baseline, alternative)?;
        let matched_links = matching_link_ids(baseline, alternative)?;
        let same_network = matched_links.len() == baseline.manifest.eligible_links
            && matched_links.len() == alternative.manifest.eligible_links;
        let baseline_statuses = optional_person_statuses(baseline)?;
        let alternative_statuses = optional_person_statuses(alternative)?;
        let common_complete_people = baseline_statuses
            .iter()
            .filter_map(|(person, status)| {
                (status == "complete"
                    && alternative_statuses
                        .get(person)
                        .is_some_and(|other| other == "complete"))
                .then_some(person.clone())
            })
            .collect::<BTreeSet<_>>();
        let common_full_population = baseline_statuses
            .iter()
            .filter_map(|(person, status)| {
                (matches!(status.as_str(), "complete" | "no_travel")
                    && alternative_statuses
                        .get(person)
                        .is_some_and(|other| matches!(other.as_str(), "complete" | "no_travel")))
                .then_some(person.clone())
            })
            .collect::<BTreeSet<_>>();
        let baseline_departures = optional_person_departures(baseline)?;
        let alternative_departures = optional_person_departures(alternative)?;
        let common_travelers = common_complete_people
            .iter()
            .filter(|person| {
                baseline_departures
                    .get(*person)
                    .is_some_and(|count| *count > 0)
                    && alternative_departures
                        .get(*person)
                        .is_some_and(|count| *count > 0)
            })
            .cloned()
            .collect::<BTreeSet<_>>();
        let baseline_leg_values = optional_complete_leg_values(baseline, &common_complete_people)?;
        let alternative_leg_values =
            optional_complete_leg_values(alternative, &common_complete_people)?;
        let baseline_daily_values =
            optional_daily_cohort_values(baseline, &common_full_population, &common_travelers)?;
        let alternative_daily_values =
            optional_daily_cohort_values(alternative, &common_full_population, &common_travelers)?;
        let compatible = baseline
            .catalog
            .iter()
            .filter_map(|(name, metric)| {
                alternative
                    .catalog
                    .get(name)
                    .filter(|other| {
                        other.unit == metric.unit && other.aggregation_key == metric.aggregation_key
                    })
                    .map(|_| (name.as_str(), metric))
            })
            .collect::<BTreeMap<_, _>>();
        let specs = table_specs();
        let mut available = 0usize;
        for spec in specs {
            if !same_network
                && matches!(
                    spec.file,
                    "coverage.csv"
                        | "group_coverage.csv"
                        | "link_speed_summary.csv"
                        | "link_speed_histogram.csv"
                        | "vc_histogram.csv"
                )
            {
                continue;
            }
            let Some(headers) = first_headers(baseline, alternative, spec.file)? else {
                continue;
            };
            let metric_columns = if spec.file == "link_speed_diagnostics.csv" {
                compatible
                    .iter()
                    .filter(|(_, metric)| metric.aggregation_key == "metric")
                    .map(|(name, _)| (*name, "count"))
                    .collect::<Vec<_>>()
            } else {
                spec.metrics.to_vec()
            };
            for (catalog_name, metric_name) in metric_columns {
                let Some(metric) = compatible.get(catalog_name).copied() else {
                    continue;
                };
                let key_columns = metric
                    .aggregation_key
                    .split(',')
                    .map(|key| {
                        if key == "interval_start_seconds" && !headers.contains(key) {
                            "hour_start_seconds"
                        } else {
                            key
                        }
                    })
                    .collect::<Vec<_>>();
                if key_columns.iter().any(|key| !headers.contains(*key))
                    || !headers.contains(metric_name)
                {
                    continue;
                }
                let (mut left, mut right) = if spec.file == "leg_hourly.csv" {
                    (
                        baseline_leg_values
                            .get(catalog_name)
                            .cloned()
                            .unwrap_or_default(),
                        alternative_leg_values
                            .get(catalog_name)
                            .cloned()
                            .unwrap_or_default(),
                    )
                } else if spec.file == "daily_summary.csv" {
                    (
                        baseline_daily_values
                            .get(catalog_name)
                            .cloned()
                            .unwrap_or_default(),
                        alternative_daily_values
                            .get(catalog_name)
                            .cloned()
                            .unwrap_or_default(),
                    )
                } else {
                    (
                        read_rows(baseline, spec.file, &key_columns, metric_name)?,
                        read_rows(alternative, spec.file, &key_columns, metric_name)?,
                    )
                };
                if spec.file == "link_speed_diagnostics.csv" {
                    let key = serde_json::to_string(&[catalog_name]).map_err(io_error)?;
                    left.retain(|row_key, _| row_key == &key);
                    right.retain(|row_key, _| row_key == &key);
                }
                let keys = left
                    .keys()
                    .chain(right.keys())
                    .cloned()
                    .collect::<BTreeSet<_>>();
                for key in keys {
                    if matches!(
                        spec.file,
                        "link_hourly.csv" | "link_capacity.csv" | "link_speed_hourly.csv"
                    ) && !serde_json::from_str::<Vec<String>>(&key)
                        .ok()
                        .and_then(|parts| parts.first().cloned())
                        .is_some_and(|link_id| matched_links.contains(&link_id))
                    {
                        continue;
                    }
                    if spec.file == "person_daily.csv" {
                        let person = serde_json::from_str::<Vec<String>>(&key)
                            .ok()
                            .and_then(|parts| parts.first().cloned());
                        if !person.is_some_and(|person| common_complete_people.contains(&person)) {
                            continue;
                        }
                    }
                    let (mut before, mut after) =
                        (left.get(&key).copied(), right.get(&key).copied());
                    if spec.file == "leg_hourly.csv"
                        && matches!(catalog_name, "leg_departures" | "departing_persons")
                    {
                        before = Some(before.unwrap_or_default());
                        after = Some(after.unwrap_or_default());
                    }
                    // Completion filters exclude no-travel, stuck and incomplete people from
                    // person-level duration metrics; those outcomes remain visible in status counts.
                    let difference = before.zip(after).map(|(before, after)| after - before);
                    let relative = difference
                        .zip(before)
                        .filter(|(_, before)| *before != 0.0)
                        .map(|(difference, before)| difference / before * 100.0);
                    let status = match (before, after) {
                        (Some(0.0), Some(_)) => "zero_baseline",
                        (Some(_), Some(_)) => "comparable",
                        (Some(_), None) => "missing_alternative_observation",
                        (None, Some(_)) => "missing_baseline_observation",
                        (None, None) => continue,
                    };
                    let (baseline_denominator, alternative_denominator) =
                        if spec.file == "leg_hourly.csv" && catalog_name == "leg_duration_mean" {
                            (
                                baseline_leg_values
                                    .get("leg_departures")
                                    .and_then(|rows| rows.get(&key))
                                    .copied(),
                                alternative_leg_values
                                    .get("leg_departures")
                                    .and_then(|rows| rows.get(&key))
                                    .copied(),
                            )
                        } else if spec.file == "daily_summary.csv" {
                            let cohort = serde_json::from_str::<Vec<String>>(&key)
                                .ok()
                                .and_then(|parts| parts.first().cloned());
                            let people = match cohort.as_deref() {
                                Some("travelers") => common_travelers.len(),
                                _ => common_full_population.len(),
                            } as f64;
                            (Some(people), Some(people))
                        } else if let Some(column) = denominator_column(catalog_name) {
                            let before_rows = read_rows(baseline, spec.file, &key_columns, column)?;
                            let after_rows =
                                read_rows(alternative, spec.file, &key_columns, column)?;
                            (
                                before_rows.get(&key).copied(),
                                after_rows.get(&key).copied(),
                            )
                        } else {
                            (None, None)
                        };
                    writeln!(
                        file,
                        "{},{},{},{},{},{},{},{},{},{},{},{},{},{}",
                        csv(&alternative.path.display().to_string()),
                        spec.file,
                        catalog_name,
                        metric.unit,
                        csv(&metric.aggregation_key),
                        csv(&key),
                        number(before),
                        number(after),
                        number(difference),
                        number(relative),
                        number(before),
                        number(baseline_denominator),
                        number(alternative_denominator),
                        status
                    )
                    .map_err(io_error)?;
                    available += 1;
                }
            }
        }
        summary.push((
            alternative.path.display().to_string(),
            matched_links.len(),
            available,
            same_network,
        ));
    }
    file.flush().map_err(io_error)?;
    write_completion_status(path, baseline, alternatives)?;
    write_metric_compatibility(path, baseline, alternatives)?;
    let mut html = String::from(
        "<!doctype html><meta charset=\"utf-8\"><title>Scenario comparison</title><style>body{font:15px system-ui;margin:2rem;color:#17212b}table{border-collapse:collapse}td,th{border:1px solid #ccd;padding:.35rem}th{position:sticky;top:0;background:white}</style><h1>Baseline and alternative comparisons</h1><p>Differences use alternative minus baseline. Relative differences are blank for zero baselines. Link metrics include matching external link IDs only. Person duration comparisons use completed observations common to both runs.</p><ul>",
    );
    for (run, links, rows, same_network) in summary {
        html.push_str(&format!(
            "<li>{}: {links} corresponding links, {rows} comparable values; {}.</li>",
            escape_html(&run),
            if same_network {
                "same network"
            } else {
                "changed network; aggregate coverage metrics omitted"
            }
        ));
    }
    html.push_str("</ul><h2>Metric differences</h2><table><thead><tr><th>Alternative</th><th>Table</th><th>Metric</th><th>Unit</th><th>Aggregation key</th><th>Key</th><th>Baseline</th><th>Alternative</th><th>Difference</th><th>Relative (%)</th><th>Relative baseline denominator</th><th>Baseline aggregation denominator</th><th>Alternative aggregation denominator</th><th>Status</th></tr></thead><tbody>");
    let mut rows = csv::Reader::from_path(path.join("metric_differences.csv")).map_err(io_error)?;
    for row in rows.records() {
        let row = row.map_err(io_error)?;
        html.push_str("<tr>");
        for value in row.iter() {
            html.push_str(&format!("<td>{}</td>", escape_html(value)));
        }
        html.push_str("</tr>");
    }
    html.push_str("</tbody></table><p><a href=\"metric_differences.csv\">Download metric differences (CSV)</a></p>");
    for file in [
        "completion_status_differences.csv",
        "completion_status_transitions.csv",
        "leg_completion_status_transitions.csv",
        "metric_compatibility.csv",
    ] {
        html.push_str(&format!(
            "<h2>{}</h2><table>",
            match file {
                "completion_status_transitions.csv" => "Completion status transitions",
                "leg_completion_status_transitions.csv" => "Leg completion status transitions",
                "metric_compatibility.csv" => "Metric compatibility and availability",
                _ => "Completion status counts",
            }
        ));
        let mut rows = csv::Reader::from_path(path.join(file)).map_err(io_error)?;
        if let Some(headers) = rows.headers().map_err(io_error)?.iter().next() {
            let _ = headers;
        }
        let header = rows.headers().map_err(io_error)?.clone();
        html.push_str("<thead><tr>");
        for value in header.iter() {
            html.push_str(&format!("<th>{}</th>", escape_html(value)));
        }
        html.push_str("</tr></thead><tbody>");
        for row in rows.records() {
            html.push_str("<tr>");
            for value in row.map_err(io_error)?.iter() {
                html.push_str(&format!("<td>{}</td>", escape_html(value)));
            }
            html.push_str("</tr>");
        }
        html.push_str("</tbody></table>");
    }
    fs::write(path.join("index.html"), html).map_err(io_error)?;
    Ok(())
}

fn write_completion_status(
    path: &Path,
    baseline: &Run,
    alternatives: &[Run],
) -> Result<(), AnalysisError> {
    let mut counts = BufWriter::new(
        File::create(path.join("completion_status_differences.csv")).map_err(io_error)?,
    );
    let mut transitions = BufWriter::new(
        File::create(path.join("completion_status_transitions.csv")).map_err(io_error)?,
    );
    let mut leg_transitions = BufWriter::new(
        File::create(path.join("leg_completion_status_transitions.csv")).map_err(io_error)?,
    );
    writeln!(
        counts,
        "alternative,status,baseline_count,alternative_count,difference"
    )
    .map_err(io_error)?;
    writeln!(
        transitions,
        "alternative,baseline_status,alternative_status,persons"
    )
    .map_err(io_error)?;
    writeln!(
        leg_transitions,
        "alternative,person_id,leg_index,baseline_status,alternative_status"
    )
    .map_err(io_error)?;
    for alternative in alternatives {
        let base = optional_person_statuses(baseline)?;
        let other = optional_person_statuses(alternative)?;
        let mut base_counts = BTreeMap::<&str, usize>::new();
        let mut other_counts = BTreeMap::<&str, usize>::new();
        for status in base.values() {
            *base_counts.entry(status).or_default() += 1;
        }
        for status in other.values() {
            *other_counts.entry(status).or_default() += 1;
        }
        for status in ["complete", "no_travel", "incomplete", "stuck"] {
            let before = base_counts.get(status).copied().unwrap_or_default();
            let after = other_counts.get(status).copied().unwrap_or_default();
            writeln!(
                counts,
                "{},{status},{before},{after},{}",
                csv(&alternative.path.display().to_string()),
                after as isize - before as isize
            )
            .map_err(io_error)?;
        }
        let mut transitions_by_status = BTreeMap::<(&str, &str), usize>::new();
        for (person, before) in &base {
            if let Some(after) = other.get(person) {
                *transitions_by_status.entry((before, after)).or_default() += 1;
            }
        }
        for ((before, after), persons) in transitions_by_status {
            writeln!(
                transitions,
                "{},{before},{after},{persons}",
                csv(&alternative.path.display().to_string())
            )
            .map_err(io_error)?;
        }
        let base_legs = optional_leg_statuses(baseline)?;
        let other_legs = optional_leg_statuses(alternative)?;
        let leg_keys = base_legs
            .keys()
            .chain(other_legs.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for (person, leg_index) in leg_keys {
            writeln!(
                leg_transitions,
                "{},{},{},{},{}",
                csv(&alternative.path.display().to_string()),
                csv(&person),
                leg_index,
                base_legs
                    .get(&(person.clone(), leg_index.clone()))
                    .map(String::as_str)
                    .unwrap_or("missing"),
                other_legs
                    .get(&(person, leg_index.clone()))
                    .map(String::as_str)
                    .unwrap_or("missing")
            )
            .map_err(io_error)?;
        }
    }
    counts.flush().map_err(io_error)?;
    transitions.flush().map_err(io_error)?;
    leg_transitions.flush().map_err(io_error)?;
    Ok(())
}

fn leg_statuses(run: &Run) -> Result<BTreeMap<(String, String), String>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(run.path.join("analysis/legs.csv")).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| AnalysisError::new(format!("legs.csv has no {name} column")))
    };
    let (person_idx, leg_idx, status_idx) = (
        column("person_id")?,
        column("leg_index")?,
        column("status")?,
    );
    reader
        .records()
        .map(|row| {
            let row = row.map_err(io_error)?;
            Ok((
                (
                    row.get(person_idx).unwrap_or_default().to_owned(),
                    row.get(leg_idx).unwrap_or_default().to_owned(),
                ),
                row.get(status_idx).unwrap_or_default().to_owned(),
            ))
        })
        .collect()
}

fn write_metric_compatibility(
    path: &Path,
    baseline: &Run,
    alternatives: &[Run],
) -> Result<(), AnalysisError> {
    let mut writer =
        BufWriter::new(File::create(path.join("metric_compatibility.csv")).map_err(io_error)?);
    writeln!(writer, "alternative,metric,unit,baseline_aggregation_key,alternative_unit,alternative_aggregation_key,status").map_err(io_error)?;
    for alternative in alternatives {
        let matched_links = matching_link_ids(baseline, alternative)?;
        let same_network = matched_links.len() == baseline.manifest.eligible_links
            && matched_links.len() == alternative.manifest.eligible_links;
        let names = baseline
            .catalog
            .keys()
            .chain(alternative.catalog.keys())
            .cloned()
            .collect::<BTreeSet<_>>();
        for name in names {
            let before = baseline.catalog.get(&name);
            let after = alternative.catalog.get(&name);
            let diagnostic_metric = before
                .or(after)
                .is_some_and(|metric| metric.aggregation_key == "metric");
            let common_cohort_metric = matches!(
                name.as_str(),
                "leg_departures"
                    | "departing_persons"
                    | "leg_duration_mean"
                    | "daily_mean_completed_travel_burden"
            );
            let leg_status_metric = name == "leg_completion_status";
            let mapped = table_specs().iter().find_map(|spec| {
                spec.metrics
                    .iter()
                    .find(|(catalog_name, _)| *catalog_name == name)
                    .map(|(_, column)| (spec.file, *column))
            });
            let registered =
                mapped.is_some() || diagnostic_metric || common_cohort_metric || leg_status_metric;
            let output_file = mapped.map(|(file, _)| file).or_else(|| {
                if leg_status_metric {
                    Some("legs.csv")
                } else if diagnostic_metric {
                    Some("link_speed_diagnostics.csv")
                } else if common_cohort_metric && name == "daily_mean_completed_travel_burden" {
                    Some("daily_summary.csv")
                } else if common_cohort_metric {
                    Some("legs.csv")
                } else {
                    None
                }
            });
            let output_available = output_file.is_some_and(|file| {
                baseline.path.join("analysis").join(file).is_file()
                    && alternative.path.join("analysis").join(file).is_file()
            }) || leg_status_metric
                && baseline.path.join("analysis/legs.csv").is_file()
                && alternative.path.join("analysis/legs.csv").is_file();
            let headers = output_file
                .map(|file| first_headers(baseline, alternative, file))
                .transpose()?
                .flatten();
            let missing_columns = if let (Some((_, column)), Some(metric), Some(headers)) =
                (mapped, before, headers.as_ref())
            {
                let keys = metric.aggregation_key.split(',').map(|key| {
                    if key == "interval_start_seconds" && !headers.contains(key) {
                        "hour_start_seconds"
                    } else {
                        key
                    }
                });
                !headers.contains(column) || keys.into_iter().any(|key| !headers.contains(key))
            } else if diagnostic_metric {
                headers
                    .as_ref()
                    .is_none_or(|headers| !headers.contains("metric") || !headers.contains("count"))
            } else {
                false
            };
            let network_aggregate = mapped.is_some_and(|(file, _)| {
                matches!(
                    file,
                    "coverage.csv"
                        | "group_coverage.csv"
                        | "link_speed_summary.csv"
                        | "link_speed_histogram.csv"
                        | "vc_histogram.csv"
                )
            });
            let status = match (before, after) {
                (None, Some(_)) => "added_in_alternative",
                (Some(_), None) => "missing_in_alternative",
                (Some(left), Some(right))
                    if left.unit != right.unit || left.aggregation_key != right.aggregation_key =>
                {
                    "incompatible_definition"
                }
                (Some(_), Some(_)) if !registered => "not_registered_for_comparison",
                (Some(_), Some(_)) if !output_available => "unavailable_output",
                (Some(_), Some(_)) if !same_network && network_aggregate => "incompatible_network",
                (Some(_), Some(_)) if missing_columns => "missing_column",
                (Some(_), Some(_)) => "compatible",
                (None, None) => unreachable!(),
            };
            writeln!(
                writer,
                "{},{},{},{},{},{},{}",
                csv(&alternative.path.display().to_string()),
                csv(&name),
                csv(before
                    .map(|metric| metric.unit.as_str())
                    .unwrap_or_default()),
                csv(before
                    .map(|metric| metric.aggregation_key.as_str())
                    .unwrap_or_default()),
                csv(after.map(|metric| metric.unit.as_str()).unwrap_or_default()),
                csv(after
                    .map(|metric| metric.aggregation_key.as_str())
                    .unwrap_or_default()),
                csv(status)
            )
            .map_err(io_error)?;
        }
    }
    writer.flush().map_err(io_error)
}

fn person_statuses(run: &Run) -> Result<BTreeMap<String, String>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(run.path.join("analysis/person_daily.csv")).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let id = headers
        .iter()
        .position(|name| name == "person_id")
        .ok_or_else(|| AnalysisError::new("person_daily.csv has no person_id column"))?;
    let status = headers
        .iter()
        .position(|name| name == "completion_status")
        .ok_or_else(|| AnalysisError::new("person_daily.csv has no completion_status column"))?;
    reader
        .records()
        .map(|row| {
            let row = row.map_err(io_error)?;
            Ok((
                row.get(id).unwrap_or_default().to_owned(),
                row.get(status).unwrap_or_default().to_owned(),
            ))
        })
        .collect()
}

fn optional_person_statuses(run: &Run) -> Result<BTreeMap<String, String>, AnalysisError> {
    if run.path.join("analysis/person_daily.csv").is_file() {
        person_statuses(run)
    } else {
        Ok(BTreeMap::new())
    }
}

fn optional_leg_statuses(run: &Run) -> Result<BTreeMap<(String, String), String>, AnalysisError> {
    if run.path.join("analysis/legs.csv").is_file() {
        leg_statuses(run)
    } else {
        Ok(BTreeMap::new())
    }
}

fn optional_person_departures(run: &Run) -> Result<BTreeMap<String, usize>, AnalysisError> {
    if run.path.join("analysis/person_daily.csv").is_file() {
        person_departures(run)
    } else {
        Ok(BTreeMap::new())
    }
}

fn optional_complete_leg_values(
    run: &Run,
    common_complete_people: &BTreeSet<String>,
) -> Result<BTreeMap<&'static str, BTreeMap<String, f64>>, AnalysisError> {
    if run.path.join("analysis/legs.csv").is_file() {
        common_complete_leg_values(run, common_complete_people)
    } else {
        Ok(BTreeMap::new())
    }
}

fn optional_daily_cohort_values(
    run: &Run,
    all_complete: &BTreeSet<String>,
    travelers: &BTreeSet<String>,
) -> Result<BTreeMap<&'static str, BTreeMap<String, f64>>, AnalysisError> {
    if run.path.join("analysis/person_daily.csv").is_file() {
        common_daily_cohort_values(run, all_complete, travelers)
    } else {
        Ok(BTreeMap::new())
    }
}

fn validate_compatible(base: &Run, other: &Run) -> Result<(), AnalysisError> {
    let a = &base.manifest;
    let b = &other.manifest;
    if a.interval_seconds != b.interval_seconds || a.simulation_end_time != b.simulation_end_time {
        return Err(AnalysisError::new(
            "comparison runs use inconsistent interval widths or simulation end times",
        ));
    }
    if a.sample_size != b.sample_size {
        return Err(AnalysisError::new(
            "comparison runs use inconsistent sample-size scales",
        ));
    }
    if a.link_labels != b.link_labels || a.urban_boundary != b.urban_boundary {
        return Err(AnalysisError::new(
            "comparison runs use inconsistent link classifications or spatial filters",
        ));
    }
    Ok(())
}

fn matching_link_ids(a: &Run, b: &Run) -> Result<BTreeSet<String>, AnalysisError> {
    let left = read_column(&a.path.join("analysis/link_hourly.csv"), "link_id")?;
    let right = read_column(&b.path.join("analysis/link_hourly.csv"), "link_id")?;
    Ok(left.intersection(&right).cloned().collect())
}

fn first_headers(a: &Run, b: &Run, file: &str) -> Result<Option<BTreeSet<String>>, AnalysisError> {
    let left = a.path.join("analysis").join(file);
    let right = b.path.join("analysis").join(file);
    if !left.is_file() || !right.is_file() {
        return Ok(None);
    }
    let l = csv::Reader::from_path(left)
        .map_err(io_error)?
        .headers()
        .map_err(io_error)?
        .iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let r = csv::Reader::from_path(right)
        .map_err(io_error)?
        .headers()
        .map_err(io_error)?
        .iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    Ok(Some(l.intersection(&r).cloned().collect()))
}

fn read_rows(
    run: &Run,
    file: &str,
    keys: &[&str],
    metric: &str,
) -> Result<BTreeMap<String, f64>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(run.path.join("analysis").join(file)).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let Some(metric_index) = headers.iter().position(|h| h == metric) else {
        return Ok(BTreeMap::new());
    };
    let key_indices = keys
        .iter()
        .filter_map(|key| headers.iter().position(|h| h == *key))
        .collect::<Vec<_>>();
    let status_index = headers.iter().position(|h| h == "completion_status");
    let mut rows = BTreeMap::new();
    for row in reader.records() {
        let row = row.map_err(io_error)?;
        if status_index.is_some_and(|index| row.get(index) != Some("complete")) {
            continue;
        }
        let Some(value) = row
            .get(metric_index)
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite())
        else {
            continue;
        };
        let key = key_indices
            .iter()
            .map(|index| row.get(*index).unwrap_or_default())
            .collect::<Vec<_>>();
        let key = serde_json::to_string(&key).map_err(io_error)?;
        rows.insert(key, value);
    }
    Ok(rows)
}

#[derive(Default)]
struct CompleteLegGroup {
    departures: usize,
    persons: BTreeSet<String>,
    duration_sum: f64,
}

fn common_complete_leg_values(
    run: &Run,
    common_complete_people: &BTreeSet<String>,
) -> Result<BTreeMap<&'static str, BTreeMap<String, f64>>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(run.path.join("analysis/legs.csv")).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| AnalysisError::new(format!("legs.csv has no {name} column")))
    };
    let (person_idx, hour_idx, mode_idx, duration_idx, status_idx) = (
        column("person_id")?,
        column("departure_hour_seconds")?,
        column("mode")?,
        column("duration_seconds")?,
        column("status")?,
    );
    let mut groups = BTreeMap::<String, CompleteLegGroup>::new();
    for record in reader.records() {
        let record = record.map_err(io_error)?;
        let person = record.get(person_idx).unwrap_or_default();
        if record.get(status_idx) != Some("completed") || !common_complete_people.contains(person) {
            continue;
        }
        let Some(duration) = record
            .get(duration_idx)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        let key = serde_json::to_string(&[
            record.get(hour_idx).unwrap_or_default(),
            record.get(mode_idx).unwrap_or_default(),
        ])
        .map_err(io_error)?;
        let group = groups.entry(key).or_default();
        group.departures += 1;
        group.persons.insert(person.to_owned());
        group.duration_sum += duration;
    }
    let mut departures = BTreeMap::new();
    let mut persons = BTreeMap::new();
    let mut means = BTreeMap::new();
    for (key, group) in groups {
        departures.insert(key.clone(), group.departures as f64);
        persons.insert(key.clone(), group.persons.len() as f64);
        means.insert(key, group.duration_sum / group.departures as f64);
    }
    Ok(BTreeMap::from([
        ("leg_departures", departures),
        ("departing_persons", persons),
        ("leg_duration_mean", means),
    ]))
}

fn person_departures(run: &Run) -> Result<BTreeMap<String, usize>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(run.path.join("analysis/person_daily.csv")).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let person = headers
        .iter()
        .position(|name| name == "person_id")
        .ok_or_else(|| AnalysisError::new("person_daily.csv has no person_id column"))?;
    let departures = headers
        .iter()
        .position(|name| name == "departed_legs")
        .ok_or_else(|| AnalysisError::new("person_daily.csv has no departed_legs column"))?;
    reader
        .records()
        .map(|row| {
            let row = row.map_err(io_error)?;
            Ok((
                row.get(person).unwrap_or_default().to_owned(),
                row.get(departures)
                    .and_then(|value| value.parse().ok())
                    .unwrap_or_default(),
            ))
        })
        .collect()
}

fn common_daily_cohort_values(
    run: &Run,
    all_complete: &BTreeSet<String>,
    travelers: &BTreeSet<String>,
) -> Result<BTreeMap<&'static str, BTreeMap<String, f64>>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(run.path.join("analysis/person_daily.csv")).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let person_idx = headers
        .iter()
        .position(|name| name == "person_id")
        .ok_or_else(|| AnalysisError::new("person_daily.csv has no person_id column"))?;
    let sum_idx = headers
        .iter()
        .position(|name| name == "completed_duration_sum_seconds")
        .ok_or_else(|| AnalysisError::new("person_daily.csv has no completed duration column"))?;
    let mut sums = BTreeMap::new();
    for row in reader.records() {
        let row = row.map_err(io_error)?;
        let person = row.get(person_idx).unwrap_or_default().to_owned();
        let Some(sum) = row
            .get(sum_idx)
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
        else {
            continue;
        };
        sums.insert(person, sum);
    }
    let cohort_mean = |people: &BTreeSet<String>| {
        let values = people
            .iter()
            .filter_map(|person| sums.get(person))
            .collect::<Vec<_>>();
        if values.is_empty() {
            None
        } else {
            Some(values.iter().map(|value| **value).sum::<f64>() / values.len() as f64)
        }
    };
    let mut values = BTreeMap::new();
    for (cohort, people) in [
        ("all_complete_persons", all_complete),
        ("travelers", travelers),
    ] {
        if let Some(mean) = cohort_mean(people) {
            values.insert(serde_json::to_string(&[cohort]).map_err(io_error)?, mean);
        }
    }
    Ok(BTreeMap::from([(
        "daily_mean_completed_travel_burden",
        values,
    )]))
}

fn read_column(path: &Path, name: &str) -> Result<BTreeSet<String>, AnalysisError> {
    let mut reader = csv::Reader::from_path(path).map_err(io_error)?;
    let headers = reader.headers().map_err(io_error)?.clone();
    let Some(index) = headers.iter().position(|header| header == name) else {
        return Err(AnalysisError::new(format!(
            "{} has no {name} column",
            path.display()
        )));
    };
    reader
        .records()
        .map(|row| {
            row.map(|record| record.get(index).unwrap_or_default().to_owned())
                .map_err(io_error)
        })
        .collect()
}

struct TableSpec {
    file: &'static str,
    metrics: &'static [(&'static str, &'static str)],
}
fn table_specs() -> &'static [TableSpec] {
    &[
        TableSpec {
            file: "link_hourly.csv",
            metrics: &[
                ("entry_vehicles", "entry_vehicles"),
                ("exit_vehicles", "exit_vehicles"),
            ],
        },
        TableSpec {
            file: "group_coverage.csv",
            metrics: &[
                ("group_eligible_links", "eligible_links"),
                ("group_used_links", "used_links"),
                ("group_unused_links", "unused_links"),
                ("group_used_link_percent", "used_percent"),
            ],
        },
        TableSpec {
            file: "coverage.csv",
            metrics: &[
                ("eligible_links", "eligible_links"),
                ("used_links", "used_links"),
                ("unused_links", "unused_links"),
                ("used_percent", "used_percent"),
            ],
        },
        TableSpec {
            file: "leg_hourly.csv",
            metrics: &[
                ("leg_departures", "departures"),
                ("departing_persons", "departing_persons"),
                ("leg_duration_mean", "mean_duration_seconds"),
            ],
        },
        TableSpec {
            file: "daily_summary.csv",
            metrics: &[(
                "daily_mean_completed_travel_burden",
                "mean_completed_leg_duration_sum_seconds",
            )],
        },
        TableSpec {
            file: "transit_trips.csv",
            metrics: &[
                ("wait_seconds", "wait_seconds"),
                ("in_vehicle_seconds", "in_vehicle_seconds"),
                ("arrival_delay_seconds", "arrival_delay_seconds"),
            ],
        },
        TableSpec {
            file: "transit_stop_hourly.csv",
            metrics: &[
                ("boardings_sample", "boardings_sample"),
                ("alightings_sample", "alightings_sample"),
                ("boardings", "boardings"),
                ("alightings", "alightings"),
            ],
        },
        TableSpec {
            file: "transit_line_summary.csv",
            metrics: &[
                ("trips_sample", "trips_sample"),
                ("trips", "trips"),
                ("missed_services_sample", "missed_services_sample"),
                ("wait_observations", "wait_observations"),
                ("mean_wait_seconds", "mean_wait_seconds"),
                ("in_vehicle_observations", "in_vehicle_observations"),
                ("mean_in_vehicle_seconds", "mean_in_vehicle_seconds"),
                ("delay_observations", "delay_observations"),
                ("mean_arrival_delay_seconds", "mean_arrival_delay_seconds"),
            ],
        },
        TableSpec {
            file: "transit_outcomes.csv",
            metrics: &[
                ("outcome_trips_sample", "outcome_trips_sample"),
                ("outcome_trips", "outcome_trips"),
            ],
        },
        TableSpec {
            file: "transit_occupancy.csv",
            metrics: &[
                ("passengers_sample", "passengers_sample"),
                ("passengers", "passengers"),
                ("capacity_persons", "capacity_persons"),
                ("load_factor", "load_factor"),
            ],
        },
        TableSpec {
            file: "transit_journeys.csv",
            metrics: &[
                ("transit_legs", "transit_legs"),
                ("transfers", "transfers"),
                ("access_seconds", "access_seconds"),
                ("egress_seconds", "egress_seconds"),
                ("transfer_seconds", "transfer_seconds"),
                ("journey_wait_seconds", "journey_wait_seconds"),
                ("journey_in_vehicle_seconds", "journey_in_vehicle_seconds"),
            ],
        },
        TableSpec {
            file: "transit_validation_matches.csv",
            metrics: &[
                ("transit_observed", "observed"),
                ("transit_simulated_sample", "transit_simulated_sample"),
                ("transit_simulated_expanded", "transit_simulated_expanded"),
                ("transit_residual", "transit_residual"),
                ("transit_relative_error", "transit_relative_error"),
                (
                    "transit_network_total_expanded",
                    "transit_network_total_expanded",
                ),
            ],
        },
        TableSpec {
            file: "transit_validation_summary.csv",
            metrics: &[
                ("transit_matched", "transit_matched"),
                ("transit_unmatched", "transit_unmatched"),
                ("transit_observed_total", "transit_observed_total"),
                ("transit_simulated_total", "transit_simulated_total"),
                ("transit_bias", "transit_bias"),
                ("transit_mae", "transit_mae"),
                ("transit_rmse", "transit_rmse"),
                ("transit_relative_bias", "transit_relative_bias"),
            ],
        },
        TableSpec {
            file: "link_speed_hourly.csv",
            metrics: &[
                ("link_speed_traversals", "observations"),
                ("link_total_distance", "total_distance_meters"),
                ("link_total_duration", "total_duration_seconds"),
                ("link_representative_speed", "representative_speed_mps"),
                ("link_vehicle_speed_mean", "vehicle_speed_mean_mps"),
                (
                    "link_vehicle_speed_population_std",
                    "vehicle_speed_population_std_mps",
                ),
            ],
        },
        TableSpec {
            file: "link_speed_summary.csv",
            metrics: &[
                ("links_with_speed", "links_with_speed"),
                ("hourly_link_speed_traversals", "observations"),
                ("hourly_mean_link_speed", "mean_link_speed_mps"),
                (
                    "hourly_link_speed_population_std",
                    "population_std_link_speed_mps",
                ),
            ],
        },
        TableSpec {
            file: "link_speed_histogram.csv",
            metrics: &[
                ("speed_histogram_link_count", "link_count"),
                ("speed_histogram_observation_count", "observation_count"),
            ],
        },
        TableSpec {
            file: "link_capacity.csv",
            metrics: &[
                ("capacity_pce_per_hour", "capacity_pce_per_hour"),
                ("effective_capacity_pce", "effective_capacity_pce"),
                ("entry_pce", "entry_pce"),
                ("exit_pce", "exit_pce"),
                ("entry_pce_scaled", "entry_pce_scaled"),
                ("exit_pce_scaled", "exit_pce_scaled"),
                ("entry_flow_pce_per_hour", "entry_flow_pce_per_hour"),
                ("exit_flow_pce_per_hour", "exit_flow_pce_per_hour"),
                ("entry_vc", "entry_vc"),
                ("exit_vc", "exit_vc"),
                ("entry_unresolved_pce", "entry_unresolved_pce"),
                ("exit_unresolved_pce", "exit_unresolved_pce"),
            ],
        },
        TableSpec {
            file: "vc_histogram.csv",
            metrics: &[
                ("links", "links"),
                ("observations", "observations"),
                ("unavailable_links", "unavailable_links"),
            ],
        },
        TableSpec {
            file: "link_speed_diagnostics.csv",
            metrics: &[],
        },
        TableSpec {
            file: "person_daily.csv",
            metrics: &[
                (
                    "person_completed_leg_duration_sum",
                    "completed_duration_sum_seconds",
                ),
                (
                    "person_completed_leg_duration_mean",
                    "completed_duration_mean_seconds",
                ),
            ],
        },
        TableSpec {
            file: "service_summary.csv",
            metrics: &[
                ("requests", "requests"),
                ("served", "served"),
                ("rejected", "rejected"),
                ("unserved", "unserved"),
                ("served_share", "served_share"),
                ("rejected_share", "rejected_share"),
                ("passengers_served", "passengers_served"),
                ("wait_mean_seconds", "wait_mean_seconds"),
                ("wait_std_seconds", "wait_std_seconds"),
                ("wait_median_seconds", "wait_median_seconds"),
                ("wait_p90_seconds", "wait_p90_seconds"),
                ("detour_mean_ratio", "detour_mean_ratio"),
                ("detour_std_ratio", "detour_std_ratio"),
                ("detour_median_ratio", "detour_median_ratio"),
                ("detour_p90_ratio", "detour_p90_ratio"),
                ("wait_limit_exceeded", "wait_limit_exceeded"),
                ("inside_area", "inside_area"),
                ("outside_area", "outside_area"),
                ("area_unknown", "area_unknown"),
                ("coverage_share", "coverage_share"),
            ],
        },
        TableSpec {
            file: "service_vehicles.csv",
            metrics: &[
                ("service_seconds", "service_seconds"),
                ("busy_seconds", "busy_seconds"),
                ("utilization", "utilization"),
                ("driven_meters", "driven_meters"),
                ("occupied_meters", "occupied_meters"),
                ("empty_meters", "empty_meters"),
                ("empty_share", "empty_share"),
                ("passenger_meters", "passenger_meters"),
                ("mean_occupancy", "mean_occupancy"),
                ("load_factor", "load_factor"),
                ("capacity_exceeded_tasks", "capacity_exceeded_tasks"),
                ("requests_served", "requests_served"),
            ],
        },
        TableSpec {
            file: "service_occupancy.csv",
            metrics: &[
                ("load_vehicle_meters", "load_vehicle_meters"),
                ("load_share", "load_share"),
            ],
        },
        TableSpec {
            file: "emissions_hourly.csv",
            metrics: &[("emissions_total_expanded", "total_expanded")],
        },
    ]
}

fn number(value: Option<f64>) -> String {
    value.map_or_else(String::new, |number| format!("{number:.6}"))
}
fn denominator_column(metric: &str) -> Option<&'static str> {
    match metric {
        "used_percent" | "group_used_link_percent" => Some("eligible_links"),
        "entry_vc" | "exit_vc" => Some("effective_capacity_pce"),
        "link_representative_speed" => Some("total_duration_seconds"),
        "link_vehicle_speed_mean" | "link_vehicle_speed_population_std" => Some("observations"),
        "hourly_mean_link_speed" | "hourly_link_speed_population_std" => Some("links_with_speed"),
        "person_completed_leg_duration_mean" => Some("completed_legs"),
        "mean_wait_seconds" => Some("wait_observations"),
        "mean_in_vehicle_seconds" => Some("in_vehicle_observations"),
        "mean_arrival_delay_seconds" => Some("delay_observations"),
        "load_factor" => Some("capacity_persons"),
        "transit_relative_error" => Some("observed"),
        "transit_relative_bias" => Some("transit_observed_total"),
        _ => None,
    }
}
fn io_error(error: impl std::fmt::Display) -> AnalysisError {
    AnalysisError::new(error.to_string())
}

pub(super) fn write(
    output_dir: &Path,
    report_dir: &Path,
    supplied_runs: &[PathBuf],
) -> Result<(), AnalysisError> {
    let mut writer = csv::Writer::from_path(report_dir.join("cross_run_comparison.csv"))
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    writer
        .write_record([
            "run",
            "iteration",
            "metric",
            "link_id",
            "period_start_seconds",
            "period_end_seconds",
            "vehicle_class",
            "simulated_sample",
            "sample_size",
            "population_value",
            "unit",
        ])
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    for supplied in supplied_runs {
        let run_dir = if supplied.is_absolute() {
            supplied.clone()
        } else {
            output_dir.join(supplied)
        };
        let latest_report = run_dir.join("analysis");
        let manifest_path = latest_report.join("manifest.json");
        let manifest: serde_json::Value = serde_json::from_reader(
            File::open(&manifest_path).map_err(io_error)?,
        )
        .map_err(|error| {
            AnalysisError::new(format!(
                "invalid run manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
        if manifest.get("status").and_then(serde_json::Value::as_str) != Some("complete") {
            return Err(AnalysisError::new(format!(
                "comparison run has no completed latest-iteration report: {}",
                latest_report.display()
            )));
        }
        let iteration = manifest
            .get("iteration")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "manifest has no iteration: {}",
                    manifest_path.display()
                ))
            })?;
        let sample_size = manifest
            .get("sample_size")
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "manifest has invalid sample_size: {}",
                    manifest_path.display()
                ))
            })?;
        let interval_seconds = manifest
            .get("interval_seconds")
            .and_then(serde_json::Value::as_u64)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "manifest has invalid interval_seconds: {}",
                    manifest_path.display()
                ))
            })?;
        append_table(
            &mut writer,
            &run_dir,
            &latest_report,
            iteration,
            sample_size,
            interval_seconds,
            "link_hourly.csv",
            "entry_vehicles",
            "all",
            "vehicles",
        )?;
        append_table(
            &mut writer,
            &run_dir,
            &latest_report,
            iteration,
            sample_size,
            interval_seconds,
            "link_speed_hourly.csv",
            "representative_speed_mps",
            "all",
            "m/s",
        )?;
        append_table(
            &mut writer,
            &run_dir,
            &latest_report,
            iteration,
            sample_size,
            interval_seconds,
            "link_hourly_by_class.csv",
            "entry_vehicles",
            "vehicle_class",
            "vehicles",
        )?;
        append_table(
            &mut writer,
            &run_dir,
            &latest_report,
            iteration,
            sample_size,
            interval_seconds,
            "link_speed_by_class.csv",
            "representative_speed_mps",
            "vehicle_class",
            "m/s",
        )?;
    }
    writer.flush().map_err(io_error)?;
    Ok(())
}

fn append_table(
    writer: &mut csv::Writer<File>,
    run_dir: &Path,
    report_dir: &Path,
    iteration: u64,
    sample_size: f64,
    interval_seconds: u64,
    filename: &str,
    value_column: &str,
    class_column: &str,
    unit: &str,
) -> Result<(), AnalysisError> {
    let path = report_dir.join(filename);
    let mut reader = csv::Reader::from_path(&path).map_err(|error| {
        AnalysisError::new(format!("could not read {}: {error}", path.display()))
    })?;
    let headers = reader
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .clone();
    let link_index = column(&headers, "link_id", &path)?;
    let period_index = column(&headers, "hour_start_seconds", &path)?;
    let value_index = column(&headers, value_column, &path)?;
    let class_index = if class_column == "all" {
        None
    } else {
        Some(column(&headers, class_column, &path)?)
    };
    for (row_index, row) in reader.records().enumerate() {
        let row = row.map_err(|error| AnalysisError::new(error.to_string()))?;
        let row_number = row_index + 2;
        let period_start = row
            .get(period_index)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "invalid hour_start_seconds in {} row {row_number}",
                    path.display()
                ))
            })?;
        let raw_value = row.get(value_index).unwrap_or_default();
        if raw_value.is_empty() && value_column != "entry_vehicles" {
            continue;
        }
        let value = raw_value
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite())
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "invalid {value_column} in {} row {row_number}",
                    path.display()
                ))
            })?;
        let class = class_index.map_or("all", |index| row.get(index).unwrap_or(""));
        if class_index.is_some() && class == "all" {
            continue;
        }
        writer
            .write_record([
                run_dir.display().to_string(),
                iteration.to_string(),
                value_column.to_owned(),
                row[link_index].to_owned(),
                period_start.to_string(),
                period_start.saturating_add(interval_seconds).to_string(),
                class.to_owned(),
                value.to_string(),
                sample_size.to_string(),
                if value_column == "entry_vehicles" {
                    (value / sample_size).to_string()
                } else {
                    value.to_string()
                },
                unit.to_owned(),
            ])
            .map_err(|error| AnalysisError::new(error.to_string()))?;
    }
    Ok(())
}

fn column(headers: &csv::StringRecord, name: &str, path: &Path) -> Result<usize, AnalysisError> {
    headers
        .iter()
        .position(|header| header == name)
        .ok_or_else(|| AnalysisError::new(format!("missing {name} column in {}", path.display())))
}

pub(super) fn write_empty(path: &Path) -> Result<(), AnalysisError> {
    fs::write(
        path.join("cross_run_comparison.csv"),
        "run,iteration,metric,link_id,period_start_seconds,period_end_seconds,vehicle_class,simulated_sample,sample_size,population_value,unit\n",
    )
    .map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, name: &str, sample: f64, links: &str) -> PathBuf {
        let output = root.join(name);
        let analysis = output.join("analysis");
        fs::create_dir_all(&analysis).unwrap();
        let eligible_links = links.lines().filter(|line| !line.is_empty()).count();
        fs::write(analysis.join("manifest.json"), format!(r#"{{"status":"complete","failure":null,"iteration":3,"interval_seconds":3600,"simulation_end_time":3600,"partitions":[0],"input_format":"xml","eligible_links":{eligible_links},"random_seed":1,"sample_size":{sample},"network_input":null,"population_input":null,"software_version":"test"}}"#)).unwrap();
        fs::write(analysis.join("metric_catalog.json"), r#"[{"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"},{"name":"entry_pce_scaled","unit":"pce","aggregation_key":"link_id,interval_start_seconds"},{"name":"entry_vc","unit":"ratio","aggregation_key":"link_id,interval_start_seconds"},{"name":"used_percent","unit":"percent","aggregation_key":"interval_start_seconds"}]"#).unwrap();
        fs::write(
            analysis.join("link_hourly.csv"),
            format!("link_id,hour_start_seconds,entry_vehicles,exit_vehicles\n{links}"),
        )
        .unwrap();
        let mut capacity = String::from(
            "link_id,interval_start_seconds,entry_pce_scaled,exit_pce_scaled,entry_vc,exit_vc\n",
        );
        for row in links.lines() {
            let fields = row.split(',').collect::<Vec<_>>();
            if fields.len() == 4 {
                let entries = fields[2].parse::<f64>().unwrap();
                capacity.push_str(&format!(
                    "{},{},{entries:.6},0,{:.6},0\n",
                    fields[0],
                    fields[1],
                    entries / 100.0
                ));
            }
        }
        fs::write(analysis.join("link_capacity.csv"), capacity).unwrap();
        let used = links
            .lines()
            .filter(|line| {
                line.split(',')
                    .nth(2)
                    .and_then(|value| value.parse::<u64>().ok())
                    .unwrap_or_default()
                    > 0
            })
            .count();
        let used_percent = if eligible_links == 0 {
            0.0
        } else {
            used as f64 * 100.0 / eligible_links as f64
        };
        fs::write(analysis.join("coverage.csv"), format!("hour_start_seconds,eligible_links,used_links,unused_links,used_percent\n0,{eligible_links},{used},{},{used_percent:.6}\n", eligible_links - used)).unwrap();
        fs::write(
            analysis.join("person_daily.csv"),
            "person_id,expected_legs,departed_legs,completed_legs,completed_duration_sum_seconds,completed_duration_mean_seconds,completion_status\n",
        )
        .unwrap();
        fs::write(
            analysis.join("legs.csv"),
            "person_id,leg_index,mode,departure_seconds,departure_hour_seconds,arrival_seconds,duration_seconds,status\n",
        )
        .unwrap();
        fs::write(analysis.join("leg_hourly.csv"), "departure_hour_seconds,mode,departures,departing_persons,completed_legs,mean_duration_seconds\n").unwrap();
        fs::write(
            analysis.join("daily_summary.csv"),
            "cohort,persons,mean_completed_leg_duration_sum_seconds\n",
        )
        .unwrap();
        fs::write(
            analysis.join("link_speed_diagnostics.csv"),
            "metric,count\n",
        )
        .unwrap();
        output
    }

    #[test]
    fn exports_alternative_differences_and_marks_zero_baselines() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\nl2,0,0,0\n");
        let alternative = run(
            temp.path(),
            "alternative",
            1.0,
            "l1,0,15,0\nl2,0,3,0\nl3,0,99,0\n",
        );
        let report = compare_completed_runs(&baseline, &[alternative]).unwrap();
        let csv =
            fs::read_to_string(report.parent().unwrap().join("metric_differences.csv")).unwrap();
        assert!(csv.contains("entry_vehicles"));
        assert!(csv.contains("entry_pce_scaled"));
        assert!(csv.contains("entry_vc"));
        assert!(csv.contains("10.000000,15.000000,5.000000,50.000000,10.000000,,,comparable"));
        assert!(csv.contains("0.000000,3.000000,3.000000,,0.000000,,,zero_baseline"));
        assert!(
            !csv.contains("99.000000"),
            "non-corresponding network links must be excluded"
        );
        let html = fs::read_to_string(&report).unwrap();
        assert!(html.contains("changed network; aggregate coverage metrics omitted"));
        assert!(
            html.contains("10.000000"),
            "the local report renders numeric differences"
        );
        let compatibility =
            fs::read_to_string(report.parent().unwrap().join("metric_compatibility.csv")).unwrap();
        assert!(
            compatibility.contains("used_percent")
                && compatibility.contains("incompatible_network")
        );
    }

    #[test]
    fn rejects_inconsistent_sample_scales() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\n");
        let alternative = run(temp.path(), "alternative", 0.5, "l1,0,20,0\n");
        let error = compare_completed_runs(&baseline, &[alternative]).unwrap_err();
        assert!(error.to_string().contains("sample-size scales"));
    }

    #[test]
    fn missing_agent_reports_do_not_block_link_comparisons_and_speed_uses_duration_denominator() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\n");
        let alternative = run(temp.path(), "alternative", 1.0, "l1,0,15,0\n");
        for output in [&baseline, &alternative] {
            let analysis = output.join("analysis");
            fs::remove_file(analysis.join("person_daily.csv")).unwrap();
            fs::remove_file(analysis.join("legs.csv")).unwrap();
            fs::write(
                analysis.join("metric_catalog.json"),
                r#"[{"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"},{"name":"link_representative_speed","unit":"m/s","aggregation_key":"link_id,hour_start_seconds"},{"name":"person_completed_leg_duration_sum","unit":"seconds","aggregation_key":"person_id"}]"#,
            )
            .unwrap();
            fs::write(
                analysis.join("link_speed_hourly.csv"),
                "link_id,hour_start_seconds,representative_speed_mps,total_duration_seconds,observations\nl1,0,10,30,2\n",
            )
            .unwrap();
        }

        let report = compare_completed_runs(&baseline, std::slice::from_ref(&alternative)).unwrap();
        let differences =
            fs::read_to_string(report.parent().unwrap().join("metric_differences.csv")).unwrap();
        assert!(differences.contains("link_representative_speed"));
        assert!(differences.contains(",30.000000,30.000000,comparable"));
        let compatibility =
            fs::read_to_string(report.parent().unwrap().join("metric_compatibility.csv")).unwrap();
        assert!(compatibility.contains("unavailable_output"));
    }

    #[test]
    fn compares_transit_metrics_from_each_latest_iteration_report() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\n");
        let alternative = run(temp.path(), "alternative", 1.0, "l1,0,15,0\n");
        for (output, boardings, passengers, missed, expanded) in
            [(&baseline, 4, 4, 1, 4), (&alternative, 7, 6, 2, 7)]
        {
            let analysis = output.join("analysis");
            fs::write(
                analysis.join("metric_catalog.json"),
                r#"[{"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"},{"name":"boardings","unit":"persons","aggregation_key":"hour_start_seconds,line_id,stop_id"},{"name":"wait_seconds","unit":"seconds","aggregation_key":"person_id,departure_seconds"},{"name":"passengers","unit":"persons","aggregation_key":"line_id,route_id,departure_id,segment_index"},{"name":"load_factor","unit":"ratio","aggregation_key":"line_id,route_id,departure_id,segment_index"},{"name":"outcome_trips","unit":"trips","aggregation_key":"hour_start_seconds,service_modeling,outcome"},{"name":"transit_observed","unit":"persons","aggregation_key":"scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row"},{"name":"transit_simulated_expanded","unit":"persons","aggregation_key":"scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row"}]"#,
            )
            .unwrap();
            fs::write(
                analysis.join("transit_stop_hourly.csv"),
                format!(
                    "hour_start_seconds,line_id,stop_id,boardings\n0,Blue,stop-1,{boardings}\n"
                ),
            )
            .unwrap();
            fs::write(
                analysis.join("transit_trips.csv"),
                "person_id,departure_seconds,wait_seconds\np1,100,20\n",
            )
            .unwrap();
            fs::write(
                analysis.join("transit_occupancy.csv"),
                format!("line_id,route_id,departure_id,segment_index,passengers,capacity_persons,load_factor\nBlue,r,dep1,0,{passengers},8,{:.2}\n", passengers as f64 / 8.0),
            )
            .unwrap();
            fs::write(
                analysis.join("transit_outcomes.csv"),
                format!("hour_start_seconds,service_modeling,outcome,outcome_trips\n0,teleported,missed_service,{missed}\n"),
            )
            .unwrap();
            fs::write(
                analysis.join("transit_validation_matches.csv"),
                format!("scope,line_id,stop_id,station_id,period_start_seconds,metric,source_row,observed,transit_simulated_expanded\nstop,Blue,stop-1,,0,boardings,2,10,{expanded}\n"),
            )
            .unwrap();
        }

        let report = compare_completed_runs(&baseline, &[alternative]).unwrap();
        let differences =
            fs::read_to_string(report.parent().unwrap().join("metric_differences.csv")).unwrap();
        assert!(differences.contains("transit_stop_hourly.csv,boardings,persons"));
        assert!(differences.contains("4.000000,7.000000,3.000000,75.000000,4.000000,,,comparable"));
        assert!(differences.contains("transit_trips.csv,wait_seconds,seconds"));
        assert!(differences.contains("transit_outcomes.csv,outcome_trips,trips"));
        assert!(differences.contains("transit_validation_matches.csv,transit_observed"));
        assert!(differences.contains("transit_validation_matches.csv,transit_simulated_expanded"));
        assert!(differences.contains(
            "0.500000,0.750000,0.250000,50.000000,0.500000,8.000000,8.000000,comparable"
        ));
    }

    #[test]
    fn reports_missing_metrics_and_rejects_changed_filters() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\nl2,0,0,0\n");
        let alternative = run(temp.path(), "alternative", 1.0, "l1,0,10,0\nl2,0,0,0\n");
        fs::write(alternative.join("analysis/metric_catalog.json"), "[]").unwrap();
        let report = compare_completed_runs(&baseline, std::slice::from_ref(&alternative)).unwrap();
        let compatibility =
            fs::read_to_string(report.parent().unwrap().join("metric_compatibility.csv")).unwrap();
        assert!(compatibility.contains("entry_vehicles"));
        assert!(compatibility.contains("missing_in_alternative"));

        let manifest_path = alternative.join("analysis/manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        manifest["link_labels"] = serde_json::json!({"l1":{"road_type":"expressway"}});
        fs::write(manifest_path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let error = compare_completed_runs(&baseline, &[alternative]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("classifications or spatial filters")
        );
    }

    #[test]
    fn compares_group_and_common_complete_person_rows() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\nl2,0,0,0\n");
        let alternative = run(temp.path(), "alternative", 1.0, "l1,0,10,0\nl2,0,0,0\n");
        let catalog = r#"[
            {"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"},
            {"name":"group_used_links","unit":"links","aggregation_key":"dimension,category,hour_start_seconds"},
            {"name":"person_completed_leg_duration_sum","unit":"seconds","aggregation_key":"person_id"},
            {"name":"leg_departures","unit":"legs","aggregation_key":"departure_hour_seconds,mode"},
            {"name":"departing_persons","unit":"persons","aggregation_key":"departure_hour_seconds,mode"},
            {"name":"leg_duration_mean","unit":"seconds","aggregation_key":"departure_hour_seconds,mode"},
            {"name":"daily_mean_completed_travel_burden","unit":"seconds","aggregation_key":"cohort"},
            {"name":"leg_completion_status","unit":"category","aggregation_key":"person_id,leg_index"},
            {"name":"partial_link_traversals","unit":"records","aggregation_key":"metric"},
            {"name":"unmatched_leave_events","unit":"records","aggregation_key":"metric"}
        ]"#;
        for (dir, group_used, alice, bob) in [
            (&baseline, 1, 100, "stuck"),
            (&alternative, 2, 120, "complete"),
        ] {
            let analysis = dir.join("analysis");
            fs::write(analysis.join("metric_catalog.json"), catalog).unwrap();
            fs::write(analysis.join("group_coverage.csv"), format!("dimension,category,hour_start_seconds,eligible_links,used_links,unused_links,used_percent\nroad_type,local,0,3,{group_used},1,66.666667\n")).unwrap();
            fs::write(analysis.join("person_daily.csv"), format!("person_id,expected_legs,departed_legs,completed_legs,completed_duration_sum_seconds,completed_duration_mean_seconds,completion_status\nalice,1,1,1,{alice},{alice},complete\nbob,1,1,0,50,,{bob}\n")).unwrap();
            let duration = if alice == 100 { 100 } else { 120 };
            let bob_status = if bob == "stuck" { "stuck" } else { "completed" };
            let hour = if alice == 100 { 0 } else { 3600 };
            fs::write(analysis.join("legs.csv"), format!("person_id,leg_index,mode,departure_seconds,departure_hour_seconds,arrival_seconds,duration_seconds,status\nalice,0,car,{hour},{hour},{},{duration},completed\nbob,0,car,0,0,,,{bob_status}\n", hour + duration)).unwrap();
            let diagnostics = if alice == 100 {
                "metric,count\npartial_link_traversals,4\nunmatched_leave_events,2\n"
            } else {
                "metric,count\npartial_link_traversals,1\nunmatched_leave_events,9\n"
            };
            fs::write(analysis.join("link_speed_diagnostics.csv"), diagnostics).unwrap();
        }
        let report = compare_completed_runs(&baseline, &[alternative]).unwrap();
        let differences =
            fs::read_to_string(report.parent().unwrap().join("metric_differences.csv")).unwrap();
        assert!(differences.contains("group_used_links"));
        assert!(differences.contains("person_completed_leg_duration_sum"));
        assert!(differences.contains("leg_departures"));
        assert!(differences.contains("departing_persons"));
        assert!(differences.contains("leg_duration_mean"));
        assert!(
            differences.contains("1.000000,0.000000,-1.000000,-100.000000,1.000000,,,comparable")
        );
        assert!(differences.contains("0.000000,1.000000,1.000000,,0.000000,,,zero_baseline"));
        assert!(differences.contains("daily_mean_completed_travel_burden"));
        assert!(differences.contains("partial_link_traversals"));
        assert!(
            differences.contains("4.000000,1.000000,-3.000000,-75.000000,4.000000,,,comparable")
        );
        assert!(
            differences.contains("1.000000,2.000000,1.000000,100.000000,1.000000,,,comparable")
        );
        assert!(differences.contains(
            "100.000000,120.000000,20.000000,20.000000,100.000000,1.000000,1.000000,comparable"
        ));
        let statuses = fs::read_to_string(
            report
                .parent()
                .unwrap()
                .join("completion_status_differences.csv"),
        )
        .unwrap();
        assert!(statuses.contains(",stuck,1,0,-1"));
        assert!(statuses.contains(",complete,1,2,1"));
        let transitions = fs::read_to_string(
            report
                .parent()
                .unwrap()
                .join("completion_status_transitions.csv"),
        )
        .unwrap();
        assert!(transitions.contains(",stuck,complete,1"));
        let leg_transitions = fs::read_to_string(
            report
                .parent()
                .unwrap()
                .join("leg_completion_status_transitions.csv"),
        )
        .unwrap();
        assert!(leg_transitions.contains(",\"bob\",0,stuck,completed"));
        assert!(
            !differences.contains("bob"),
            "stuck or incomplete people must not enter duration comparisons"
        );
    }

    #[test]
    fn compares_service_metrics_from_published_reports() {
        let temp = tempfile::tempdir().unwrap();
        let baseline = run(temp.path(), "baseline", 1.0, "l1,0,10,0\n");
        let alternative = run(temp.path(), "alternative", 1.0, "l1,0,10,0\n");
        let catalog = serde_json::to_string(&super::super::metrics(false)).unwrap();
        for (dir, served, wait, empty, loaded) in [
            (&baseline, 8, 100, 250, 700),
            (&alternative, 10, 80, 300, 900),
        ] {
            let analysis = dir.join("analysis");
            fs::write(analysis.join("metric_catalog.json"), &catalog).unwrap();
            fs::write(
                analysis.join("service_summary.csv"),
                format!("scope,group,served,wait_mean_seconds\ntotal,,{served},{wait}\n"),
            )
            .unwrap();
            fs::write(
                analysis.join("service_vehicles.csv"),
                format!("scope,vehicle_id,empty_meters\nfleet,,{empty}\n"),
            )
            .unwrap();
            fs::write(
                analysis.join("service_occupancy.csv"),
                format!("load_passengers,load_vehicle_meters\n1,{loaded}\n"),
            )
            .unwrap();
        }

        let report = compare_completed_runs(&baseline, &[alternative]).unwrap();
        let differences =
            fs::read_to_string(report.parent().unwrap().join("metric_differences.csv")).unwrap();
        assert!(differences.contains("service_summary.csv,served"));
        assert!(differences.contains("8.000000,10.000000,2.000000,25.000000"));
        assert!(differences.contains("service_summary.csv,wait_mean_seconds"));
        assert!(differences.contains("service_vehicles.csv,empty_meters"));
        assert!(differences.contains("service_occupancy.csv,load_vehicle_meters"));
    }

    #[test]
    fn consumes_only_each_supplied_runs_published_iteration() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let report = root.join("comparison/analysis");
        fs::create_dir_all(&report).unwrap();
        fs::write(
            report.join("manifest.json"),
            r#"{"status":"complete","iteration":7,"sample_size":0.5,"interval_seconds":3600}"#,
        )
        .unwrap();
        fs::write(
            report.join("link_hourly.csv"),
            "link_id,hour_start_seconds,entry_vehicles\nlink-a,0,4\n",
        )
        .unwrap();
        fs::write(
            report.join("link_speed_hourly.csv"),
            "link_id,hour_start_seconds,representative_speed_mps\nlink-a,0,10\n",
        )
        .unwrap();
        fs::write(
            report.join("link_hourly_by_class.csv"),
            "vehicle_class,link_id,hour_start_seconds,entry_vehicles\ncar,link-a,0,3\nall,link-a,0,99\n",
        )
        .unwrap();
        fs::write(
            report.join("link_speed_by_class.csv"),
            "vehicle_class,link_id,hour_start_seconds,representative_speed_mps\ncar,link-a,0,12\nall,link-a,0,99\n",
        )
        .unwrap();

        let destination = root.join("destination");
        fs::create_dir(&destination).unwrap();
        write(root, &destination, &[PathBuf::from("comparison")]).unwrap();
        let rows = fs::read_to_string(destination.join("cross_run_comparison.csv")).unwrap();
        assert!(rows.contains("comparison,7,entry_vehicles,link-a,0,3600,all,4,0.5,8,vehicles"));
        assert!(
            rows.contains("comparison,7,representative_speed_mps,link-a,0,3600,all,10,0.5,10,m/s")
        );
        assert!(rows.contains("comparison,7,entry_vehicles,link-a,0,3600,car,3,0.5,6,vehicles"));
        assert!(
            rows.contains("comparison,7,representative_speed_mps,link-a,0,3600,car,12,0.5,12,m/s")
        );
        assert_eq!(rows.lines().count(), 5);

        fs::write(
            report.join("link_hourly.csv"),
            "link_id,hour_start_seconds,entry_vehicles\nlink-a,0,invalid\n",
        )
        .unwrap();
        assert!(write(root, &destination, &[PathBuf::from("comparison")]).is_err());
    }
}
