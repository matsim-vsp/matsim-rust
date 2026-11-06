//! MATSim's config format (`config_v2.dtd`), read generically, and the conversion of its scoring module into
//! scoring parameter sets.

use crate::simulation::config::{ActivityParameter, ActivityScoring, ModeParameter};
use crate::simulation::io::xml;
use crate::simulation::scoring::{ScoringParameterSet, validate_scoring_parameter_sets};
use crate::simulation::time::SimTime;
use serde::Deserialize;
use std::path::Path;
use tracing::warn;

/// The scoring module is called `planCalcScore` in older configs.
const SCORING_MODULE_NAMES: [&str; 2] = ["scoring", "planCalcScore"];

/// This struct represents the MATSim java config. Right now, it is only used to read the scoring module. Internally, the rust
/// [`Config`](crate::simulation::config::Config) is used.
#[derive(Debug, Deserialize, PartialEq, Clone, Default)]
pub struct IOConfig {
    #[serde(rename = "module", default)]
    pub modules: Vec<IOConfigModule>,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct IOConfigModule {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "param", default)]
    pub params: Vec<IOConfigParam>,
    #[serde(rename = "parameterset", default)]
    pub parameter_sets: Vec<IOConfigParameterSet>,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct IOConfigParameterSet {
    #[serde(rename = "@type")]
    pub set_type: String,
    #[serde(rename = "param", default)]
    pub params: Vec<IOConfigParam>,
    #[serde(rename = "parameterset", default)]
    pub parameter_sets: Vec<IOConfigParameterSet>,
}

#[derive(Debug, Deserialize, PartialEq, Clone)]
pub struct IOConfigParam {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@value")]
    pub value: String,
}

impl IOConfig {
    pub fn from_file(path: impl AsRef<Path>) -> Self {
        xml::read_from_file(path)
    }
}

/// Converts the `scoringParameters` sets of the scoring module, in the order of the config, like
/// MATSim reads them. Parameters that this simulation cannot honor are rejected instead of dropped:
/// unknown names, `latestStartTime`, `earliestEndTime`, `minimalDuration`, a `priority` other than 1,
/// and a `typicalDurationScoreComputation` other than `relative`. Unknown parameter sets, such as
/// `tasteVariations`, are skipped with a warning. Missing values take MATSim's defaults, so that a
/// reduced output config, which omits them, gives the same sets as the full one.
pub fn scoring_parameter_sets(config: &IOConfig) -> Result<Vec<ScoringParameterSet>, String> {
    let module = config
        .modules
        .iter()
        .find(|module| SCORING_MODULE_NAMES.contains(&module.name.as_str()))
        .ok_or_else(|| "The config has no scoring module.".to_string())?;
    let sets = module
        .parameter_sets
        .iter()
        .filter(|set| {
            let known = set.set_type == "scoringParameters";
            if !known {
                warn!(
                    "Skipping the unsupported parameter set {} in the scoring module.",
                    set.set_type
                );
            }
            known
        })
        .map(scoring_parameter_set)
        .collect::<Result<Vec<_>, _>>()?;
    validate_scoring_parameter_sets(&sets)?;
    Ok(sets)
}

