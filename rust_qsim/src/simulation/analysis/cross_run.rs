//! Comparisons between reports from completed runs.

use super::{AnalysisError, Manifest, read_json};
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
    writeln!(file, "alternative,table,metric,unit,aggregation_key,key,baseline_value,alternative_value,absolute_difference,relative_difference_percent,relative_baseline_denominator,status").map_err(io_error)?;
    let mut summary = Vec::new();
    for alternative in alternatives {
        validate_compatible(baseline, alternative)?;
        let matched_links = matching_link_ids(baseline, alternative)?;
        let same_network = matched_links.len() == baseline.manifest.eligible_links
            && matched_links.len() == alternative.manifest.eligible_links;
        let baseline_statuses = person_statuses(baseline)?;
        let alternative_statuses = person_statuses(alternative)?;
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
            if !same_network && matches!(spec.file, "coverage.csv" | "group_coverage.csv") {
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
                let (left, right) = (
                    read_rows(baseline, spec.file, &key_columns, metric_name)?,
                    read_rows(alternative, spec.file, &key_columns, metric_name)?,
                );
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
                    let before = left.get(&key).copied();
                    let after = right.get(&key).copied();
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
                    writeln!(
                        file,
                        "{},{},{},{},{},{},{},{},{},{},{},{}",
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
    html.push_str("</ul><h2>Metric differences</h2><table><thead><tr><th>Alternative</th><th>Table</th><th>Metric</th><th>Unit</th><th>Aggregation key</th><th>Key</th><th>Baseline</th><th>Alternative</th><th>Difference</th><th>Relative (%)</th><th>Relative baseline denominator</th><th>Status</th></tr></thead><tbody>");
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
        "metric_compatibility.csv",
    ] {
        html.push_str(&format!(
            "<h2>{}</h2><table>",
            match file {
                "completion_status_transitions.csv" => "Completion status transitions",
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
    for alternative in alternatives {
        let base = person_statuses(baseline)?;
        let other = person_statuses(alternative)?;
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
    }
    counts.flush().map_err(io_error)?;
    transitions.flush().map_err(io_error)?;
    Ok(())
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
            let registered = table_specs().iter().any(|spec| {
                spec.metrics
                    .iter()
                    .any(|(catalog_name, _)| *catalog_name == name)
            }) || diagnostic_metric;
            let output_available = table_specs().iter().any(|spec| {
                spec.metrics
                    .iter()
                    .any(|(catalog_name, _)| *catalog_name == name)
                    && baseline.path.join("analysis").join(spec.file).is_file()
                    && alternative.path.join("analysis").join(spec.file).is_file()
            }) || diagnostic_metric
                && baseline
                    .path
                    .join("analysis/link_speed_diagnostics.csv")
                    .is_file()
                && alternative
                    .path
                    .join("analysis/link_speed_diagnostics.csv")
                    .is_file();
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
                ("unused_links", "unused_links"),
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
    ]
}

fn csv(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}
fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
fn number(value: Option<f64>) -> String {
    value.map_or_else(String::new, |number| format!("{number:.6}"))
}
fn io_error(error: impl std::fmt::Display) -> AnalysisError {
    AnalysisError::new(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(root: &Path, name: &str, sample: f64, links: &str) -> PathBuf {
        let output = root.join(name);
        let analysis = output.join("analysis");
        fs::create_dir_all(&analysis).unwrap();
        fs::write(analysis.join("manifest.json"), format!(r#"{{"status":"complete","failure":null,"iteration":3,"interval_seconds":3600,"simulation_end_time":3600,"partitions":[0],"input_format":"xml","eligible_links":2,"random_seed":1,"sample_size":{sample},"network_input":null,"population_input":null,"software_version":"test"}}"#)).unwrap();
        fs::write(analysis.join("metric_catalog.json"), r#"[{"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"},{"name":"entry_pce_scaled","unit":"pce","aggregation_key":"link_id,interval_start_seconds"},{"name":"entry_vc","unit":"ratio","aggregation_key":"link_id,interval_start_seconds"}]"#).unwrap();
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
        fs::write(
            analysis.join("person_daily.csv"),
            "person_id,completion_status\n",
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
        assert!(csv.contains("10.000000,15.000000,5.000000,50.000000,10.000000,comparable"));
        assert!(csv.contains("0.000000,3.000000,3.000000,,0.000000,zero_baseline"));
        assert!(
            !csv.contains("99.000000"),
            "non-corresponding network links must be excluded"
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
            {"name":"person_completed_leg_duration_sum","unit":"seconds","aggregation_key":"person_id"}
        ]"#;
        for (dir, group_used, alice, bob) in [
            (&baseline, 1, 100, "stuck"),
            (&alternative, 2, 120, "complete"),
        ] {
            let analysis = dir.join("analysis");
            fs::write(analysis.join("metric_catalog.json"), catalog).unwrap();
            fs::write(analysis.join("group_coverage.csv"), format!("dimension,category,hour_start_seconds,eligible_links,used_links,unused_links,used_percent\nroad_type,local,0,3,{group_used},1,66.666667\n")).unwrap();
            fs::write(analysis.join("person_daily.csv"), format!("person_id,expected_legs,departed_legs,completed_legs,completed_duration_sum_seconds,completed_duration_mean_seconds,completion_status\nalice,1,1,1,{alice},{alice},complete\nbob,1,1,0,50,,{bob}\n")).unwrap();
        }
        let report = compare_completed_runs(&baseline, &[alternative]).unwrap();
        let differences =
            fs::read_to_string(report.parent().unwrap().join("metric_differences.csv")).unwrap();
        assert!(differences.contains("group_used_links"));
        assert!(differences.contains("person_completed_leg_duration_sum"));
        assert!(differences.contains("1.000000,2.000000,1.000000,100.000000,1.000000,comparable"));
        assert!(
            differences.contains("100.000000,120.000000,20.000000,20.000000,100.000000,comparable")
        );
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
        assert!(
            !differences.contains("bob"),
            "stuck or incomplete people must not enter duration comparisons"
        );
    }
}
