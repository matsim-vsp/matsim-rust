//! Comparison of final-iteration link totals with a supplied observation CSV.

use super::{AnalysisError, csv, io_error, table_writer};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

#[derive(Debug, Deserialize)]
struct Observation {
    link_id: String,
    period_start_seconds: u64,
    period_end_seconds: u64,
    vehicle_class: String,
    metric: String,
    unit: String,
    value: f64,
    split: String,
}

#[derive(Debug, Clone)]
struct Match {
    link_id: String,
    period_start_seconds: u64,
    vehicle_class: String,
    metric: String,
    split: String,
    observed: f64,
    simulated: f64,
    expanded_simulated: f64,
    expansion_factor: f64,
    provenance: String,
    source_row: usize,
}

struct UnmatchedObservation {
    source_row: usize,
    split: String,
    metric: String,
    reason: String,
}

/// Match exact link/period/class/metric keys and report aggregate residual statistics.
/// `vehicle_class` names a run vehicle type, or `all` for the aggregate link output.
pub(super) fn write(
    report_dir: &Path,
    source: &Path,
    interval_seconds: u32,
    sample_size: f64,
    vehicle_classes: &[String],
) -> Result<(), AnalysisError> {
    let count_rows = read_simulated(
        &report_dir.join("link_hourly.csv"),
        "entry_vehicles",
        "count",
        1.0 / sample_size,
        None,
    )?;
    let class_count_rows = read_simulated(
        &report_dir.join("link_hourly_by_class.csv"),
        "entry_vehicles",
        "count",
        1.0 / sample_size,
        Some("vehicle_class"),
    )?;
    let speed_rows = read_simulated(
        &report_dir.join("link_speed_hourly.csv"),
        "representative_speed_mps",
        "speed",
        1.0,
        None,
    )?;
    let class_speed_rows = read_simulated(
        &report_dir.join("link_speed_by_class.csv"),
        "representative_speed_mps",
        "speed",
        1.0,
        Some("vehicle_class"),
    )?;
    let mut simulated = count_rows;
    simulated.extend(speed_rows);
    simulated.extend(
        class_count_rows
            .into_iter()
            .filter(|((class, _, _, _), _)| class != "all"),
    );
    simulated.extend(
        class_speed_rows
            .into_iter()
            .filter(|((class, _, _, _), _)| class != "all"),
    );
    let vehicle_classes: std::collections::BTreeSet<_> =
        vehicle_classes.iter().map(String::as_str).collect();

    let file = File::open(source).map_err(io_error)?;
    let mut reader = csv::Reader::from_reader(file);
    let mut matched = Vec::new();
    let mut unmatched = Vec::new();
    for (row_index, record) in reader.deserialize::<Observation>().enumerate() {
        let observation = record.map_err(|error| {
            AnalysisError::new(format!(
                "invalid observed-data CSV row {}: {error}",
                row_index + 2
            ))
        })?;
        let reason = if !observation.value.is_finite()
            || (observation.metric == "count" && observation.value < 0.0)
            || (observation.metric == "speed" && observation.value <= 0.0)
        {
            Some("invalid_observation_value")
        } else if observation.split != "calibration" && observation.split != "holdout" {
            Some("invalid_split")
        } else if observation.vehicle_class != "all"
            && !vehicle_classes.contains(observation.vehicle_class.as_str())
        {
            Some("vehicle_class_unavailable")
        } else if observation
            .period_start_seconds
            .checked_add(u64::from(interval_seconds))
            != Some(observation.period_end_seconds)
        {
            Some("period_mismatch")
        } else {
            None
        };
        if let Some(reason) = reason {
            unmatched.push(UnmatchedObservation {
                source_row: row_index + 2,
                split: observation.split,
                metric: observation.metric,
                reason: reason.to_owned(),
            });
            continue;
        }
        let Some((simulated_value, expansion_factor)) = simulated.get(&(
            observation.vehicle_class.clone(),
            observation.link_id.clone(),
            observation.period_start_seconds,
            observation.metric.clone(),
        )) else {
            unmatched.push(UnmatchedObservation {
                source_row: row_index + 2,
                split: observation.split,
                metric: observation.metric,
                reason: "no_simulation_match".to_owned(),
            });
            continue;
        };
        let Some(observed_value) =
            convert(&observation.metric, &observation.unit, observation.value)
        else {
            unmatched.push(UnmatchedObservation {
                source_row: row_index + 2,
                split: observation.split,
                metric: observation.metric,
                reason: "unsupported_unit".to_owned(),
            });
            continue;
        };
        let expanded_simulated = simulated_value * expansion_factor;
        if !expanded_simulated.is_finite() {
            unmatched.push(UnmatchedObservation {
                source_row: row_index + 2,
                split: observation.split,
                metric: observation.metric,
                reason: "non_finite_simulated_value".to_owned(),
            });
            continue;
        }
        matched.push(Match {
            link_id: observation.link_id,
            period_start_seconds: observation.period_start_seconds,
            vehicle_class: observation.vehicle_class,
            metric: observation.metric,
            split: observation.split,
            observed: observed_value,
            simulated: *simulated_value,
            expanded_simulated,
            expansion_factor: *expansion_factor,
            provenance: source.display().to_string(),
            source_row: row_index + 2,
        });
    }

    write_matches(report_dir, &matched)?;
    write_unmatched(report_dir, &unmatched)?;
    write_summary(report_dir, &matched, &unmatched, interval_seconds)?;
    write_plot(report_dir, &matched)?;
    write_residual_map(report_dir, &matched)?;
    write_time_profile(report_dir, &matched)?;
    Ok(())
}