fn scoring_parameter_set(set: &IOConfigParameterSet) -> Result<ScoringParameterSet, String> {
    let mut result = ScoringParameterSet {
        activity_params: Vec::new(),
        mode_params: Vec::new(),
        ..ScoringParameterSet::default()
    };
    let mut unknown = Vec::new();
    for param in &set.params {
        match param.name.as_str() {
            "subpopulation" => {
                result.subpopulation = (param.value != "null").then(|| param.value.clone())
            }
            "lateArrival" => result.late_arrival = parse_number(param)?,
            "earlyDeparture" => result.early_departure = parse_number(param)?,
            "performing" => result.performing = parse_number(param)?,
            "waiting" => result.waiting = parse_number(param)?,
            "marginalUtilityOfMoney" => result.marginal_utility_of_money = parse_number(param)?,
            "waitingPt" => result.waiting_pt = Some(parse_number(param)?),
            "utilityOfLineSwitch" => result.utility_of_line_switch = parse_number(param)?,
            _ => unknown.push(param.name.as_str()),
        }
    }
    let describe = result.describe_subpopulation();
    reject_unknown(&unknown, &format!("the scoring parameters for {describe}"))?;

    for nested in &set.parameter_sets {
        match nested.set_type.as_str() {
            "activityParams" => result.activity_params.push(activity_parameter(nested)?),
            "modeParams" => result.mode_params.push(mode_parameter(nested)?),
            other => warn!(
                "Skipping the unsupported parameter set {other} in the scoring parameters for {describe}."
            ),
        }
    }

    result.aborted_plan_score = aborted_plan_score(&result);
    Ok(result)
}

/// MATSim does not configure the score of an aborted plan but derives it in `ScoringParameters`
/// from the worst marginal utility per second, times 24 hours. This simulation takes the same
/// minimum per hour and scales it by the simulated period.
fn aborted_plan_score(set: &ScoringParameterSet) -> f64 {
    let worst_traveling = set
        .mode_params
        .iter()
        .map(|params| params.marginal_utility_of_traveling)
        .fold(0.0, f64::min);
    set.late_arrival
        .min(set.early_departure)
        .min((worst_traveling - set.performing).min(set.waiting - set.performing))
}

fn activity_parameter(set: &IOConfigParameterSet) -> Result<ActivityParameter, String> {
    let mut activity_type = None;
    let mut typical_duration_s = None;
    let mut opening_time_s = None;
    let mut closing_time_s = None;
    let mut score_at_all = None;
    let mut unsupported = Vec::new();
    let mut unknown = Vec::new();
    for param in &set.params {
        match param.name.as_str() {
            "activityType" => activity_type = Some(param.value.clone()),
            "typicalDuration" => typical_duration_s = parse_time(param)?,
            "openingTime" => opening_time_s = parse_time(param)?,
            "closingTime" => closing_time_s = parse_time(param)?,
            "scoringThisActivityAtAll" => score_at_all = Some(parse_bool(param)?),
            "latestStartTime" | "earliestEndTime" | "minimalDuration" => {
                if parse_time(param)?.is_some() {
                    unsupported.push(format!("{} {}", param.name, param.value));
                }
            }
            "priority" => {
                if parse_number(param)? != 1.0 {
                    unsupported.push(format!("priority {}", param.value));
                }
            }
            "typicalDurationScoreComputation" => {
                if param.value != "relative" {
                    unsupported.push(format!("typicalDurationScoreComputation {}", param.value));
                }
            }
            _ => unknown.push(param.name.as_str()),
        }
    }
    let activity_type =
        activity_type.ok_or_else(|| "Activity parameters without activityType.".to_string())?;
    let context = format!("the activity parameters of {activity_type}");
    // MATSim's default config already contains `car interaction`, `pt interaction` and further stage
    // activities that are not scored, and `checkConsistency` adds them for the other modes. A
    // reduced output config therefore omits `scoringThisActivityAtAll=false` for some of them.
    // Like MATSim's `StageActivityTypeIdentifier`, a type ending with `interaction` is a stage
    // activity.
    let score_at_all = score_at_all.unwrap_or(!activity_type.ends_with("interaction"));
    reject_unknown(&unknown, &context)?;
    if !unsupported.is_empty() {
        return Err(format!(
            "Unsupported values in {context}: {}.",
            unsupported.join(", ")
        ));
    }
    if score_at_all && typical_duration_s.is_none() {
        return Err(format!("No typicalDuration in {context}."));
    }

    Ok(ActivityParameter {
        activity_type,
        // Activities that are not scored keep the default of the YAML format.
        typical_duration_s: typical_duration_s.unwrap_or(1.0),
        activity_scoring: if score_at_all {
            ActivityScoring::ConsiderFullActivity
        } else {
            ActivityScoring::Ignore
        },
        opening_time_s,
        closing_time_s,
    })
}

