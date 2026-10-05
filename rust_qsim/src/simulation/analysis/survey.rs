//! Compare weighted travel-survey journey records with the latest run's journeys.

use super::{AnalysisError, io_error};
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::path::Path;

const JOURNEY_DEFINITION: &str = "matsim-substantive-activities-v1";

#[derive(Deserialize)]
struct Record {
    study_population: String,
    journey_definition: String,
    split: String,
    mode: String,
    purpose: String,
    departure_seconds: String,
    duration_seconds: String,
    distance_meters: String,
    weight: String,
    uncertainty: Option<String>,
}

#[derive(Default)]
struct Cell {
    weight: f64,
    variance: f64,
    uncertainty_supplied: bool,
}

type Key = (String, String, String);

/// Survey records are weighted journeys. Distribution bins intentionally mirror journey
/// analysis categories; duration bins are fixed here so independent studies can share them.
pub(super) fn write(report: &Path, source: &Path) -> Result<(), AnalysisError> {
    let input = File::open(source).map_err(io_error)?;
    let mut reader = ::csv::Reader::from_reader(input);
    let mut survey = BTreeMap::<Key, Cell>::new();
    let mut population = None;
    let mut definitions = BTreeSet::new();
    for (index, row) in reader.deserialize::<Record>().enumerate() {
        let row = row.map_err(|error| {
            AnalysisError::new(format!("invalid journey survey row {}: {error}", index + 2))
        })?;
        let number = |value: &str, field: &str| -> Result<f64, AnalysisError> {
            let parsed = value.parse::<f64>().map_err(|_| {
                AnalysisError::new(format!(
                    "invalid {field} in journey survey row {}",
                    index + 2
                ))
            })?;
            if !parsed.is_finite() {
                return Err(AnalysisError::new(format!(
                    "non-finite {field} in journey survey row {}",
                    index + 2
                )));
            }
            Ok(parsed)
        };
        let study_population = number(&row.study_population, "study_population")?;
        let weight = number(&row.weight, "weight")?;
        let uncertainty = row
            .uncertainty
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .map(|value| number(value, "uncertainty"))
            .transpose()?;
        let uncertainty_value = uncertainty.unwrap_or_default();
        if study_population <= 0.0 || weight < 0.0 || uncertainty_value < 0.0 {
            return Err(AnalysisError::new(format!(
                "journey survey row {} requires positive population and non-negative weight and uncertainty",
                index + 2
            )));
        }
        if row.mode.trim().is_empty() || row.purpose.trim().is_empty() {
            return Err(AnalysisError::new(format!(
                "journey survey row {} requires mode and purpose",
                index + 2
            )));
        }
        if population
            .replace(study_population)
            .is_some_and(|previous| previous != study_population)
        {
            return Err(AnalysisError::new(
                "journey survey rows must use one study_population",
            ));
        }
        if row.split != "calibration" && row.split != "holdout" {
            return Err(AnalysisError::new(format!(
                "invalid split in journey survey row {}: expected calibration or holdout",
                index + 2
            )));
        }
        definitions.insert(row.journey_definition);
        let departure = number(&row.departure_seconds, "departure_seconds")?;
        let duration = optional_number(&row.duration_seconds, number, "duration_seconds")?;
        let distance = optional_number(&row.distance_meters, number, "distance_meters")?;
        if departure < 0.0
            || duration.is_some_and(|value| value < 0.0)
            || distance.is_some_and(|value| value < 0.0)
        {
            return Err(AnalysisError::new(format!(
                "journey survey row {} has a negative journey time or distance",
                index + 2
            )));
        }
        let mut categories = vec![
            ("mode", row.mode),
            ("purpose", row.purpose),
            ("departure", format!("{}", (departure as u64) / 3600 * 3600)),
        ];
        if let Some(duration) = duration {
            categories.push(("duration", duration_class(duration).to_owned()));
        }
        categories.push(("distance", super::distance_class(distance).to_owned()));
        for (metric, category) in categories {
            let cell = survey
                .entry((row.split.clone(), metric.to_owned(), category))
                .or_default();
            cell.weight += weight;
            cell.variance += (weight * uncertainty_value).powi(2);
            cell.uncertainty_supplied |= uncertainty.is_some();
        }
    }

    if population.is_none() {
        return Err(AnalysisError::new("journey survey has no records"));
    }
    let comparable = definitions.len() == 1 && definitions.contains(JOURNEY_DEFINITION);
    let study_population = population.unwrap_or_default();
    let mut observed_denominators = BTreeMap::<(String, String), f64>::new();
    for ((split, metric, _), cell) in &survey {
        *observed_denominators
            .entry((split.clone(), metric.clone()))
            .or_default() += cell.weight;
    }
    let mut simulated = BTreeMap::<(String, String), u64>::new();
    let mut simulation_denominators = BTreeMap::<String, u64>::new();
    let mut journeys = ::csv::Reader::from_path(report.join("journeys.csv"))
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    let headers = journeys
        .headers()
        .map_err(|error| AnalysisError::new(error.to_string()))?
        .clone();
    let column = |name: &str| {
        headers
            .iter()
            .position(|header| header == name)
            .ok_or_else(|| AnalysisError::new(format!("journeys.csv is missing {name}")))
    };
    let mode = column("main_mode")?;
    let purpose = column("purpose")?;
    let departure = column("departure_seconds")?;
    let duration = column("duration_seconds")?;
    let distance = column("distance_class")?;
    let completion = column("completion")?;
    for row in journeys.records() {
        let row = row.map_err(|error| AnalysisError::new(error.to_string()))?;
        let Some(departure_seconds) = row
            .get(departure)
            .filter(|value| !value.is_empty())
            .and_then(|value| value.parse::<f64>().ok())
        else {
            continue;
        };
        let mut categories = vec![
            ("mode", row.get(mode).unwrap_or_default().to_owned()),
            ("purpose", row.get(purpose).unwrap_or_default().to_owned()),
            (
                "departure",
                format!("{}", departure_seconds as u64 / 3600 * 3600),
            ),
        ];
        if let Some(value) = row.get(duration).filter(|value| !value.is_empty()) {
            categories.push((
                "duration",
                duration_class(value.parse().unwrap_or_default()).to_owned(),
            ));
        }
        categories.push((
            "distance",
            row.get(distance).unwrap_or("unknown").to_owned(),
        ));
        for (metric, category) in categories {
            if metric != "duration" || row.get(completion) == Some("completed") {
                *simulated.entry((metric.to_owned(), category)).or_default() += 1;
                *simulation_denominators
                    .entry(metric.to_owned())
                    .or_default() += 1;
            }
        }
    }

    let mut keys: BTreeSet<Key> = survey.keys().cloned().collect();
    for ((metric, category), _) in &simulated {
        for split in ["calibration", "holdout"] {
            keys.insert((split.to_owned(), metric.clone(), category.clone()));
        }
    }
    let mut writer = ::csv::Writer::from_path(report.join("journey_survey_comparison.csv"))
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    writer
        .write_record([
            "split",
            "metric",
            "category",
            "observed_weight",
            "observed_share",
            "observed_denominator",
            "simulated_journeys",
            "simulated_share",
            "simulated_denominator",
            "study_population",
            "journey_definition",
            "uncertainty",
            "status",
        ])
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    let definition = definitions.iter().cloned().collect::<Vec<_>>().join("|");
    for (split, metric, category) in keys {
        let observed = survey.get(&(split.clone(), metric.clone(), category.clone()));
        let observed_total = observed_denominators
            .get(&(split.clone(), metric.clone()))
            .copied()
            .unwrap_or_default();
        let simulated_value = simulated
            .get(&(metric.clone(), category.clone()))
            .copied()
            .unwrap_or_default();
        let simulated_total = simulation_denominators
            .get(&metric)
            .copied()
            .unwrap_or_default();
        let status = if !comparable {
            "non_comparable_definition"
        } else if observed.is_none() {
            "missing_observation_group"
        } else if simulated_value == 0 {
            "missing_simulation_group"
        } else {
            "matched"
        };
        writer
            .write_record([
                split.as_str(),
                metric.as_str(),
                category.as_str(),
                if comparable {
                    observed
                        .map(|cell| format!("{:.6}", cell.weight))
                        .unwrap_or_else(|| "0.000000".into())
                } else {
                    String::new()
                }
                .as_str(),
                if comparable && observed_total > 0.0 {
                    format!(
                        "{:.6}",
                        observed.map_or(0.0, |cell| cell.weight / observed_total)
                    )
                } else {
                    String::new()
                }
                .as_str(),
                if comparable {
                    format!("{observed_total:.6}")
                } else {
                    String::new()
                }
                .as_str(),
                simulated_value.to_string().as_str(),
                if simulated_total > 0 {
                    format!("{:.6}", simulated_value as f64 / simulated_total as f64)
                } else {
                    String::new()
                }
                .as_str(),
                simulated_total.to_string().as_str(),
                format!("{study_population:.6}").as_str(),
                definition.as_str(),
                if comparable {
                    observed
                        .filter(|cell| cell.uncertainty_supplied)
                        .map(|cell| format!("{:.6}", cell.variance.sqrt()))
                        .unwrap_or_default()
                } else {
                    String::new()
                }
                .as_str(),
                status,
            ])
            .map_err(|error| AnalysisError::new(error.to_string()))?;
    }
    writer.flush().map_err(io_error)?;
    Ok(())
}

