//! Aggregation of externally modeled MATSim emission records.

use super::{AnalysisError, csv, io_error};
use crate::simulation::config::EmissionsInputs;
use serde::Serialize;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

#[derive(serde::Deserialize)]
struct Record {
    time_seconds: f64,
    pollutant: String,
    unit: String,
    value: f64,
    vehicle_id: String,
    link_id: Option<String>,
    area_id: Option<String>,
    emission_type: String,
}

#[derive(Default)]
struct Total {
    sample: f64,
    expanded: f64,
    records: u64,
}

#[derive(Ord, PartialOrd, Eq, PartialEq)]
struct Key {
    hour: u64,
    pollutant: String,
    unit: String,
    category: String,
    location_type: String,
    location_id: String,
    emission_type: String,
}

#[derive(Serialize)]
struct Provenance<'a> {
    records: &'a str,
    fleet: &'a str,
    emission_factors: &'a str,
    accounting_boundary: &'a str,
    sample_size: f64,
    expansion_factor: f64,
    warm_start_records: u64,
    cold_start_records: u64,
    warm_start_coverage: &'static str,
    cold_start_coverage: &'static str,
    interpretation: &'static str,
}

pub(super) fn write(
    out: &Path,
    source: &Path,
    inputs: &EmissionsInputs,
    vehicle_type_by_id: &BTreeMap<String, String>,
    iteration: u32,
    sample_size: f64,
) -> Result<bool, AnalysisError> {
    if inputs.fleet_provenance.trim().is_empty()
        || inputs.emission_factor_provenance.trim().is_empty()
        || inputs.accounting_boundary.trim().is_empty()
    {
        return Err(AnalysisError::new(
            "emissions require fleet_provenance, emission_factor_provenance and accounting_boundary",
        ));
    }
    let mut reader =
        csv::Reader::from_path(source).map_err(|e| AnalysisError::new(e.to_string()))?;
    let headers = reader
        .headers()
        .map_err(|e| AnalysisError::new(e.to_string()))?
        .clone();
    let iteration_column = headers
        .iter()
        .position(|header| header == "iteration")
        .ok_or_else(|| AnalysisError::new("emissions CSV is missing the iteration column"))?;
    let mut totals = BTreeMap::<Key, Total>::new();
    let (mut warm, mut cold) = (0, 0);
    let mut seen = false;
    for raw in reader.records() {
        let raw = raw.map_err(|e| AnalysisError::new(format!("invalid emissions record: {e}")))?;
        let row_iteration = raw
            .get(iteration_column)
            .and_then(|value| value.parse::<u32>().ok())
            .ok_or_else(|| AnalysisError::new("emissions iteration must be an unsigned integer"))?;
        if row_iteration != iteration {
            continue;
        }
        let row: Record = raw
            .deserialize(Some(&headers))
            .map_err(|e| AnalysisError::new(format!("invalid emissions record: {e}")))?;
        seen = true;
        if !row.time_seconds.is_finite()
            || row.time_seconds < 0.0
            || !row.value.is_finite()
            || row.value < 0.0
            || row.pollutant.trim().is_empty()
            || row.unit.trim().is_empty()
            || row.vehicle_id.trim().is_empty()
        {
            return Err(AnalysisError::new(
                "emissions records require finite non-negative time/value and non-empty pollutant, unit and vehicle_id",
            ));
        }
        let vehicle_type = vehicle_type_by_id.get(&row.vehicle_id).ok_or_else(|| {
            AnalysisError::new(format!(
                "emissions record references unknown vehicle {}",
                row.vehicle_id
            ))
        })?;
        let vehicle_category = inputs.vehicle_categories.get(vehicle_type).ok_or_else(|| {
            AnalysisError::new(format!(
                "no emissions category configured for vehicle type {vehicle_type}"
            ))
        })?;
        match row.emission_type.as_str() {
            "warm" => warm += 1,
            "cold" => cold += 1,
            _ => return Err(AnalysisError::new("emission_type must be 'warm' or 'cold'")),
        }
        let (location_type, location_id) = match (
            row.link_id.filter(|s| !s.is_empty()),
            row.area_id.filter(|s| !s.is_empty()),
        ) {
            (Some(link), None) => ("link", link),
            (None, Some(area)) => ("area", area),
            _ => {
                return Err(AnalysisError::new(
                    "each emissions record must have exactly one of link_id or area_id",
                ));
            }
        };
        let key = Key {
            hour: (row.time_seconds as u64 / 3600) * 3600,
            pollutant: row.pollutant,
            unit: row.unit,
            category: vehicle_category.clone(),
            location_type: location_type.to_owned(),
            location_id,
            emission_type: row.emission_type,
        };
        let total = totals.entry(key).or_default();
        let sample_total = total.sample + row.value;
        let expanded_total = total.expanded + row.value / sample_size;
        if !sample_total.is_finite() || !expanded_total.is_finite() {
            return Err(AnalysisError::new(
                "emissions total overflowed for the configured sample size",
            ));
        }
        total.sample = sample_total;
        total.expanded = expanded_total;
        total.records += 1;
    }
    let mut table =
        BufWriter::new(File::create(out.join("emissions_hourly.csv")).map_err(io_error)?);
    writeln!(table, "hour_start_seconds,pollutant,unit,vehicle_category,location_type,location_id,emission_type,records,total_sample,total_expanded").map_err(io_error)?;
    for (key, total) in &totals {
        writeln!(
            table,
            "{},{},{},{},{},{},{},{},{:.9},{:.9}",
            key.hour,
            csv(&key.pollutant),
            csv(&key.unit),
            csv(&key.category),
            key.location_type,
            csv(&key.location_id),
            key.emission_type,
            total.records,
            total.sample,
            total.expanded
        )
        .map_err(io_error)?;
    }
    let records_path = inputs.records.display().to_string();
    let provenance = Provenance {
        records: &records_path,
        fleet: &inputs.fleet_provenance,
        emission_factors: &inputs.emission_factor_provenance,
        accounting_boundary: &inputs.accounting_boundary,
        sample_size,
        expansion_factor: 1.0 / sample_size,
        warm_start_records: warm,
        cold_start_records: cold,
        warm_start_coverage: if warm > 0 { "available" } else { "absent" },
        cold_start_coverage: if cold > 0 { "available" } else { "absent" },
        interpretation: "emitted mass; not concentration or exposure",
    };
    super::write_json(&out.join("emissions_provenance.json"), &provenance)?;
    Ok(seen)
}