fn mode_parameter(set: &IOConfigParameterSet) -> Result<ModeParameter, String> {
    let mut mode = None;
    let mut result = ModeParameter::default();
    let mut unknown = Vec::new();
    for param in &set.params {
        match param.name.as_str() {
            "mode" => mode = Some(param.value.clone()),
            "marginalUtilityOfTraveling_util_hr" => {
                result.marginal_utility_of_traveling = parse_number(param)?
            }
            "marginalUtilityOfDistance_util_m" => {
                result.marginal_utility_of_distance = parse_number(param)?
            }
            "monetaryDistanceRate" => result.monetary_distance_cost_rate = parse_number(param)?,
            "constant" => result.constant = parse_number(param)?,
            "dailyMonetaryConstant" => result.daily_money_constant = parse_number(param)?,
            "dailyUtilityConstant" => result.daily_utility_constant = parse_number(param)?,
            _ => unknown.push(param.name.as_str()),
        }
    }
    result.mode = mode.ok_or_else(|| "Mode parameters without mode.".to_string())?;
    reject_unknown(&unknown, &format!("the mode parameters of {}", result.mode))?;
    Ok(result)
}

fn reject_unknown(unknown: &[&str], context: &str) -> Result<(), String> {
    if unknown.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Unknown parameters in {context}: {}.",
            unknown.join(", ")
        ))
    }
}

fn parse_number(param: &IOConfigParam) -> Result<f64, String> {
    let value = param
        .value
        .trim()
        .parse::<f64>()
        .map_err(|e| format!("Invalid number {} for {}: {e}", param.value, param.name))?;
    if !value.is_finite() {
        return Err(format!(
            "{} must be finite, got {}.",
            param.name, param.value
        ));
    }
    Ok(value)
}

fn parse_bool(param: &IOConfigParam) -> Result<bool, String> {
    param
        .value
        .trim()
        .parse::<bool>()
        .map_err(|e| format!("Invalid boolean {} for {}: {e}", param.value, param.name))
}

/// Reads a time like MATSim writes it, `HH:MM:SS`, seconds, or `undefined`, in seconds.
fn parse_time(param: &IOConfigParam) -> Result<Option<f64>, String> {
    let value = param.value.trim();
    if value == "undefined" {
        return Ok(None);
    }
    let time = if value.contains(':') {
        SimTime::parse(value)
    } else {
        SimTime::parse_decimal_seconds(value)
    }
    .map_err(|e| format!("Invalid time {value} for {}: {e}", param.name))?;
    Ok(Some(time.as_duration().as_secs_f64()))
}

#[cfg(test)]
mod tests {
    use super::{IOConfig, scoring_parameter_sets};
    use crate::simulation::config::{ActivityParameter, ActivityScoring, ModeParameter};
    use crate::simulation::scoring::ScoringParameterSet;
    use quick_xml::de::from_str;

