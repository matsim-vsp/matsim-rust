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
    let baseline = read_run(baseline_dir)?;
    let runs = alternative_dirs
        .iter()
        .map(|path| read_run(path))
        .collect::<Result<Vec<_>, _>>()?;
    let staging = baseline.path.join("analysis/.comparison-staging");
    let published = baseline.path.join("analysis/comparison");
    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(io_error)?;
    }
    fs::create_dir_all(&staging).map_err(io_error)?;
    let result = write_comparison(&staging, &baseline, &runs);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }
    let backup = baseline.path.join("analysis/.comparison-backup");
    if backup.exists() {
        fs::remove_dir_all(&backup).map_err(io_error)?;
    }
    if published.exists() {
        fs::rename(&published, &backup).map_err(io_error)?;
    }
    if let Err(error) = fs::rename(&staging, &published) {
        if backup.exists() {
            let _ = fs::rename(&backup, &published);
        }
        return Err(io_error(error));
    }
    if backup.exists() {
        fs::remove_dir_all(backup).map_err(io_error)?;
    }
    Ok(published.join("index.html"))
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
    writeln!(file, "alternative,table,metric,aggregation_key,key,baseline_value,alternative_value,absolute_difference,relative_difference_percent,denominator,status").map_err(io_error)?;
    let mut summary = Vec::new();
    for alternative in alternatives {
        validate_compatible(baseline, alternative)?;
        let matched_links = matching_link_ids(baseline, alternative)?;
        let same_network = matched_links.len() == baseline.manifest.eligible_links
            && matched_links.len() == alternative.manifest.eligible_links;
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
            for (catalog_name, metric_name) in spec.metrics {
                let Some(metric) = compatible.get(catalog_name).copied() else {
                    continue;
                };
                let key_columns = metric
                    .aggregation_key
                    .split(',')
                    .map(|key| match key {
                        "interval_start_seconds" => "hour_start_seconds",
                        _ => key,
                    })
                    .collect::<Vec<_>>();
                if key_columns.iter().any(|key| !headers.contains(*key))
                    || !headers.contains(*metric_name)
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
                    if spec.file == "link_hourly.csv"
                        && !matched_links.contains(key.split('|').next().unwrap_or_default())
                    {
                        continue;
                    }
                    let before = left.get(&key).copied();
                    let after = right.get(&key).copied();
                    // Completion filters exclude no-travel, stuck and incomplete people from
                    // person-level duration metrics; those outcomes remain visible in status counts.
                    let (Some(before), Some(after)) = (before, after) else {
                        continue;
                    };
                    let difference = after - before;
                    let relative = (before != 0.0).then(|| difference / before * 100.0);
                    writeln!(
                        file,
                        "{},{},{},{},{},{:.6},{:.6},{:.6},{},{:.6},{}",
                        csv(&alternative.path.display().to_string()),
                        spec.file,
                        catalog_name,
                        csv(&metric.aggregation_key),
                        csv(&key),
                        before,
                        after,
                        difference,
                        relative.map(|v| format!("{v:.6}")).unwrap_or_default(),
                        before,
                        if before == 0.0 {
                            "zero_baseline"
                        } else {
                            "comparable"
                        }
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
    html.push_str("</ul><h2>Metric differences</h2><table><thead><tr><th>Alternative</th><th>Table</th><th>Metric</th><th>Aggregation key</th><th>Key</th><th>Baseline</th><th>Alternative</th><th>Difference</th><th>Relative (%)</th><th>Denominator</th><th>Status</th></tr></thead><tbody>");
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
    fs::write(path.join("index.html"), html).map_err(io_error)?;
    Ok(())
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
            .collect::<Vec<_>>()
            .join("|");
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
            file: "leg_hourly.csv",
            metrics: &[
                ("leg_departures", "departures"),
                ("departing_persons", "departing_persons"),
                ("leg_duration_mean", "mean_duration_seconds"),
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
            file: "link_capacity.csv",
            metrics: &[
                ("entry_pce_scaled", "entry_pce_scaled"),
                ("exit_pce_scaled", "exit_pce_scaled"),
                ("entry_vc", "entry_vc"),
                ("exit_vc", "exit_vc"),
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
        fs::write(analysis.join("metric_catalog.json"), r#"[{"name":"entry_vehicles","unit":"vehicles","aggregation_key":"link_id,interval_start_seconds"}]"#).unwrap();
        fs::write(
            analysis.join("link_hourly.csv"),
            format!("link_id,hour_start_seconds,entry_vehicles,exit_vehicles\n{links}"),
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
        assert!(
            differences.contains("100.000000,120.000000,20.000000,20.000000,100.000000,comparable")
        );
        assert!(
            !differences.contains("bob"),
            "stuck or incomplete people must not enter duration comparisons"
        );
    }
}