fn read_simulated(
    path: &Path,
    value_column: &str,
    metric: &str,
    expansion_factor: f64,
    class_column: Option<&str>,
) -> Result<BTreeMap<(String, String, u64, String), (f64, f64)>, AnalysisError> {
    let mut reader =
        csv::Reader::from_path(path).map_err(|error| AnalysisError::new(error.to_string()))?;
    let headers = reader
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .clone();
    let link = headers
        .iter()
        .position(|name| name == "link_id")
        .ok_or_else(|| {
            AnalysisError::new(format!("missing link_id column in {}", path.display()))
        })?;
    let period = headers
        .iter()
        .position(|name| name == "hour_start_seconds")
        .ok_or_else(|| {
            AnalysisError::new(format!(
                "missing hour_start_seconds column in {}",
                path.display()
            ))
        })?;
    let value = headers
        .iter()
        .position(|name| name == value_column)
        .ok_or_else(|| {
            AnalysisError::new(format!(
                "missing {value_column} column in {}",
                path.display()
            ))
        })?;
    let class =
        match class_column {
            Some(column) => Some(headers.iter().position(|name| name == column).ok_or_else(
                || AnalysisError::new(format!("missing {column} column in {}", path.display())),
            )?),
            None => None,
        };
    let mut values = BTreeMap::new();
    for row in reader.records() {
        let row = row.map_err(|error| AnalysisError::new(error.to_string()))?;
        let Ok(period_start) = row[period].parse::<u64>() else {
            continue;
        };
        let Ok(value) = row[value].parse::<f64>() else {
            continue;
        };
        if value.is_finite() {
            values.insert(
                (
                    class.map_or_else(|| "all".to_owned(), |index| row[index].to_owned()),
                    row[link].to_owned(),
                    period_start,
                    metric.to_owned(),
                ),
                (value, expansion_factor),
            );
        }
    }
    Ok(values)
}

fn convert(metric: &str, unit: &str, value: f64) -> Option<f64> {
    match (metric, unit) {
        ("count", "vehicles" | "vehicle" | "veh") => Some(value),
        ("speed", "m/s" | "mps") => Some(value),
        ("speed", "km/h" | "kph") => Some(value / 3.6),
        _ => None,
    }
}

