//! Appraisal of explicitly supplied utilities and monetary costs.

use super::{AnalysisError, csv as quote_csv, io_error};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

const HEADER: &str = "scope,entity_id,group,account,value,unit,money_equivalent,money_unit,transfer_id,boundary,source,status,marginal_utility_of_money";

/// Convert supplied records into a ledger. Plan scores are deliberately not an input because
/// this build may write placeholder scores instead of welfare scores.
pub(super) fn write(
    report: &Path,
    source: Option<&Path>,
    output_dir: &Path,
) -> Result<(), AnalysisError> {
    let mut rows = Vec::new();
    if let Some(source) = source {
        let source = if source.is_absolute() {
            source.to_path_buf()
        } else {
            output_dir.join(source)
        };
        let mut reader = csv::Reader::from_path(&source).map_err(|error| {
            AnalysisError::new(format!(
                "cannot read economic inputs {}: {error}",
                source.display()
            ))
        })?;
        let headers = reader
            .headers()
            .map_err(|error| AnalysisError::new(error.to_string()))?
            .clone();
        let index = |name: &str| {
            headers
                .iter()
                .position(|value| value == name)
                .ok_or_else(|| {
                    AnalysisError::new(format!("economic inputs are missing the {name} column"))
                })
        };
        let (
            scope_i,
            id_i,
            group_i,
            account_i,
            value_i,
            unit_i,
            mum_i,
            money_i,
            transfer_i,
            source_i,
        ) = (
            index("scope")?,
            index("entity_id")?,
            index("group")?,
            index("account")?,
            index("value")?,
            index("unit")?,
            index("marginal_utility_of_money")?,
            index("money_unit")?,
            index("transfer_id")?,
            index("source")?,
        );
        for (line, record) in reader.records().enumerate() {
            let record = record.map_err(|error| AnalysisError::new(error.to_string()))?;
            let get = |column| record.get(column).unwrap_or_default().trim();
            let fail = |reason: &str| {
                AnalysisError::new(format!("economic inputs row {}: {reason}", line + 2))
            };
            let scope = get(scope_i);
            let entity_id = get(id_i);
            let group = get(group_i);
            if !matches!(scope, "person" | "group" | "run")
                || (scope != "run" && entity_id.is_empty())
                || (scope == "run" && !entity_id.is_empty())
            {
                return Err(fail(
                    "scope must be person, group, or run with a matching entity_id",
                ));
            }
            let value = get(value_i)
                .parse::<f64>()
                .map_err(|_| fail("value must be numeric"))?;
            if !value.is_finite() {
                return Err(fail("value must be finite"));
            }
            let account = get(account_i);
            let source_name = get(source_i);
            let transfer = get(transfer_i);
            match account {
                "utility" => {
                    if get(unit_i) != "utils" {
                        return Err(fail("utility unit must be utils"));
                    }
                    let money_unit = get(money_i);
                    let conversion = get(mum_i)
                        .parse::<f64>()
                        .ok()
                        .filter(|mum| mum.is_finite() && *mum > 0.0);
                    if let Some(mum) = conversion.filter(|_| !money_unit.is_empty()) {
                        let money_value = value / mum;
                        if !money_value.is_finite() {
                            return Err(fail("converted utility is not finite"));
                        }
                        rows.push(row(
                            scope,
                            entity_id,
                            group,
                            "traveler_utility_money_equivalent",
                            money_value,
                            money_unit,
                            Some(money_value),
                            money_unit,
                            "",
                            "traveler welfare valuation",
                            source_name,
                            "available",
                            get(mum_i),
                        ));
                        rows.push(row(
                            scope,
                            entity_id,
                            group,
                            "traveler_utility",
                            value,
                            "utils",
                            None,
                            money_unit,
                            "",
                            "traveler welfare valuation",
                            source_name,
                            "available",
                            get(mum_i),
                        ));
                    } else {
                        rows.push(row(
                            scope,
                            entity_id,
                            group,
                            "traveler_utility",
                            value,
                            "utils",
                            None,
                            money_unit,
                            "",
                            "traveler welfare valuation",
                            source_name,
                            "unavailable_missing_conversion",
                            get(mum_i),
                        ));
                    }
                }
                "fare" | "toll" => {
                    if get(unit_i).is_empty() || !get(mum_i).is_empty() || transfer.is_empty() {
                        return Err(fail(
                            "fare and toll require a currency unit and transfer_id, and no marginal utility",
                        ));
                    }
                    rows.push(row(
                        scope,
                        entity_id,
                        group,
                        if account == "fare" {
                            "traveler_fare"
                        } else {
                            "traveler_toll"
                        },
                        value,
                        get(unit_i),
                        Some(value),
                        get(unit_i),
                        transfer,
                        "transfer",
                        source_name,
                        "available",
                        "",
                    ));
                    rows.push(row(
                        scope,
                        entity_id,
                        group,
                        "operator_transfer_revenue",
                        value,
                        get(unit_i),
                        Some(value),
                        get(unit_i),
                        transfer,
                        "transfer",
                        source_name,
                        "available",
                        "",
                    ));
                }
                "operator_revenue"
                | "operator_operating_cost"
                | "operator_investment_cost"
                | "external_cost" => {
                    if get(unit_i).is_empty()
                        || !get(mum_i).is_empty()
                        || (account != "operator_revenue" && !transfer.is_empty())
                        || (account == "operator_revenue" && transfer.is_empty())
                    {
                        return Err(fail(
                            "monetary cost/revenue requires a currency unit; operator revenue also requires a transfer_id",
                        ));
                    }
                    if account.ends_with("_cost") && value < 0.0 {
                        return Err(fail(
                            "cost amounts must be non-negative; net social value subtracts them",
                        ));
                    }
                    rows.push(row(
                        scope,
                        entity_id,
                        group,
                        account,
                        value,
                        get(unit_i),
                        Some(value),
                        get(unit_i),
                        transfer,
                        if account == "operator_revenue" {
                            "transfer"
                        } else {
                            "resource cost"
                        },
                        source_name,
                        "available",
                        "",
                    ));
                }
                _ => {
                    return Err(fail(
                        "account must be utility, fare, toll, operator_revenue, operator_operating_cost, operator_investment_cost, or external_cost",
                    ));
                }
            }
        }
    }
    rows.sort();
    let mut ledger =
        BufWriter::new(File::create(report.join("economic_appraisal.csv")).map_err(io_error)?);
    writeln!(ledger, "{HEADER}").map_err(io_error)?;
    for row in rows {
        writeln!(ledger, "{row}").map_err(io_error)?;
    }
    ledger.flush().map_err(io_error)?;
    let mut totals =
        BTreeMap::<(String, String, String, String, String, String), (f64, usize)>::new();
    let mut supplied_accounts = std::collections::BTreeSet::new();
    let mut reader = csv::Reader::from_path(report.join("economic_appraisal.csv"))
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    for record in reader.records() {
        let record = record.map_err(|error| AnalysisError::new(error.to_string()))?;
        supplied_accounts.insert(record.get(3).unwrap_or_default().to_owned());
        let Some(amount) = record
            .get(6)
            .filter(|field| !field.is_empty())
            .and_then(|field| field.parse::<f64>().ok())
        else {
            continue;
        };
        let key = (
            record[0].to_owned(),
            record[1].to_owned(),
            record[2].to_owned(),
            record[3].to_owned(),
            record[7].to_owned(),
            record[9].to_owned(),
        );
        let total = totals.entry(key).or_default();
        total.0 += amount;
        total.1 += 1;
    }
    let mut summary =
        BufWriter::new(File::create(report.join("economic_summary.csv")).map_err(io_error)?);
    writeln!(
        summary,
        "scope,entity_id,group,account,money_unit,boundary,total,net_social_value,observations,included_in_net_social_accounting,status"
    )
    .map_err(io_error)?;
    for ((scope, entity, group, account, unit, boundary), (total, count)) in &totals {
        let included = !matches!(
            account.as_str(),
            "traveler_fare" | "traveler_toll" | "operator_transfer_revenue" | "operator_revenue"
        );
        let net_value = if !included {
            String::new()
        } else if account.ends_with("_cost") {
            format!("{:.6}", -total)
        } else {
            format!("{total:.6}")
        };
        writeln!(
            summary,
            "{},{},{},{},{},{},{total:.6},{net_value},{count},{included},available",
            quote_csv(scope),
            quote_csv(entity),
            quote_csv(group),
            quote_csv(account),
            quote_csv(unit),
            quote_csv(boundary)
        )
        .map_err(io_error)?;
    }
    for account in [
        "traveler_utility",
        "traveler_utility_money_equivalent",
        "traveler_fare",
        "traveler_toll",
        "operator_transfer_revenue",
        "operator_revenue",
        "operator_operating_cost",
        "operator_investment_cost",
        "external_cost",
    ] {
        if !supplied_accounts.contains(account) {
            let included = !matches!(
                account,
                "traveler_fare"
                    | "traveler_toll"
                    | "operator_transfer_revenue"
                    | "operator_revenue"
            );
            writeln!(
                summary,
                "{},{},{},{},{},{},{},{},{},{},{}",
                quote_csv("run"),
                quote_csv(""),
                quote_csv(""),
                quote_csv(account),
                quote_csv(""),
                quote_csv("not supplied or no convertible input"),
                "",
                "",
                0,
                included,
                quote_csv("unavailable_not_supplied")
            )
            .map_err(io_error)?;
        }
    }
    summary.flush().map_err(io_error)
}