fn optional_number(
    value: &str,
    parse: impl Fn(&str, &str) -> Result<f64, AnalysisError>,
    field: &str,
) -> Result<Option<f64>, AnalysisError> {
    if value.trim().is_empty() {
        Ok(None)
    } else {
        parse(value, field).map(Some)
    }
}

fn duration_class(seconds: f64) -> &'static str {
    match seconds {
        value if value < 900.0 => "under_15_min",
        value if value < 1800.0 => "15_to_30_min",
        value if value < 3600.0 => "30_to_60_min",
        value if value < 7200.0 => "60_to_120_min",
        _ => "120_min_or_more",
    }
}

pub(super) fn write_empty(report: &Path) -> Result<(), AnalysisError> {
    std::fs::write(
        report.join("journey_survey_comparison.csv"),
        "split,metric,category,observed_weight,observed_share,observed_denominator,simulated_journeys,simulated_share,simulated_denominator,study_population,journey_definition,uncertainty,status\n",
    ).map_err(io_error)
}

#[cfg(test)]
mod tests {
    use super::write;
    use std::fs;

    #[test]
    fn weighted_records_keep_split_denominators_and_missing_modes() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("journeys.csv"),
            "main_mode,purpose,departure_seconds,duration_seconds,distance_meters,distance_class,completion\ncar,work,0,600,12000,10_to_25_km,completed\ntransit,education,0,1800,8000,5_to_10_km,completed\n",
        )
        .unwrap();
        let survey = dir.path().join("survey.csv");
        fs::write(
            &survey,
            "study_population,journey_definition,split,mode,purpose,departure_seconds,duration_seconds,distance_meters,weight,uncertainty\n100,matsim-substantive-activities-v1,calibration,car,work,120,600,12000,2,0.5\n100,matsim-substantive-activities-v1,holdout,transit,work,1800,1800,8000,1,0\n",
        )
        .unwrap();

        write(dir.path(), &survey).unwrap();

        let rows = fs::read_to_string(dir.path().join("journey_survey_comparison.csv")).unwrap();
        assert!(
            rows.contains(
                "calibration,mode,car,2.000000,1.000000,2.000000,1,0.500000,2,100.000000"
            )
        );
        assert!(
            rows.contains(
                "holdout,mode,transit,1.000000,1.000000,1.000000,1,0.500000,2,100.000000"
            )
        );
        assert!(rows.contains("matsim-substantive-activities-v1,0.000000,matched"));
        assert!(
            rows.contains(
                "calibration,mode,transit,0.000000,0.000000,2.000000,1,0.500000,2,100.000000"
            ),
            "{rows}"
        );
    }

    #[test]
    fn non_comparable_journey_definitions_are_reported_without_comparing_shares() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("journeys.csv"),
            "main_mode,purpose,departure_seconds,duration_seconds,distance_class,completion\ncar,work,0,600,1_to_5_km,completed\n",
        )
        .unwrap();
        let survey = dir.path().join("survey.csv");
        fs::write(
            &survey,
            "study_population,journey_definition,split,mode,purpose,departure_seconds,duration_seconds,distance_meters,weight\n100,home-based-trip,holdout,car,work,0,600,1000,1\n",
        )
        .unwrap();

        write(dir.path(), &survey).unwrap();

        let rows = fs::read_to_string(dir.path().join("journey_survey_comparison.csv")).unwrap();
        assert!(rows.contains("holdout,mode,car,,"));
        assert!(rows.contains("home-based-trip,,non_comparable_definition"));
    }
}