pub(super) fn write_empty(out: &Path) -> Result<(), AnalysisError> {
    std::fs::write(out.join("emissions_hourly.csv"), "hour_start_seconds,pollutant,unit,vehicle_category,location_type,location_id,emission_type,records,total_sample,total_expanded\n").map_err(io_error)?;
    let provenance = out.join("emissions_provenance.json");
    if !provenance.exists() {
        super::write_json(
            &provenance,
            &serde_json::json!({
                "status": "unavailable",
                "reason": "No modeled emissions input is configured",
                "interpretation": "emitted mass; not concentration or exposure"
            }),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn groups_pollutant_units_categories_and_hours_for_requested_iteration() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("records.csv");
        fs::write(&input, concat!(
            "iteration,time_seconds,pollutant,unit,value,vehicle_id,link_id,area_id,emission_type\n",
            "2,3599,CO2,g,10,v1,l1,,warm\n",
            "2,3600,CO2,g,6,v1,,city,cold\n",
            "1,3600,CO2,g,not-a-number,v1,l1,,warm\n",
            "2,3600,NOx,mg,4,v2,l2,,warm\n",
        )).unwrap();
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        let inputs = EmissionsInputs {
            records: input,
            vehicle_categories: BTreeMap::from([
                ("type1".to_owned(), "passenger_car".to_owned()),
                ("type2".to_owned(), "truck".to_owned()),
            ]),
            fleet_provenance: "fleet-v1".to_owned(),
            emission_factor_provenance: "factors-v1".to_owned(),
            accounting_boundary: "tailpipe".to_owned(),
        };

        let vehicle_types = BTreeMap::from([
            ("v1".to_owned(), "type1".to_owned()),
            ("v2".to_owned(), "type2".to_owned()),
        ]);
        assert!(write(&out, &inputs.records, &inputs, &vehicle_types, 2, 0.5).unwrap());

        let table = fs::read_to_string(out.join("emissions_hourly.csv")).unwrap();
        assert!(table.contains(
            "0,\"CO2\",\"g\",\"passenger_car\",link,\"l1\",warm,1,10.000000000,20.000000000"
        ));
        assert!(table.contains(
            "3600,\"CO2\",\"g\",\"passenger_car\",area,\"city\",cold,1,6.000000000,12.000000000"
        ));
        assert!(
            table.contains(
                "3600,\"NOx\",\"mg\",\"truck\",link,\"l2\",warm,1,4.000000000,8.000000000"
            )
        );
        assert!(!table.contains("500"));
        let provenance: serde_json::Value =
            serde_json::from_slice(&fs::read(out.join("emissions_provenance.json")).unwrap())
                .unwrap();
        assert_eq!(provenance["warm_start_records"], 2);
        assert_eq!(provenance["cold_start_records"], 1);
        assert_eq!(
            provenance["interpretation"],
            "emitted mass; not concentration or exposure"
        );
        assert!(!write(&out, &inputs.records, &inputs, &vehicle_types, 9, 0.5).unwrap());
    }
}