pub(super) fn write_empty(report: &Path) -> Result<(), AnalysisError> {
    for (name, header) in [
        ("economic_appraisal.csv", HEADER),
        (
            "economic_summary.csv",
            "scope,entity_id,group,account,money_unit,boundary,total,net_social_value,observations,included_in_net_social_accounting,status",
        ),
    ] {
        let mut writer = BufWriter::new(File::create(report.join(name)).map_err(io_error)?);
        writeln!(writer, "{header}").map_err(io_error)?;
        writer.flush().map_err(io_error)?;
    }
    Ok(())
}

fn row(
    scope: &str,
    entity_id: &str,
    group: &str,
    account: &str,
    value: f64,
    unit: &str,
    money_equivalent: Option<f64>,
    money_unit: &str,
    transfer: &str,
    boundary: &str,
    source: &str,
    status: &str,
    marginal_utility_of_money: &str,
) -> String {
    format!(
        "{},{},{},{},{value:.6},{},{},{},{},{},{},{},{}",
        quote_csv(scope),
        quote_csv(entity_id),
        quote_csv(group),
        quote_csv(account),
        quote_csv(unit),
        money_equivalent
            .map(|value| format!("{value:.6}"))
            .unwrap_or_default(),
        quote_csv(money_unit),
        quote_csv(transfer),
        quote_csv(boundary),
        quote_csv(source),
        quote_csv(status),
        quote_csv(marginal_utility_of_money)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn utility_conversion_and_transfers_are_accounted_once() {
        let root = std::env::temp_dir().join(format!("economic-analysis-{}", std::process::id()));
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,2,USD,,survey\nperson,p1,workers,fare,3,USD,,USD,t1,operator\nrun,,,operator_operating_cost,5,USD,,USD,,ledger\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), &root).unwrap();
        let mut ledger = csv::Reader::from_path(report.join("economic_appraisal.csv")).unwrap();
        let ledger = ledger.records().map(Result::unwrap).collect::<Vec<_>>();
        assert!(ledger.iter().any(
            |row| row.get(3) == Some("traveler_utility_money_equivalent")
                && row.get(6) == Some("5.000000")
        ));
        assert!(
            ledger
                .iter()
                .any(|row| row.get(3) == Some("traveler_fare") && row.get(8) == Some("t1"))
        );
        assert!(
            ledger
                .iter()
                .any(|row| row.get(3) == Some("operator_transfer_revenue")
                    && row.get(8) == Some("t1"))
        );
        let summary = fs::read_to_string(report.join("economic_summary.csv")).unwrap();
        let mut summary = csv::Reader::from_reader(summary.as_bytes());
        let transfer = summary
            .records()
            .map(Result::unwrap)
            .find(|row| row.get(3) == Some("operator_transfer_revenue"))
            .unwrap();
        assert_eq!(&transfer[6], "3.000000");
        assert_eq!(&transfer[7], "");
        assert_eq!(&transfer[9], "false");
        let cost = summary
            .records()
            .map(Result::unwrap)
            .find(|row| row.get(3) == Some("operator_operating_cost"))
            .unwrap();
        assert_eq!(&cost[6], "5.000000");
        assert_eq!(&cost[7], "-5.000000");
        assert!(
            summary
                .records()
                .map(Result::unwrap)
                .any(|row| row.get(3) == Some("external_cost")
                    && row.get(10) == Some("unavailable_not_supplied"))
        );
    }

    #[test]
    fn missing_conversion_and_placeholder_scores_are_not_welfare_evidence() {
        let root =
            std::env::temp_dir().join(format!("economic-analysis-invalid-{}", std::process::id()));
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,,, ,survey\nperson,p1,workers,score,1,utils,1,USD,,placeholder\n").unwrap();
        assert!(write(&report, Some(Path::new("inputs.csv")), &root).is_err());
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,,, ,survey\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), &root).unwrap();
        let mut ledger = csv::Reader::from_path(report.join("economic_appraisal.csv")).unwrap();
        let utility = ledger.records().next().unwrap().unwrap();
        assert_eq!(&utility[3], "traveler_utility");
        assert_eq!(&utility[11], "unavailable_missing_conversion");
        assert_eq!(ledger.records().count(), 0);
    }

    #[test]
    fn missing_cost_inputs_export_empty_accounting_tables() {
        let root =
            std::env::temp_dir().join(format!("economic-analysis-empty-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        write_empty(&root).unwrap();
        let summary = fs::read_to_string(root.join("economic_summary.csv")).unwrap();
        assert_eq!(
            summary.trim(),
            "scope,entity_id,group,account,money_unit,boundary,total,net_social_value,observations,included_in_net_social_accounting,status"
        );
    }
}
