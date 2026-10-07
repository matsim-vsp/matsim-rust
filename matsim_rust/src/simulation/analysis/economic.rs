//! Appraisal of explicitly supplied utilities and monetary costs.

use super::{AnalysisError, CompensatedSum, csv as quote_csv, io_error};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;

const HEADER: &str = "scope,entity_id,group,account,value,unit,money_equivalent,money_unit,transfer_id,boundary,source,status,marginal_utility_of_money";
const SUMMARY_HEADER: &str = "scope,entity_id,group,account,money_unit,boundary,total,net_social_value,observations,included_in_net_social_accounting,status";
/// Summary key: scope, entity, group, account and money unit. The accounting boundary and the
/// net social accounting membership both follow from the account alone.
type SummaryKey = (String, String, String, String, String);
type ScopeKey = (String, String, String);
const UTILITY_ACCOUNT: &str = "traveler_utility_money_equivalent";
const NET_ACCOUNTS: [&str; 4] = [
    UTILITY_ACCOUNT,
    "operator_operating_cost",
    "operator_investment_cost",
    "external_cost",
];

/// Costs subtract from, and benefits add to, the net social value of their account.
fn net_sign(account: &str) -> f64 {
    if account.ends_with("_cost") {
        -1.0
    } else {
        1.0
    }
}

/// Payments and their matching revenue are transfers, so they cancel out of social accounting.
fn is_transfer(account: &str) -> bool {
    boundary(account) == "transfer"
}

/// The accounting boundary an account belongs to. Transfers are excluded from net social
/// accounting; costs are resources consumed; everything else is a valued benefit.
fn boundary(account: &str) -> &'static str {
    match account {
        "traveler_fare" | "traveler_toll" | "operator_transfer_revenue" | "operator_revenue" => {
            "transfer"
        }
        "operator_operating_cost" | "operator_investment_cost" | "external_cost" => "resource cost",
        _ => "traveler welfare valuation",
    }
}