    /// Shaped like a MATSim output config, with a DOCTYPE, comments and other modules.
    const CONFIG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE config SYSTEM "http://www.matsim.org/files/dtd/config_v2.dtd">
<config>
	<module name="controller" >
		<param name="lastIteration" value="10" />
	</module>
	<module name="scoring" >
		<!-- fraction of iterations at which MSA score averaging is started. -->
		<param name="fractionOfIterationsToStartScoreMSA" value="0.9" />
		<parameterset type="scoringParameters" >
			<param name="lateArrival" value="-18.0" />
			<param name="performing" value="6.88" />
			<param name="subpopulation" value="null" />
			<param name="waiting" value="-1.0" />
			<param name="waitingPt" value="0.0" />
			<parameterset type="activityParams" >
				<param name="activityType" value="leisure_3600" />
				<!-- Closing time of activity type. -->
				<param name="closingTime" value="27:00:00" />
				<param name="earliestEndTime" value="undefined" />
				<param name="latestStartTime" value="undefined" />
				<param name="minimalDuration" value="undefined" />
				<param name="openingTime" value="09:00:00" />
				<param name="priority" value="1.0" />
				<param name="scoringThisActivityAtAll" value="true" />
				<param name="typicalDuration" value="01:00:00" />
				<param name="typicalDurationScoreComputation" value="relative" />
			</parameterset>
			<parameterset type="activityParams" >
				<!-- Like in a reduced output config, which omits MATSim's default scoringThisActivityAtAll=false. -->
				<param name="activityType" value="car interaction" />
				<param name="typicalDuration" value="undefined" />
			</parameterset>
			<parameterset type="modeParams" >
				<param name="constant" value="0.5" />
				<param name="dailyMonetaryConstant" value="-5.0" />
				<param name="marginalUtilityOfTraveling_util_hr" value="-0.5" />
				<param name="mode" value="car" />
				<param name="monetaryDistanceRate" value="-1.49E-4" />
			</parameterset>
			<parameterset type="modeParams" >
				<param name="marginalUtilityOfTraveling_util_hr" value="-20.0" />
				<param name="mode" value="ride" />
			</parameterset>
			<!-- Not supported, so skipped with a warning. -->
			<parameterset type="tasteVariations" >
				<param name="mode" value="car" />
			</parameterset>
		</parameterset>
		<parameterset type="scoringParameters" >
			<param name="performing" value="2.0" />
			<param name="subpopulation" value="freight" />
		</parameterset>
	</module>
</config>"#;

    #[test]
    fn scoring_parameter_sets_are_converted_from_matsim_config() {
        let config: IOConfig = from_str(CONFIG).unwrap();

        let expected = vec![
            ScoringParameterSet {
                late_arrival: -18.0,
                performing: 6.88,
                waiting: -1.0,
                waiting_pt: Some(0.0),
                // MATSim's minimum of late arrival, early departure, and the worst traveling and
                // waiting, each minus performing: here traveling by ride.
                aborted_plan_score: -20.0 - 6.88,
                activity_params: vec![
                    ActivityParameter {
                        activity_type: "leisure_3600".to_string(),
                        typical_duration_s: 3_600.0,
                        activity_scoring: ActivityScoring::ConsiderFullActivity,
                        opening_time_s: Some(9.0 * 3_600.0),
                        closing_time_s: Some(27.0 * 3_600.0),
                    },
                    ActivityParameter {
                        activity_type: "car interaction".to_string(),
                        typical_duration_s: 1.0,
                        activity_scoring: ActivityScoring::Ignore,
                        opening_time_s: None,
                        closing_time_s: None,
                    },
                ],
                mode_params: vec![
                    ModeParameter {
                        mode: "car".to_string(),
                        marginal_utility_of_traveling: -0.5,
                        marginal_utility_of_distance: 0.0,
                        monetary_distance_cost_rate: -1.49e-4,
                        daily_money_constant: -5.0,
                        daily_utility_constant: 0.0,
                        constant: 0.5,
                    },
                    ModeParameter {
                        marginal_utility_of_traveling: -20.0,
                        ..ModeParameter::default_for_mode("ride")
                    },
                ],
                ..ScoringParameterSet::default()
            },
            ScoringParameterSet {
                subpopulation: Some("freight".to_string()),
                performing: 2.0,
                activity_params: Vec::new(),
                mode_params: Vec::new(),
                ..ScoringParameterSet::default()
            },
        ];
        assert_eq!(scoring_parameter_sets(&config), Ok(expected));

        let latest_start = CONFIG.replace(
            r#"name="latestStartTime" value="undefined""#,
            r#"name="latestStartTime" value="08:00:00""#,
        );
        assert_eq!(
            scoring_parameter_sets(&from_str(&latest_start).unwrap()),
            Err(
                "Unsupported values in the activity parameters of leisure_3600: latestStartTime 08:00:00."
                    .to_string()
            )
        );
    }
}