fn write_matches(path: &Path, rows: &[Match]) -> Result<(), AnalysisError> {
    let mut writer = table_writer(path, "validation_matches.csv")?;
    writeln!(writer, "link_id,period_start_seconds,vehicle_class,metric,split,observed,simulated_sample,expansion_factor,simulated_expanded,residual,relative_error,observation_source,source_row")
        .map_err(io_error)?;
    for row in rows {
        let relative = if row.observed == 0.0 {
            String::new()
        } else {
            format_number((row.expanded_simulated - row.observed) / row.observed)
        };
        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{},{},{},{},{}",
            csv(&row.link_id),
            row.period_start_seconds,
            csv(&row.vehicle_class),
            csv(&row.metric),
            csv(&row.split),
            format_number(row.observed),
            format_number(row.simulated),
            format_number(row.expansion_factor),
            format_number(row.expanded_simulated),
            format_number(row.expanded_simulated - row.observed),
            relative,
            csv(&row.provenance),
            row.source_row
        )
        .map_err(io_error)?;
    }
    Ok(())
}

fn write_unmatched(path: &Path, rows: &[UnmatchedObservation]) -> Result<(), AnalysisError> {
    let mut writer = table_writer(path, "validation_unmatched.csv")?;
    writeln!(writer, "source_row,split,metric,reason").map_err(io_error)?;
    for row in rows {
        writeln!(
            writer,
            "{},{},{},{}",
            row.source_row,
            csv(&row.split),
            csv(&row.metric),
            csv(&row.reason)
        )
        .map_err(io_error)?;
    }
    Ok(())
}

fn write_summary(
    path: &Path,
    matches: &[Match],
    unmatched: &[UnmatchedObservation],
    interval_seconds: u32,
) -> Result<(), AnalysisError> {
    let mut grouped: BTreeMap<(&str, &str), Vec<&Match>> = BTreeMap::new();
    for row in matches {
        grouped
            .entry((&row.split, &row.metric))
            .or_default()
            .push(row);
    }
    for row in unmatched {
        grouped.entry((&row.split, &row.metric)).or_default();
    }
    let mut writer = table_writer(path, "validation_summary.csv")?;
    writeln!(writer, "split,metric,sample_size,bias,mae,rmse,geh_mean,geh_count,unmatched_observations,undefined_relative_errors")
        .map_err(io_error)?;
    for ((split, metric), rows) in grouped {
        let mut errors = Vec::new();
        let mut geh = Vec::new();
        for row in &rows {
            let residual = row.expanded_simulated - row.observed;
            errors.push(residual);
            if metric == "count" {
                let hourly_factor = 3600.0 / f64::from(interval_seconds);
                let simulated_hourly = row.expanded_simulated * hourly_factor;
                let observed_hourly = row.observed * hourly_factor;
                let denominator = simulated_hourly + observed_hourly;
                if denominator > 0.0 {
                    geh.push(
                        std::f64::consts::SQRT_2 * (simulated_hourly - observed_hourly).abs()
                            / denominator.sqrt(),
                    );
                }
            }
        }
        let n = errors.len() as f64;
        let bias = if n == 0.0 {
            String::new()
        } else {
            format_number(errors.iter().map(|error| error / n).sum::<f64>())
        };
        let mae = if n == 0.0 {
            String::new()
        } else {
            format_number(errors.iter().map(|error| error.abs() / n).sum::<f64>())
        };
        let rmse = if n == 0.0 {
            String::new()
        } else {
            format_number(errors.iter().fold(0.0_f64, |sum, error| sum.hypot(*error)) / n.sqrt())
        };
        let geh_mean = if geh.is_empty() {
            String::new()
        } else {
            format_number(
                geh.iter()
                    .map(|value| value / geh.len() as f64)
                    .sum::<f64>(),
            )
        };
        let undefined_relative = rows.iter().filter(|row| row.observed == 0.0).count();
        let unmatched_count = unmatched
            .iter()
            .filter(|row| row.split == split && row.metric == metric)
            .count();
        writeln!(
            writer,
            "{split},{metric},{},{bias},{mae},{rmse},{geh_mean},{},{unmatched_count},{undefined_relative}",
            rows.len(),
            geh.len()
        )
        .map_err(io_error)?;
    }
    Ok(())
}