/// Convert supplied records into a ledger. Plan scores are deliberately not an input because
/// this build may write placeholder scores instead of welfare scores.
pub(super) fn write(
    report: &Path,
    source: Option<&Path>,
    output_dir: &Path,
) -> Result<(), AnalysisError> {
    let mut rows = Vec::new();
    let mut totals = BTreeMap::<SummaryKey, CompensatedSum>::new();
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
                    let money_value = conversion
                        .filter(|_| !money_unit.is_empty())
                        .map(|mum| value / mum)
                        .filter(|money_value| money_value.is_finite());
                    if let Some(money_value) = money_value {
                        rows.push(row(
                            scope,
                            entity_id,
                            group,
                            UTILITY_ACCOUNT,
                            money_value,
                            money_unit,
                            Some(money_value),
                            money_unit,
                            "",
                            boundary(UTILITY_ACCOUNT),
                            source_name,
                            "available",
                            get(mum_i),
                        ));
                        add_total(
                            &mut totals,
                            scope,
                            entity_id,
                            group,
                            UTILITY_ACCOUNT,
                            money_unit,
                            money_value,
                        );
                    }
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
                        boundary(UTILITY_ACCOUNT),
                        source_name,
                        if money_value.is_some() {
                            "available"
                        } else {
                            "unavailable_missing_conversion"
                        },
                        get(mum_i),
                    ));
                }
                "fare" | "toll" => {
                    if get(unit_i).is_empty()
                        || get(money_i) != get(unit_i)
                        || !get(mum_i).is_empty()
                        || transfer.is_empty()
                    {
                        return Err(fail(
                            "fare and toll require a currency unit and transfer_id, and no marginal utility",
                        ));
                    }
                    // A payment and the revenue it finances are one transfer, recorded on both
                    // sides so neither traveler outlay nor operator income is lost.
                    let traveler_account = if account == "fare" {
                        "traveler_fare"
                    } else {
                        "traveler_toll"
                    };
                    for ledger_account in [traveler_account, "operator_transfer_revenue"] {
                        rows.push(row(
                            scope,
                            entity_id,
                            group,
                            ledger_account,
                            value,
                            get(unit_i),
                            Some(value),
                            get(unit_i),
                            transfer,
                            boundary(ledger_account),
                            source_name,
                            "available",
                            "",
                        ));
                        add_total(
                            &mut totals,
                            scope,
                            entity_id,
                            group,
                            ledger_account,
                            get(unit_i),
                            value,
                        );
                    }
                }
                "operator_revenue"
                | "operator_operating_cost"
                | "operator_investment_cost"
                | "external_cost" => {
                    if get(unit_i).is_empty()
                        || get(money_i) != get(unit_i)
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
                        boundary(account),
                        source_name,
                        "available",
                        "",
                    ));
                    add_total(
                        &mut totals,
                        scope,
                        entity_id,
                        group,
                        account,
                        get(unit_i),
                        value,
                    );
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
    let mut supplied_accounts = BTreeSet::new();
    let mut unavailable_conversions = BTreeSet::new();
    let mut reader = csv::Reader::from_path(report.join("economic_appraisal.csv"))
        .map_err(|error| AnalysisError::new(error.to_string()))?;
    for record in reader.records() {
        let record = record.map_err(|error| AnalysisError::new(error.to_string()))?;
        supplied_accounts.insert(record.get(3).unwrap_or_default().to_owned());
        if record.get(11) == Some("unavailable_missing_conversion") {
            unavailable_conversions.insert((
                record[0].to_owned(),
                record[1].to_owned(),
                record[2].to_owned(),
                record[7].to_owned(),
            ));
        }
    }
    let mut summary =
        BufWriter::new(File::create(report.join("economic_summary.csv")).map_err(io_error)?);
    writeln!(summary, "{SUMMARY_HEADER}").map_err(io_error)?;
    for ((scope, entity, group, account, unit), total) in &totals {
        let amount = total.value();
        let included = amount.is_some() && !is_transfer(account);
        let net_value = match amount {
            Some(amount) if included => format!("{:.6}", net_sign(account) * amount),
            _ => String::new(),
        };
        let total_value = amount
            .map(|amount| format!("{amount:.6}"))
            .unwrap_or_default();
        let status = if amount.is_some() {
            "available"
        } else {
            "unavailable_overflow"
        };
        writeln!(
            summary,
            "{},{},{},{},{},{},{total_value},{net_value},{},{included},{status}",
            quote_csv(scope),
            quote_csv(entity),
            quote_csv(group),
            quote_csv(account),
            quote_csv(unit),
            quote_csv(boundary(account)),
            total.count
        )
        .map_err(io_error)?;
    }
    for (scope, entity, group, unit) in &unavailable_conversions {
        writeln!(
            summary,
            "{},{},{},{},{},{},{},{},{},{},unavailable_missing_conversion",
            quote_csv(scope),
            quote_csv(entity),
            quote_csv(group),
            quote_csv(UTILITY_ACCOUNT),
            quote_csv(unit),
            quote_csv(boundary(UTILITY_ACCOUNT)),
            "",
            "",
            0,
            false
        )
        .map_err(io_error)?;
    }
    for account in [
        "traveler_utility",
        "traveler_fare",
        "traveler_toll",
        "operator_transfer_revenue",
        "operator_revenue",
    ] {
        if !supplied_accounts.contains(account) {
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
                false,
                quote_csv("unavailable_not_supplied")
            )
            .map_err(io_error)?;
        }
    }
    write_scoped_net_values(&mut summary, &totals, &unavailable_conversions)?;
    summary.flush().map_err(io_error)
}

/// Report a net social value per group and run scope, per money unit.
///
/// A scope is only appraised from what it supplied itself: person utility never stands in for a
/// group total, and a run total never borrows a group's benefits or costs. Anything a scope did
/// not supply stays explicitly unavailable instead of being read as zero.
fn write_scoped_net_values(
    summary: &mut impl Write,
    totals: &BTreeMap<SummaryKey, CompensatedSum>,
    unavailable_conversions: &BTreeSet<(String, String, String, String)>,
) -> Result<(), AnalysisError> {
    let total = |scope: &str, entity: &str, group: &str, account: &str, unit: &str| {
        totals.get(&(
            scope.to_owned(),
            entity.to_owned(),
            group.to_owned(),
            account.to_owned(),
            unit.to_owned(),
        ))
    };
    let mut scopes = BTreeMap::<ScopeKey, BTreeSet<String>>::new();
    let mut units = BTreeSet::new();
    for (scope, entity, group, _, unit) in totals.keys() {
        units.insert(unit.clone());
        add_scope(&mut scopes, scope, entity, group, unit);
    }
    for (scope, entity, group, unit) in unavailable_conversions {
        units.insert(unit.clone());
        add_scope(&mut scopes, scope, entity, group, unit);
    }
    if units.is_empty() {
        units.insert(String::new());
    }
    // A run is appraised as a whole, so every unit seen anywhere gets a run row even when only
    // person or group records supplied it.
    scopes
        .entry(("run".to_owned(), String::new(), String::new()))
        .or_default()
        .extend(units);

    for ((scope, entity, group), units) in scopes {
        for unit in units {
            // A utility without a conversion is already reported as unavailable; it needs no
            // second "not supplied" row.
            let utility_unavailable = unavailable_conversions.contains(&(
                scope.clone(),
                entity.clone(),
                group.clone(),
                unit.clone(),
            ));
            let amounts = NET_ACCOUNTS
                .iter()
                .map(|account| (*account, total(&scope, &entity, &group, account, &unit)))
                .collect::<Vec<_>>();
            for (account, total) in &amounts {
                if total.is_none() && !(*account == UTILITY_ACCOUNT && utility_unavailable) {
                    writeln!(
                        summary,
                        "{},{},{},{},{},{},{},{},0,false,unavailable_not_supplied_at_scope",
                        quote_csv(&scope),
                        quote_csv(&entity),
                        quote_csv(&group),
                        quote_csv(account),
                        quote_csv(&unit),
                        quote_csv("not supplied at this scope"),
                        "",
                        ""
                    )
                    .map_err(io_error)?;
                }
            }
            let mut net = 0.0;
            let mut observations = 0;
            let mut usable = !utility_unavailable;
            for (account, total) in &amounts {
                match total.and_then(CompensatedSum::value) {
                    Some(amount) => {
                        net += net_sign(account) * amount;
                        observations += total.map_or(0, |total| total.count);
                    }
                    None => usable = false,
                }
            }
            let (net, status) = if !usable {
                (None, "unavailable_missing_inputs")
            } else if net.is_finite() {
                (Some(net), "available")
            } else {
                (None, "unavailable_overflow")
            };
            let amount = net.map(|net| format!("{net:.6}")).unwrap_or_default();
            writeln!(
                summary,
                "{},{},{},{},{},{},{amount},{amount},{observations},{},{}",
                quote_csv(&scope),
                quote_csv(&entity),
                quote_csv(&group),
                quote_csv("net_social_value"),
                quote_csv(&unit),
                quote_csv("social accounting"),
                net.is_some(),
                status
            )
            .map_err(io_error)?;
        }
    }
    Ok(())
}

