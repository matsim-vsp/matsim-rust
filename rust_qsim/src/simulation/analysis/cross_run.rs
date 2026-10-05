//! Combines metrics from each supplied run's published latest-iteration report.

use super::{AnalysisError, io_error};
use csv::{Reader, Writer};
use serde_json::Value;
use std::fs::{self, File};
use std::path::{Path, PathBuf};

pub(super) fn write(
    output_dir: &Path,
    report_dir: &Path,
    supplied_runs: &[PathBuf],
) -> Result<(), AnalysisError> {
    let mut writer = Writer::from_path(report_dir.join("cross_run_comparison.csv"))
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
        let manifest: Value = serde_json::from_reader(
            File::open(&manifest_path).map_err(io_error)?,
        )
        .map_err(|error| {
            AnalysisError::new(format!(
                "invalid run manifest {}: {error}",
                manifest_path.display()
            ))
        })?;
        if manifest.get("status").and_then(Value::as_str) != Some("complete") {
            return Err(AnalysisError::new(format!(
                "comparison run has no completed latest-iteration report: {}",
                latest_report.display()
            )));
        }
        let iteration = manifest
            .get("iteration")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "manifest has no iteration: {}",
                    manifest_path.display()
                ))
            })?;
        let sample_size = manifest
            .get("sample_size")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value > 0.0)
            .ok_or_else(|| {
                AnalysisError::new(format!(
                    "manifest has invalid sample_size: {}",
                    manifest_path.display()
                ))
            })?;
        let interval_seconds = manifest
            .get("interval_seconds")
            .and_then(Value::as_u64)
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
    writer: &mut Writer<File>,
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
    let mut reader = Reader::from_path(&path).map_err(|error| {
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
