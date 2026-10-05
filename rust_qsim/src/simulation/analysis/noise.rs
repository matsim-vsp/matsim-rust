//! Supplied modeled receiver sound, exposure and affected-population summaries.

use crate::simulation::analysis::{AnalysisError, TableSpec, xml_escape};
use crate::simulation::config::NoiseInputs;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

#[derive(Debug, Deserialize)]
struct Record {
    receiver_id: String,
    period_start_seconds: u32,
    period_end_seconds: u32,
    metric: String,
    unit: String,
    value: f64,
    #[serde(default)]
    x: Option<f64>,
    #[serde(default)]
    y: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct Population {
    receiver_id: String,
    period_start_seconds: u32,
    period_end_seconds: u32,
    affected_population: f64,
}

#[derive(Default)]
struct Aggregate {
    values: Vec<f64>,
    unit: String,
    location: Option<(f64, f64)>,
}

type Key = (String, u32, u32, String);

/// Write grouped supplied metrics, joining population only on receiver and exact period.
pub(super) fn write(
    dir: &Path,
    output_dir: &Path,
    inputs: &NoiseInputs,
) -> Result<(), AnalysisError> {
    let records_path = resolve(output_dir, &inputs.records);
    let mut records = csv::Reader::from_path(records_path).map_err(noise_error)?;
    let mut groups: BTreeMap<Key, Aggregate> = BTreeMap::new();
    for row in records.deserialize::<Record>() {
        let row = row.map_err(noise_error)?;
        if row.receiver_id.trim().is_empty()
            || row.period_end_seconds <= row.period_start_seconds
            || !row.value.is_finite()
            || row.metric.trim().is_empty()
            || row.unit.trim().is_empty()
        {
            return Err(AnalysisError::new("invalid noise record"));
        }
        if matches!(row.metric.as_str(), "source_sound" | "exposure") && row.unit != "dB" {
            return Err(AnalysisError::new(format!(
                "noise metric {} must use dB",
                row.metric
            )));
        }
        if row.x.is_some() != row.y.is_some()
            || row.x.is_some_and(|coordinate| !coordinate.is_finite())
            || row.y.is_some_and(|coordinate| !coordinate.is_finite())
        {
            return Err(AnalysisError::new(
                "noise receiver coordinates must be paired and finite",
            ));
        }
        let group = groups
            .entry((
                row.receiver_id,
                row.period_start_seconds,
                row.period_end_seconds,
                row.metric,
            ))
            .or_insert_with(|| Aggregate {
                values: Vec::new(),
                unit: row.unit.clone(),
                location: row.x.zip(row.y),
            });
        if group.unit != row.unit {
            return Err(AnalysisError::new("noise metric has inconsistent units"));
        }
        if group.location != row.x.zip(row.y) {
            return Err(AnalysisError::new(
                "noise receiver has inconsistent coordinates",
            ));
        }
        group.values.push(row.value);
    }

    let mut populations: BTreeMap<(String, u32, u32), f64> = BTreeMap::new();
    if let Some(path) = &inputs.affected_population {
        let mut reader = csv::Reader::from_path(resolve(output_dir, path)).map_err(noise_error)?;
        for row in reader.deserialize::<Population>() {
            let row = row.map_err(noise_error)?;
            if row.receiver_id.trim().is_empty()
                || row.period_end_seconds <= row.period_start_seconds
                || !row.affected_population.is_finite()
                || row.affected_population < 0.0
            {
                return Err(AnalysisError::new("invalid affected-population record"));
            }
            let population = populations
                .entry((
                    row.receiver_id,
                    row.period_start_seconds,
                    row.period_end_seconds,
                ))
                .or_default();
            *population += row.affected_population;
            if !population.is_finite() {
                return Err(AnalysisError::new("affected population sum is not finite"));
            }
        }
    }

    let map_files = groups
        .iter()
        .filter(|((_, _, _, metric), group)| {
            group.location.is_some() && matches!(metric.as_str(), "source_sound" | "exposure")
        })
        .map(|((_, start, end, metric), _)| (metric.clone(), *start, *end))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .enumerate()
        .map(|(index, key)| (key, format!("noise_map_{index}.svg")))
        .collect::<BTreeMap<_, _>>();
    let has_receiver_locations = groups.values().any(|group| group.location.is_some());
    let mut map_groups: BTreeMap<(String, u32, u32), Vec<(String, f64, f64, f64)>> =
        BTreeMap::new();
    let mut writer = csv::Writer::from_path(dir.join("noise_summary.csv")).map_err(noise_error)?;
    writer
        .write_record([
            "receiver_id",
            "period_start_seconds",
            "period_end_seconds",
            "metric",
            "unit",
            "value",
            "averaging_convention",
            "affected_population",
            "map_file",
        ])
        .map_err(noise_error)?;
    for ((receiver, start, end, metric), group) in groups {
        let value = if matches!(metric.as_str(), "source_sound" | "exposure") {
            // Decibels are logarithmic: average sound energy, then convert back to dB.
            let max = group
                .values
                .iter()
                .copied()
                .fold(f64::NEG_INFINITY, f64::max);
            max + 10.0
                * (group
                    .values
                    .iter()
                    .map(|v| 10_f64.powf((v - max) / 10.0))
                    .sum::<f64>()
                    / group.values.len() as f64)
                    .log10()
        } else if metric == "damage" {
            group.values.iter().sum()
        } else {
            arithmetic_mean(&group.values)
        };
        if !value.is_finite() {
            return Err(AnalysisError::new("noise metric aggregate is not finite"));
        }
        let map_file = if matches!(metric.as_str(), "source_sound" | "exposure") {
            if let Some((x, y)) = group.location {
                map_groups
                    .entry((metric.clone(), start, end))
                    .or_default()
                    .push((receiver.clone(), x, y, value));
                map_files
                    .get(&(metric.clone(), start, end))
                    .expect("map key collected from sound records")
                    .clone()
            } else {
                String::new()
            }
        } else {
            String::new()
        };
        let population = populations
            .get(&(receiver.clone(), start, end))
            .map(ToString::to_string)
            .unwrap_or_default();
        writer
            .write_record([
                receiver,
                start.to_string(),
                end.to_string(),
                metric.clone(),
                group.unit,
                value.to_string(),
                if matches!(metric.as_str(), "source_sound" | "exposure") {
                    "energy_mean"
                } else if metric == "damage" {
                    "sum"
                } else {
                    "arithmetic_mean"
                }
                .to_owned(),
                population,
                map_file,
            ])
            .map_err(noise_error)?;
    }
    writer.flush().map_err(noise_error)?;
    let mut maps = csv::Writer::from_path(dir.join("noise_maps.csv")).map_err(noise_error)?;
    maps.write_record([
        "metric",
        "period_start_seconds",
        "period_end_seconds",
        "map_file",
    ])
    .map_err(noise_error)?;
    for ((metric, start, end), points) in map_groups {
        let file = map_files
            .get(&(metric.clone(), start, end))
            .expect("map group has an assigned filename")
            .clone();
        write_map(&dir.join(&file), &metric, start, end, &points)?;
        maps.write_record([metric, start.to_string(), end.to_string(), file])
            .map_err(noise_error)?;
    }
    maps.flush().map_err(noise_error)?;
    let mut availability =
        csv::Writer::from_path(dir.join("noise_availability.csv")).map_err(noise_error)?;
    availability
        .write_record(["metric", "status", "reason"])
        .map_err(noise_error)?;
    availability
        .write_record([
            "affected_population",
            if inputs.affected_population.is_some() {
                "available"
            } else {
                "unavailable"
            },
            if inputs.affected_population.is_some() {
                ""
            } else {
                "No affected-population input supplied"
            },
        ])
        .map_err(noise_error)?;
    availability
        .write_record([
            "receiver_locations",
            if has_receiver_locations {
                "available"
            } else {
                "unavailable"
            },
            if has_receiver_locations {
                ""
            } else {
                "No receiver coordinates supplied"
            },
        ])
        .map_err(noise_error)?;
    availability.flush().map_err(noise_error)?;
    Ok(())
}

/// Tables of this module that a cross-run comparison can line up, as `core::cross_run` reads
/// them.
pub(super) const COMPARISON_TABLES: &[TableSpec] = &[TableSpec {
    file: "noise_summary.csv",
    metrics: &[
        ("receiver_noise_value", "value"),
        ("affected_population", "affected_population"),
    ],
}];

pub(super) fn write_empty(dir: &Path) -> Result<(), AnalysisError> {
    std::fs::write(
        dir.join("noise_summary.csv"),
        "receiver_id,period_start_seconds,period_end_seconds,metric,unit,value,averaging_convention,affected_population,map_file\n",
    )
    .map_err(noise_error)?;
    std::fs::write(
        dir.join("noise_availability.csv"),
        "metric,status,reason\naffected_population,unavailable,No noise records configured\nreceiver_locations,unavailable,No noise records configured\n",
    )
    .map_err(noise_error)?;
    std::fs::write(
        dir.join("noise_maps.csv"),
        "metric,period_start_seconds,period_end_seconds,map_file\n",
    )
    .map_err(noise_error)
}

fn write_map(
    path: &Path,
    metric: &str,
    start: u32,
    end: u32,
    points: &[(String, f64, f64, f64)],
) -> Result<(), AnalysisError> {
    let min_x = points
        .iter()
        .map(|point| point.1)
        .fold(f64::INFINITY, f64::min);
    let max_x = points
        .iter()
        .map(|point| point.1)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = points
        .iter()
        .map(|point| point.2)
        .fold(f64::INFINITY, f64::min);
    let max_y = points
        .iter()
        .map(|point| point.2)
        .fold(f64::NEG_INFINITY, f64::max);
    let min_value = points
        .iter()
        .map(|point| point.3)
        .fold(f64::INFINITY, f64::min);
    let max_value = points
        .iter()
        .map(|point| point.3)
        .fold(f64::NEG_INFINITY, f64::max);
    let scale = (max_x - min_x).max(max_y - min_y).max(1.0);
    let mut svg = format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 800 600\"><rect width=\"800\" height=\"600\" fill=\"white\"/><text x=\"20\" y=\"24\">{}, {start}–{end} seconds; color scale {min_value:.2}–{max_value:.2} dB</text>",
        xml_escape(metric)
    );
    for (receiver, x, y, value) in points {
        let px = 40.0 + (x - min_x) / scale * 720.0;
        let py = 560.0 - (y - min_y) / scale * 520.0;
        let ratio = if max_value == min_value {
            0.5
        } else {
            (value - min_value) / (max_value - min_value)
        };
        let hue = 240.0 * (1.0 - ratio);
        svg.push_str(&format!(
            "<circle cx=\"{px:.2}\" cy=\"{py:.2}\" r=\"7\" fill=\"hsl({hue:.0},70%,45%)\"><title>{}: {value} dB</title></circle>",
            xml_escape(receiver)
        ));
    }
    svg.push_str("</svg>");
    std::fs::write(path, svg).map_err(noise_error)
}