/// Record a money unit for a group or run scope. Person scopes are appraised per person in the
/// per-account rows, so they get no scoped net value.
fn add_scope(
    scopes: &mut BTreeMap<ScopeKey, BTreeSet<String>>,
    scope: &str,
    entity: &str,
    group: &str,
    unit: &str,
) {
    if scope == "group" || scope == "run" {
        scopes
            .entry((scope.to_owned(), entity.to_owned(), group.to_owned()))
            .or_default()
            .insert(unit.to_owned());
    }
}

fn add_total(
    totals: &mut BTreeMap<SummaryKey, CompensatedSum>,
    scope: &str,
    entity: &str,
    group: &str,
    account: &str,
    unit: &str,
    value: f64,
) {
    totals
        .entry((
            scope.to_owned(),
            entity.to_owned(),
            group.to_owned(),
            account.to_owned(),
            unit.to_owned(),
        ))
        .or_default()
        .add(value);
}

pub(super) fn write_empty(report: &Path) -> Result<(), AnalysisError> {
    for (name, header) in [
        ("economic_appraisal.csv", HEADER),
        ("economic_summary.csv", SUMMARY_HEADER),
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
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,2,USD,,survey\nperson,p1,workers,fare,3,USD,,USD,t1,operator\nrun,,,operator_operating_cost,5,USD,,USD,,ledger\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), root).unwrap();
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
        let mut reader = csv::Reader::from_reader(summary.as_bytes());
        let summary = reader.records().map(Result::unwrap).collect::<Vec<_>>();
        let transfer = summary
            .iter()
            .find(|row| row.get(3) == Some("operator_transfer_revenue"))
            .unwrap();
        assert_eq!(&transfer[6], "3.000000");
        assert_eq!(&transfer[7], "");
        assert_eq!(&transfer[9], "false");
        let cost = summary
            .iter()
            .find(|row| row.get(3) == Some("operator_operating_cost"))
            .unwrap();
        assert_eq!(&cost[6], "5.000000");
        assert_eq!(&cost[7], "-5.000000");
        assert!(summary.iter().any(|row| row.get(3) == Some("external_cost")
            && row.get(10) == Some("unavailable_not_supplied_at_scope")));
        assert!(
            summary
                .iter()
                .any(|row| row.get(3) == Some("operator_investment_cost")
                    && row.get(10) == Some("unavailable_not_supplied_at_scope"))
        );
    }

    #[test]
    fn net_social_value_uses_only_complete_inputs_at_the_same_scope() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\ngroup,workers,workers,utility,10,utils,2,USD,,survey\nrun,,,utility,20,utils,2,USD,,survey\nrun,,,operator_operating_cost,2,USD,,USD,,accounts\nrun,,,operator_investment_cost,3,USD,,USD,,accounts\nrun,,,external_cost,1,USD,,USD,,valuation\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), root).unwrap();
        let summary = fs::read_to_string(report.join("economic_summary.csv")).unwrap();
        let mut reader = csv::Reader::from_reader(summary.as_bytes());
        let rows = reader.records().map(Result::unwrap).collect::<Vec<_>>();
        let run_net = rows
            .iter()
            .find(|row| row.get(0) == Some("run") && row.get(3) == Some("net_social_value"))
            .unwrap();
        assert_eq!(&run_net[6], "4.000000");
        assert_eq!(&run_net[7], "4.000000");
        assert_eq!(&run_net[9], "true");
        let group_net = rows
            .iter()
            .find(|row| row.get(0) == Some("group") && row.get(3) == Some("net_social_value"))
            .unwrap();
        assert_eq!(&group_net[10], "unavailable_missing_inputs");
        assert_eq!(&group_net[9], "false");
        assert!(rows.iter().any(|row| {
            row.get(0) == Some("group")
                && row.get(3) == Some("operator_operating_cost")
                && row.get(10) == Some("unavailable_not_supplied_at_scope")
        }));
    }

    #[test]
    fn missing_conversion_and_placeholder_scores_are_not_welfare_evidence() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,,, ,survey\nperson,p1,workers,score,1,utils,1,USD,,placeholder\n").unwrap();
        assert!(write(&report, Some(Path::new("inputs.csv")), root).is_err());
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,,, ,survey\nperson,p2,workers,utility,1e308,utils,1e-308,USD,,survey\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), root).unwrap();
        let mut ledger = csv::Reader::from_path(report.join("economic_appraisal.csv")).unwrap();
        let ledger = ledger.records().map(Result::unwrap).collect::<Vec<_>>();
        let unavailable = ledger
            .iter()
            .find(|row| row.get(1) == Some("p2") && row.get(3) == Some("traveler_utility"))
            .unwrap();
        assert_eq!(&unavailable[11], "unavailable_missing_conversion");
        assert!(ledger.iter().any(|row| row.get(6) == Some("")));
    }

    #[test]
    fn mixed_utility_conversion_keeps_unavailable_entity_in_summary() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nperson,p1,workers,utility,10,utils,2,USD,,survey\nperson,p2,workers,utility,8,utils,,, ,survey\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), root).unwrap();
        let summary = fs::read_to_string(report.join("economic_summary.csv")).unwrap();
        let mut summary = csv::Reader::from_reader(summary.as_bytes());
        let unavailable = summary
            .records()
            .map(Result::unwrap)
            .find(|row| row.get(1) == Some("p2"))
            .unwrap();
        assert_eq!(&unavailable[3], "traveler_utility_money_equivalent");
        assert_eq!(&unavailable[10], "unavailable_missing_conversion");
        assert_eq!(&unavailable[9], "false");
    }

    #[test]
    fn monetary_account_rejects_conflicting_currency_units() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nrun,,,external_cost,10,USD,,EUR,,accounts\n").unwrap();
        assert!(write(&report, Some(Path::new("inputs.csv")), root).is_err());
    }

    #[test]
    fn missing_cost_inputs_export_empty_accounting_tables() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        write_empty(root).unwrap();
        let summary = fs::read_to_string(root.join("economic_summary.csv")).unwrap();
        assert_eq!(summary.trim(), SUMMARY_HEADER);
    }

    #[test]
    fn summary_uses_unrounded_values_and_marks_overflow_unavailable() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let report = root.join("report");
        fs::create_dir_all(&report).unwrap();
        fs::write(root.join("inputs.csv"), "scope,entity_id,group,account,value,unit,marginal_utility_of_money,money_unit,transfer_id,source\nrun,,,external_cost,0.0000004,USD,,USD,,accounts\nrun,,,external_cost,0.0000004,USD,,USD,,accounts\nrun,,,operator_operating_cost,1e308,USD,,USD,,accounts\nrun,,,operator_operating_cost,1e308,USD,,USD,,accounts\n").unwrap();
        write(&report, Some(Path::new("inputs.csv")), root).unwrap();
        let summary = fs::read_to_string(report.join("economic_summary.csv")).unwrap();
        let mut summary = csv::Reader::from_reader(summary.as_bytes());
        let rows = summary.records().map(Result::unwrap).collect::<Vec<_>>();
        let external = rows
            .iter()
            .find(|row| row.get(3) == Some("external_cost"))
            .unwrap();
        assert_eq!(&external[6], "0.000001");
        let overflow = rows
            .iter()
            .find(|row| row.get(3) == Some("operator_operating_cost"))
            .unwrap();
        assert_eq!(&overflow[10], "unavailable_overflow");
        assert_eq!(&overflow[6], "");
        assert_eq!(&overflow[7], "");
        assert_eq!(&overflow[9], "false");
    }
}