fn write_plot(path: &Path, matches: &[Match]) -> Result<(), AnalysisError> {
    for metric in ["count", "speed"] {
        for split in ["calibration", "holdout"] {
            let rows: Vec<_> = matches
                .iter()
                .filter(|row| {
                    row.metric == metric && row.split == split && row.vehicle_class == "all"
                })
                .collect();
            let max = rows
                .iter()
                .map(|row| row.observed.max(row.expanded_simulated))
                .fold(1.0_f64, f64::max);
            let points = rows
                .iter()
                .map(|row| {
                    let x = 40.0 + (row.observed / max) * 580.0;
                    let y = 560.0 - (row.expanded_simulated / max) * 520.0;
                    format!("<circle cx=\"{x:.2}\" cy=\"{y:.2}\" r=\"3\"/>")
                })
                .collect::<String>();
            let svg = format!(
                "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"600\" viewBox=\"0 0 700 600\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/><path d=\"M40 560H640 M40 560V40 M40 560L640 40\" stroke=\"#333\"/><g fill=\"#1769aa\">{points}</g><text x=\"300\" y=\"590\">Observed {metric}</text><text transform=\"translate(15 320) rotate(-90)\">Simulated {metric}</text></svg>"
            );
            std::fs::write(
                path.join(format!("validation_scatter_{metric}_{split}.svg")),
                svg,
            )
            .map_err(io_error)?;
        }
    }
    std::fs::copy(
        path.join("validation_scatter_count_calibration.svg"),
        path.join("validation_scatter.svg"),
    )
    .map_err(io_error)?;
    Ok(())
}

fn write_residual_map(path: &Path, matches: &[Match]) -> Result<(), AnalysisError> {
    let network = std::fs::read_to_string(path.join("network_map.svg")).map_err(io_error)?;
    for split in ["calibration", "holdout"] {
        let mut residual_totals: BTreeMap<&str, (f64, u64)> = BTreeMap::new();
        for row in matches
            .iter()
            .filter(|row| row.metric == "count" && row.vehicle_class == "all" && row.split == split)
        {
            let total = residual_totals.entry(&row.link_id).or_default();
            total.1 += 1;
            total.0 += ((row.expanded_simulated - row.observed) - total.0) / total.1 as f64;
        }
        let residuals: BTreeMap<_, _> = residual_totals
            .into_iter()
            .map(|(link, (residual, _))| (link, residual))
            .collect();
        let mut svg = String::with_capacity(network.len());
        for line in network.lines() {
            if line.starts_with("<line ")
                && let Some(attribute) = line.split("data-link-id=\"").nth(1)
                && let Some((link, _)) = attribute.split_once('"')
                && let Some(residual) = residuals.get(link)
            {
                let color = if *residual > 0.0 {
                    "#c62828"
                } else if *residual < 0.0 {
                    "#1565c0"
                } else {
                    "#777777"
                };
                svg.push_str(
                    &line
                        .replace("stroke=\"#287a3d\"", &format!("stroke=\"{color}\""))
                        .replace("stroke=\"#c8ccd0\"", &format!("stroke=\"{color}\"")),
                );
            } else {
                svg.push_str(line);
            }
            svg.push('\n');
        }
        std::fs::write(
            path.join(format!("validation_residual_map_{split}.svg")),
            &svg,
        )
        .map_err(io_error)?;
        if split == "calibration" {
            std::fs::write(path.join("validation_residual_map.svg"), svg).map_err(io_error)?;
        }
    }
    Ok(())
}