fn noise_error(error: impl std::fmt::Display) -> AnalysisError {
    AnalysisError::new(error.to_string())
}

fn arithmetic_mean(values: &[f64]) -> f64 {
    let scale = values.iter().map(|value| value.abs()).fold(0.0, f64::max);
    if scale == 0.0 {
        return 0.0;
    }
    scale * (values.iter().map(|value| value / scale).sum::<f64>() / values.len() as f64)
}

fn resolve(output_dir: &Path, path: &Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        output_dir.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::config::NoiseInputs;
    use std::fs;

    #[test]
    fn energy_averages_sound_and_joins_only_supplied_population() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("records.csv"),
            "receiver_id,period_start_seconds,period_end_seconds,metric,unit,value\nr1,0,3600,source_sound,dB,50\nr1,0,3600,source_sound,dB,60\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("population.csv"),
            "receiver_id,period_start_seconds,period_end_seconds,affected_population\nr1,0,3600,12\n",
        )
        .unwrap();
        let inputs = NoiseInputs {
            records: "records.csv".into(),
            affected_population: Some("population.csv".into()),
        };
        write(dir.path(), dir.path(), &inputs).unwrap();
        let mut rows = csv::Reader::from_path(dir.path().join("noise_summary.csv")).unwrap();
        let row = rows.records().next().unwrap().unwrap();
        let value: f64 = row[5].parse().unwrap();
        assert!((value - 57.4036268949).abs() < 1e-8);
        assert_eq!(&row[6], "energy_mean");
        assert_eq!(&row[7], "12");
    }

    #[test]
    fn absent_population_is_left_blank_and_reported_unavailable() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("records.csv"),
            "receiver_id,period_start_seconds,period_end_seconds,metric,unit,value\nr1,0,3600,exposure,dB,42\n",
        )
        .unwrap();
        let inputs = NoiseInputs {
            records: "records.csv".into(),
            affected_population: None,
        };
        write(dir.path(), dir.path(), &inputs).unwrap();
        let mut summary = csv::Reader::from_path(dir.path().join("noise_summary.csv")).unwrap();
        let row = summary.records().next().unwrap().unwrap();
        assert_eq!(&row[6], "energy_mean");
        assert!(row[7].is_empty());
        let availability = fs::read_to_string(dir.path().join("noise_availability.csv")).unwrap();
        assert!(availability.contains("unavailable"));
        assert!(availability.contains("receiver coordinates"));
    }

    #[test]
    fn periods_stay_separate_and_population_requires_exact_receiver_and_period() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("records.csv"),
            "receiver_id,period_start_seconds,period_end_seconds,metric,unit,value\nr1,0,1800,exposure,dB,40\nr1,1800,3600,exposure,dB,50\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("population.csv"),
            "receiver_id,period_start_seconds,period_end_seconds,affected_population\nr1,0,3600,10\nother,1800,3600,20\n",
        )
        .unwrap();
        write(
            dir.path(),
            dir.path(),
            &NoiseInputs {
                records: "records.csv".into(),
                affected_population: Some("population.csv".into()),
            },
        )
        .unwrap();
        let mut rows = csv::Reader::from_path(dir.path().join("noise_summary.csv")).unwrap();
        let rows = rows.records().map(Result::unwrap).collect::<Vec<_>>();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get(1), Some("0"));
        assert_eq!(rows[0].get(2), Some("1800"));
        assert!(rows[0][7].is_empty());
        assert_eq!(rows[1].get(1), Some("1800"));
        assert_eq!(rows[1].get(2), Some("3600"));
        assert!(rows[1][7].is_empty());
    }

    #[test]
    fn arithmetic_mean_does_not_overflow_representable_results() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("records.csv"),
            "receiver_id,period_start_seconds,period_end_seconds,metric,unit,value\nr1,0,3600,other,unit,1e308\nr1,0,3600,other,unit,1e308\n",
        )
        .unwrap();
        write(
            dir.path(),
            dir.path(),
            &NoiseInputs {
                records: "records.csv".into(),
                affected_population: None,
            },
        )
        .unwrap();
        let mut rows = csv::Reader::from_path(dir.path().join("noise_summary.csv")).unwrap();
        let row = rows.records().next().unwrap().unwrap();
        assert_eq!(row[5].parse::<f64>().unwrap(), 1e308);
    }
}