fn write_time_profile(path: &Path, matches: &[Match]) -> Result<(), AnalysisError> {
    for split in ["calibration", "holdout"] {
        let mut periods: BTreeMap<u64, (f64, f64, u64)> = BTreeMap::new();
        for row in matches
            .iter()
            .filter(|row| row.metric == "count" && row.vehicle_class == "all" && row.split == split)
        {
            let values = periods.entry(row.period_start_seconds).or_default();
            values.2 += 1;
            values.0 += (row.observed - values.0) / values.2 as f64;
            values.1 += (row.expanded_simulated - values.1) / values.2 as f64;
        }
        let first = periods.first_key_value().map_or(0, |(period, _)| *period);
        let last = periods
            .last_key_value()
            .map_or(first + 1, |(period, _)| *period);
        let max_value = periods
            .values()
            .map(|(observed, simulated, _)| observed.max(*simulated))
            .fold(1.0_f64, f64::max);
        let mut svg = String::from(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"700\" height=\"400\" viewBox=\"0 0 700 400\"><rect width=\"100%\" height=\"100%\" fill=\"white\"/><path d=\"M40 360H660 M40 360V30\" stroke=\"#333\"/>",
        );
        for (period, (observed, simulated, _)) in periods {
            let x = 50.0
                + (period.saturating_sub(first) as f64 * 580.0
                    / last.saturating_sub(first).max(1) as f64);
            let observed_y = 350.0 - observed * 320.0 / max_value;
            let simulated_y = 350.0 - simulated * 320.0 / max_value;
            svg.push_str(&format!("<circle cx=\"{x:.1}\" cy=\"{observed_y:.1}\" r=\"3\" fill=\"#1565c0\"><title>Mean observed count at {period} s</title></circle><circle cx=\"{x:.1}\" cy=\"{simulated_y:.1}\" r=\"3\" fill=\"#c62828\"><title>Mean simulated count at {period} s</title></circle>"));
        }
        svg.push_str("<text x=\"300\" y=\"390\">Period start (seconds)</text><text x=\"50\" y=\"20\" fill=\"#1565c0\">Observed mean</text><text x=\"160\" y=\"20\" fill=\"#c62828\">Simulated mean</text></svg>");
        std::fs::write(
            path.join(format!("validation_time_profiles_{split}.svg")),
            &svg,
        )
        .map_err(io_error)?;
        if split == "calibration" {
            std::fs::write(path.join("validation_time_profiles.svg"), svg).map_err(io_error)?;
        }
    }
    Ok(())
}

fn format_number(value: f64) -> String {
    if value.is_finite() {
        format!("{value:.6}")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_data_and_reports_class_period_missing_and_zero_reference_cases() {
        let directory = tempfile::tempdir().unwrap();
        let report = directory.path().join("analysis");
        std::fs::create_dir(&report).unwrap();
        std::fs::write(
            report.join("link_hourly.csv"),
            "link_id,hour_start_seconds,entry_vehicles\nlink-a,0,10\n",
        )
        .unwrap();
        std::fs::write(
            report.join("link_speed_hourly.csv"),
            "link_id,hour_start_seconds,representative_speed_mps\nlink-a,0,10\n",
        )
        .unwrap();
        std::fs::write(
            report.join("link_hourly_by_class.csv"),
            "vehicle_class,link_id,hour_start_seconds,entry_vehicles\ncar,link-a,0,4\nall,link-a,0,99\n",
        )
        .unwrap();
        std::fs::write(
            report.join("link_speed_by_class.csv"),
            "vehicle_class,link_id,hour_start_seconds,representative_speed_mps\ncar,link-a,0,20\nall,link-a,0,99\n",
        )
        .unwrap();
        std::fs::write(
            report.join("network_map.svg"),
            "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>",
        )
        .unwrap();
        let source = directory.path().join("observed.csv");
        std::fs::write(
            &source,
            "link_id,period_start_seconds,period_end_seconds,vehicle_class,metric,unit,value,split\n\
             link-a,0,3600,all,count,vehicles,8,calibration\n\
             link-a,0,3600,car,count,vehicles,4,calibration\n\
             link-a,0,3600,car,speed,km/h,72,holdout\n\
             link-a,0,3600,all,speed,km/h,36,holdout\n\
             link-a,0,3600,bus,count,vehicles,5,calibration\n\
             link-a,0,1800,all,count,vehicles,5,calibration\n\
             missing,0,3600,all,count,vehicles,5,calibration\n\
             link-a,0,3600,all,count,vehicles,0,holdout\n",
        )
        .unwrap();

        write(&report, &source, 3600, 0.5, &["car".to_owned()]).unwrap();
        let rows = std::fs::read_to_string(report.join("validation_matches.csv")).unwrap();
        assert!(rows.contains("\"link-a\",0,\"all\",\"count\",\"calibration\",8.000000,10.000000,2.000000,20.000000,12.000000,1.500000"));
        assert!(rows.contains(
            "\"link-a\",0,\"all\",\"speed\",\"holdout\",10.000000,10.000000,1.000000,10.000000,0.000000,0.000000"
        ));
        assert!(rows.contains(
            "\"link-a\",0,\"car\",\"count\",\"calibration\",4.000000,4.000000,2.000000,8.000000,4.000000,1.000000"
        ));
        assert!(rows.contains(
            "\"link-a\",0,\"car\",\"speed\",\"holdout\",20.000000,20.000000,1.000000,20.000000,0.000000,0.000000"
        ));
        assert!(rows.contains(
            "\"link-a\",0,\"all\",\"count\",\"holdout\",0.000000,10.000000,2.000000,20.000000,20.000000,,"
        ));
        let unmatched = std::fs::read_to_string(report.join("validation_unmatched.csv")).unwrap();
        assert!(unmatched.contains("vehicle_class_unavailable"));
        assert!(unmatched.contains("period_mismatch"));
        assert!(unmatched.contains("no_simulation_match"));
        let summary = std::fs::read_to_string(report.join("validation_summary.csv")).unwrap();
        assert!(summary.contains("calibration,count,2,8.000000,8.000000,8.944272,"));
        assert!(summary.contains("holdout,count,1,20.000000,20.000000,20.000000,6.324555,1,0,1"));
        assert!(summary.contains("holdout,speed,2,0.000000,0.000000,0.000000,,0,0,0"));
        let calibration_profile =
            std::fs::read_to_string(report.join("validation_time_profiles_calibration.svg"))
                .unwrap();
        let holdout_profile =
            std::fs::read_to_string(report.join("validation_time_profiles_holdout.svg")).unwrap();
        assert_ne!(calibration_profile, holdout_profile);
        assert!(
            report
                .join("validation_residual_map_calibration.svg")
                .is_file()
        );
        assert!(report.join("validation_residual_map_holdout.svg").is_file());
    }

    #[test]
    fn geh_scales_interval_counts_to_hourly_rates() {
        let directory = tempfile::tempdir().unwrap();
        let matches = [Match {
            link_id: "link-a".to_owned(),
            period_start_seconds: 0,
            vehicle_class: "all".to_owned(),
            metric: "count".to_owned(),
            split: "calibration".to_owned(),
            observed: 10.0,
            simulated: 20.0,
            expanded_simulated: 20.0,
            expansion_factor: 1.0,
            provenance: "observed.csv".to_owned(),
            source_row: 2,
        }];
        write_summary(directory.path(), &matches, &[], 1800).unwrap();
        let summary =
            std::fs::read_to_string(directory.path().join("validation_summary.csv")).unwrap();
        assert!(
            summary.contains("calibration,count,1,10.000000,10.000000,10.000000,3.651484,1,0,0")
        );
    }
}
