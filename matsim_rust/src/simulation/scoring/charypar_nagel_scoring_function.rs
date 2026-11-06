use crate::simulation::config::ActivityScoring;
use crate::simulation::config::Config;
use crate::simulation::id::Id;
use crate::simulation::replanning::routing::utils::calc_distance;
use crate::simulation::scenario::network::Network;
use crate::simulation::scenario::population::{
    InternalActivity, InternalLeg, InternalPerson, InternalPlan, InternalPlanElement, InternalRoute,
};
use crate::simulation::scenario::trip_structure_utils::get_trip_spans_default;
use crate::simulation::scoring::{
    PlanScorer, ScoringParameterSet, validate_scoring_parameter_sets,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const SECONDS_PER_HOUR: f64 = 3_600.0;
const SECONDS_PER_DAY: f64 = 86_400.0;

#[derive(Debug, Clone, Copy)]
struct ActivityScoringParams {
    typical_duration_s: f64,
    activity_scoring: ActivityScoring,
    opening_time_s: Option<f64>,
    closing_time_s: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct ModeScoringParams {
    marginal_utility_of_traveling_s: f64,
    marginal_utility_of_distance_m: f64,
    monetary_distance_cost_rate: f64,
    daily_money_constant: f64,
    daily_utility_constant: f64,
    constant: f64,
}

#[derive(Debug, Clone, Copy)]
struct AgentScoringParams {
    marginal_utility_of_performing_s: f64,
    marginal_utility_of_waiting_s: f64,
    marginal_utility_of_money: f64,
    aborted_plan_score_h: f64,
    /// `None` takes the marginal utility of traveling by `pt`.
    marginal_utility_of_waiting_pt_s: Option<f64>,
    utility_of_line_switch: f64,
}

/// The scoring parameters of one subpopulation, from one `ScoringParameterSet`.
#[derive(Debug)]
struct SubpopulationScoringParams {
    agent: AgentScoringParams,
    activity_params: BTreeMap<String, ActivityScoringParams>,
    mode_params: BTreeMap<String, ModeScoringParams>,
}

impl SubpopulationScoringParams {
    fn new(set: &ScoringParameterSet) -> Self {
        let activity_params = set
            .activity_params
            .iter()
            .map(|params| {
                (
                    params.activity_type.clone(),
                    ActivityScoringParams {
                        typical_duration_s: params.typical_duration_s,
                        activity_scoring: params.activity_scoring,
                        opening_time_s: params.opening_time_s,
                        closing_time_s: params.closing_time_s,
                    },
                )
            })
            .collect();
        let mode_params = set
            .mode_params
            .iter()
            .map(|params| {
                (
                    params.mode.clone(),
                    ModeScoringParams {
                        marginal_utility_of_traveling_s: params.marginal_utility_of_traveling
                            / SECONDS_PER_HOUR,
                        marginal_utility_of_distance_m: params.marginal_utility_of_distance,
                        monetary_distance_cost_rate: params.monetary_distance_cost_rate,
                        daily_money_constant: params.daily_money_constant,
                        daily_utility_constant: params.daily_utility_constant,
                        constant: params.constant,
                    },
                )
            })
            .collect();
        let agent = AgentScoringParams {
            marginal_utility_of_performing_s: set.performing / SECONDS_PER_HOUR,
            marginal_utility_of_waiting_s: set.waiting / SECONDS_PER_HOUR,
            marginal_utility_of_money: set.marginal_utility_of_money,
            aborted_plan_score_h: set.aborted_plan_score,
            marginal_utility_of_waiting_pt_s: set
                .waiting_pt
                .map(|waiting_pt| waiting_pt / SECONDS_PER_HOUR),
            utility_of_line_switch: set.utility_of_line_switch,
        };

        Self {
            agent,
            activity_params,
            mode_params,
        }
    }
}

/// Scores the event-reconstructed plan of one person.
///
/// Configuration values are deliberately not validated up front. Missing parameters and plan data
/// are reported only when the corresponding activity or leg is actually scored. Only the
/// structure of the parameter sets is checked up front: several sets for one subpopulation, or an
/// activity type or mode that occurs more than once in a set, panic in `new`.
#[derive(Debug)]
pub struct CharyparNagelScoringFunction {
    /// The parameters of the subpopulations with a parameter set of their own.
    params_per_subpopulation: BTreeMap<String, SubpopulationScoringParams>,
    /// The parameters of the default set, for the persons of all other subpopulations.
    default_params: Option<SubpopulationScoringParams>,
    /// The transit passenger modes, whose legs after the first in a trip count as line switches.
    passenger_modes: BTreeSet<String>,
    network: Arc<Network>,
    qsim_end_time_s: f64,
}

impl CharyparNagelScoringFunction {
    /// `scoring_parameters` are the sets that `Scenario::load` read for `config`.
    pub fn new(
        config: &Config,
        scoring_parameters: &[ScoringParameterSet],
        network: Arc<Network>,
    ) -> Self {
        if let Err(e) = validate_scoring_parameter_sets(scoring_parameters) {
            panic!("Invalid scoring parameters: {e}");
        }

        let mut params_per_subpopulation = BTreeMap::new();
        let mut default_params = None;
        for set in scoring_parameters {
            let params = SubpopulationScoringParams::new(set);
            match &set.subpopulation {
                Some(subpopulation) => {
                    params_per_subpopulation.insert(subpopulation.clone(), params);
                }
                None => default_params = Some(params),
            }
        }

        Self {
            params_per_subpopulation,
            default_params,
            passenger_modes: config.transit().passenger_modes.iter().cloned().collect(),
            network,
            qsim_end_time_s: f64::from(config.qsim().end_time),
        }
    }

    fn score_activities(
        &self,
        person_id: &Id<InternalPerson>,
        experienced_plan: &InternalPlan,
        aborted: bool,
        subpopulation_params: &SubpopulationScoringParams,
    ) -> Result<f64, String> {
        let activities = main_activities(experienced_plan);

        if activities.is_empty() {
            if aborted {
                return Ok(0.0);
            }
            return Err(format!(
                "Cannot score person {}: experienced plan contains no main activity.",
                person_id.external()
            ));
        }

        if aborted {
            return activities.iter().try_fold(0.0, |score, activity| {
                let Some((start_s, end_s)) = completed_activity_interval(activity) else {
                    return Ok(score);
                };
                self.score_activity_interval(
                    person_id,
                    activity,
                    start_s,
                    end_s,
                    subpopulation_params,
                )
                .map(|activity_score| score + activity_score)
            });
        }

        if activities.len() == 1 {
            return self.score_activity_interval(
                person_id,
                activities[0],
                0.0,
                SECONDS_PER_DAY,
                subpopulation_params,
            );
        }

        let first = activities[0];
        let last = activities[activities.len() - 1];
        let mut score = 0.0;

        if first.act_type == last.act_type {
            let first_end_s = required_time(person_id, first.end_time, "first activity end")?;
            let last_start_s = required_time(person_id, last.start_time, "last activity start")?;
            // Like MATSim, the overnight activity lasts from the start of the last activity until the
            // end of the first one on the next day.
            score += self.score_activity_interval(
                person_id,
                first,
                last_start_s,
                first_end_s + SECONDS_PER_DAY,
                subpopulation_params,
            )?;
        } else {
            let first_end_s = required_time(person_id, first.end_time, "first activity end")?;
            score += self.score_activity_interval(
                person_id,
                first,
                0.0,
                first_end_s,
                subpopulation_params,
            )?;

            let last_start_s = required_time(person_id, last.start_time, "last activity start")?;
            score += self.score_activity_interval(
                person_id,
                last,
                last_start_s,
                SECONDS_PER_DAY,
                subpopulation_params,
            )?;
        }

        for activity in &activities[1..activities.len() - 1] {
            let start_s = required_time(person_id, activity.start_time, "activity start")?;
            let end_s = required_time(person_id, activity.end_time, "activity end")?;
            score += self.score_activity_interval(
                person_id,
                activity,
                start_s,
                end_s,
                subpopulation_params,
            )?;
        }

        Ok(score)
    }

    /// Scores an activity from the arrival to the departure of the agent, like MATSim's
    /// `CharyparNagelActivityScoring.calcActScore` without `latestStartTime`, `earliestEndTime` and
    /// `minimalDuration`.
    fn score_activity_interval(
        &self,
        person_id: &Id<InternalPerson>,
        activity: &InternalActivity,
        arrival_s: f64,
        departure_s: f64,
        subpopulation_params: &SubpopulationScoringParams,
    ) -> Result<f64, String> {
        let agent_params = &subpopulation_params.agent;
        let params = subpopulation_params
            .activity_params
            .get(activity.act_type.external())
            .ok_or_else(|| {
                format!(
                    "Cannot score person {}: no scoring parameters configured for activity type {}.",
                    person_id.external(),
                    activity.act_type.external()
                )
            })?;

        if params.activity_scoring == ActivityScoring::Ignore {
            return Ok(0.0);
        }

        // The activity is only performed while it is open; before and after, the agent waits:
        // As in MATSim, an opening time after the departure without a closing time gives a negative duration,
        // scored on the linear branch.
        let mut activity_start_s = arrival_s;
        let mut activity_end_s = departure_s;
        if let Some(opening_s) = params.opening_time_s
            && arrival_s < opening_s
        {
            // opened before activity start => activity started later
            activity_start_s = opening_s;
        }
        if let Some(closing_s) = params.closing_time_s
            && closing_s < departure_s
        {
            // closed before activity end => activity ended earlier
            activity_end_s = closing_s;
        }
        if let (Some(opening_s), Some(closing_s)) = (params.opening_time_s, params.closing_time_s)
            && (opening_s > departure_s || closing_s < arrival_s)
        {
            // activity time and opening times didn't overlap
            activity_start_s = departure_s;
            activity_end_s = departure_s;
        }
        let duration_s = activity_end_s - activity_start_s;
        let waiting_s =
            (activity_start_s - arrival_s).max(0.0) + (departure_s - activity_end_s).max(0.0);

        if !duration_s.is_finite() {
            return Err(format!(
                "Cannot score person {} activity {}: duration must be finite, got {duration_s}.",
                person_id.external(),
                activity.act_type.external()
            ));
        }
        if !params.typical_duration_s.is_finite() || params.typical_duration_s <= 0.0 {
            return Err(format!(
                "Cannot score person {} activity {}: typical duration must be finite and positive, got {}.",
                person_id.external(),
                activity.act_type.external(),
                params.typical_duration_s
            ));
        }
        require_finite(
            person_id,
            agent_params.marginal_utility_of_performing_s,
            "marginal utility of performing",
        )?;

        let mut score = score_activity(
            duration_s,
            params.typical_duration_s,
            agent_params.marginal_utility_of_performing_s,
        );
        // Only added when the agent waits, so that scores without opening times stay unchanged.
        if waiting_s > 0.0 {
            require_finite(
                person_id,
                agent_params.marginal_utility_of_waiting_s,
                "marginal utility of waiting",
            )?;
            score += agent_params.marginal_utility_of_waiting_s * waiting_s;
        }
        // An extreme ratio of duration to typical duration overflows the argument of the
        // logarithm, so such a duration can no longer be scored.
        if !score.is_finite() {
            return Err(format!(
                "Cannot score person {} activity {}: score is not finite for duration {duration_s} s and typical duration {} s.",
                person_id.external(),
                activity.act_type.external(),
                params.typical_duration_s
            ));
        }
        Ok(score)
    }

    fn score_trips(
        &self,
        person_id: &Id<InternalPerson>,
        plan: &InternalPlan,
        subpopulation_params: &SubpopulationScoringParams,
    ) -> Result<f64, String> {
        let agent_params = &subpopulation_params.agent;
        let mut score = 0.0;
        let mut seen_modes_in_plan = BTreeSet::new();

        for (trip_index, trip) in get_trip_spans_default(&plan.elements)
            .into_iter()
            .enumerate()
        {
            let mut seen_modes_in_trip = BTreeSet::new();
            let mut transit_legs_in_trip = 0;
            for (leg_index, leg) in trip.legs(&plan.elements).enumerate() {
                score += self.score_leg(
                    person_id,
                    trip_index,
                    leg_index,
                    leg,
                    subpopulation_params,
                    &mut seen_modes_in_trip,
                    &mut seen_modes_in_plan,
                )?;
                if self.passenger_modes.contains(leg.mode.external()) {
                    transit_legs_in_trip += 1;
                    if transit_legs_in_trip > 1 {
                        require_finite(
                            person_id,
                            agent_params.utility_of_line_switch,
                            "utility of line switch",
                        )?;
                        score += agent_params.utility_of_line_switch;
                    }
                }
            }
        }

        Ok(score)
    }

    fn score_leg(
        &self,
        person_id: &Id<InternalPerson>,
        trip_index: usize,
        leg_index: usize,
        leg: &InternalLeg,
        subpopulation_params: &SubpopulationScoringParams,
        seen_modes_in_trip: &mut BTreeSet<String>,
        seen_modes_in_plan: &mut BTreeSet<String>,
    ) -> Result<f64, String> {
        let agent_params = &subpopulation_params.agent;
        let mode = leg.mode.external();
        let params = subpopulation_params.mode_params.get(mode).ok_or_else(|| {
            format!(
                "Cannot score person {} trip {trip_index} leg {leg_index}: no scoring parameters configured for mode {mode}.",
                person_id.external()
            )
        })?;
        for (description, value) in [
            (
                "marginal utility of traveling",
                params.marginal_utility_of_traveling_s,
            ),
            (
                "marginal utility of distance",
                params.marginal_utility_of_distance_m,
            ),
            (
                "monetary distance cost rate",
                params.monetary_distance_cost_rate,
            ),
            ("mode constant", params.constant),
            ("daily money constant", params.daily_money_constant),
            ("daily utility constant", params.daily_utility_constant),
        ] {
            if !value.is_finite() {
                return Err(format!(
                    "Cannot score person {} trip {trip_index} leg {leg_index}: {description} for mode {mode} is not finite.",
                    person_id.external()
                ));
            }
        }
        let travel_time_s = leg.trav_time.ok_or_else(|| {
            format!(
                "Cannot score person {} trip {trip_index} leg {leg_index}: travel time is missing.",
                person_id.external()
            )
        })?;

        let mut score = travel_time_s.as_secs_f64() * params.marginal_utility_of_traveling_s;
        if params.marginal_utility_of_distance_m != 0.0 || params.monetary_distance_cost_rate != 0.0
        {
            let distance_m = self.leg_distance(person_id, trip_index, leg_index, leg)?;
            if !distance_m.is_finite() || distance_m < 0.0 {
                return Err(format!(
                    "Cannot score person {} trip {trip_index} leg {leg_index}: route distance must be finite and non-negative, got {distance_m}.",
                    person_id.external()
                ));
            }
            if params.monetary_distance_cost_rate != 0.0 {
                require_finite(
                    person_id,
                    agent_params.marginal_utility_of_money,
                    "marginal utility of money",
                )?;
            }
            score += distance_m * params.marginal_utility_of_distance_m;
            score += distance_m
                * params.monetary_distance_cost_rate
                * agent_params.marginal_utility_of_money;
        }
        score += self.wait_score(
            person_id,
            trip_index,
            leg_index,
            leg,
            params,
            subpopulation_params,
        )?;
        if seen_modes_in_trip.insert(mode.to_string()) {
            score += params.constant;
        }
        // Daily constants apply once per mode across all trips in this scoring call.
        if seen_modes_in_plan.insert(mode.to_string()) {
            let mut daily_score = params.daily_utility_constant;
            if params.daily_money_constant != 0.0 {
                require_finite(
                    person_id,
                    agent_params.marginal_utility_of_money,
                    "marginal utility of money",
                )?;
                daily_score += params.daily_money_constant * agent_params.marginal_utility_of_money;
            }
            score += daily_score;
        }

        Ok(score)
    }

    /// Like MATSim, the time from the departure until boarding counts as waiting instead of
    /// traveling for legs with a transit route. With the default marginal utility of waiting, both
    /// cost the same for `pt`, so the score does not change. A boarding before the departure,
    /// possible with the planned boarding time of teleported transit, is not rejected, as in
    /// MATSim.
    fn wait_score(
        &self,
        person_id: &Id<InternalPerson>,
        trip_index: usize,
        leg_index: usize,
        leg: &InternalLeg,
        params: &ModeScoringParams,
        subpopulation_params: &SubpopulationScoringParams,
    ) -> Result<f64, String> {
        let Some(InternalRoute::Pt(route)) = &leg.route else {
            return Ok(0.0);
        };
        let context = || {
            format!(
                "Cannot score person {} trip {trip_index} leg {leg_index}",
                person_id.external()
            )
        };
        let boarding_s = route
            .description
            .boarding_time
            .ok_or_else(|| format!("{}: boarding time is missing.", context()))?
            .as_duration()
            .as_secs_f64();
        let departure_s = required_time(person_id, leg.dep_time, "departure")?;
        let waiting_s = match subpopulation_params.agent.marginal_utility_of_waiting_pt_s {
            Some(waiting_s) => waiting_s,
            None => {
                subpopulation_params
                    .mode_params
                    .get("pt")
                    .ok_or_else(|| {
                        format!(
                            "{}: the marginal utility of waiting for transit falls back to that of \
                             traveling by pt, but no scoring parameters are configured for mode pt.",
                            context()
                        )
                    })?
                    .marginal_utility_of_traveling_s
            }
        };
        require_finite(
            person_id,
            waiting_s,
            "marginal utility of waiting for transit",
        )?;
        Ok((boarding_s - departure_s) * (waiting_s - params.marginal_utility_of_traveling_s))
    }

    fn leg_distance(
        &self,
        person_id: &Id<InternalPerson>,
        trip_index: usize,
        leg_index: usize,
        leg: &InternalLeg,
    ) -> Result<f64, String> {
        let route = leg.route.as_ref().ok_or_else(|| {
            format!(
                "Cannot score person {} trip {trip_index} leg {leg_index}: route is missing.",
                person_id.external()
            )
        })?;
        if let Some(distance) = route.as_generic().distance() {
            return Ok(distance);
        }
        if let InternalRoute::Network(network_route) = route {
            return Ok(calc_distance(network_route, 1.0, 1.0, &self.network));
        }

        Err(format!(
            "Cannot score person {} trip {trip_index} leg {leg_index}: route distance is missing.",
            person_id.external()
        ))
    }
}

impl PlanScorer for CharyparNagelScoringFunction {
    fn score(
        &self,
        person_id: &Id<InternalPerson>,
        subpopulation: &str,
        experienced_plan: &InternalPlan,
    ) -> Result<f64, String> {
        if experienced_plan.elements.is_empty() {
            return Ok(0.0);
        }

        let subpopulation_params = self.params_per_subpopulation
            .get(subpopulation)
            .or(self.default_params.as_ref()).ok_or_else(|| {
            format!(
                "Cannot score person {}: no scoring parameters configured for subpopulation {subpopulation} and no default scoring parameters.",
                person_id.external()
            )
        })?;
        let agent_params = &subpopulation_params.agent;

        let aborted = is_aborted(experienced_plan);
        let activity_score =
            self.score_activities(person_id, experienced_plan, aborted, subpopulation_params)?;
        let trip_score = self.score_trips(person_id, experienced_plan, subpopulation_params)?;
        let abort_score = if aborted {
            require_finite(
                person_id,
                agent_params.aborted_plan_score_h,
                "aborted plan score",
            )?;
            self.qsim_end_time_s / SECONDS_PER_HOUR * agent_params.aborted_plan_score_h
        } else {
            0.0
        };

        Ok(activity_score + trip_score + abort_score)
    }
}

fn main_activities(plan: &InternalPlan) -> Vec<&InternalActivity> {
    plan.elements
        .iter()
        .filter_map(InternalPlanElement::as_activity)
        .filter(|activity| !activity.is_interaction())
        .collect()
}

fn completed_activity_interval(activity: &InternalActivity) -> Option<(f64, f64)> {
    match (activity.start_time, activity.end_time) {
        (Some(start), Some(end)) => Some((
            start.as_duration().as_secs_f64(),
            end.as_duration().as_secs_f64(),
        )),
        (None, Some(end)) => Some((0.0, end.as_duration().as_secs_f64())),
        _ => None,
    }
}

fn required_time(
    person_id: &Id<InternalPerson>,
    time: Option<crate::simulation::time::SimTime>,
    description: &str,
) -> Result<f64, String> {
    time.map(|time| time.as_duration().as_secs_f64())
        .ok_or_else(|| {
            format!(
                "Cannot score person {}: {description} time is missing.",
                person_id.external()
            )
        })
}

fn require_finite(
    person_id: &Id<InternalPerson>,
    value: f64,
    description: &str,
) -> Result<(), String> {
    if value.is_finite() {
        Ok(())
    } else {
        Err(format!(
            "Cannot score person {}: {description} is not finite.",
            person_id.external()
        ))
    }
}

fn is_aborted(plan: &InternalPlan) -> bool {
    plan.elements.iter().any(|element| match element {
        InternalPlanElement::Activity(activity) => {
            activity.attributes.get::<bool>("aborted") == Some(true)
        }
        InternalPlanElement::Leg(leg) => leg.attributes.get::<bool>("aborted") == Some(true),
    })
}

/// Scores the duration of one activity like MATSim's `ActivityUtilityParameters` with priority 1 and
/// MATSim's default `typicalDurationScoreComputation = relative` (`SameRelativeScore`):
/// `zeroUtilityDuration = typicalDuration * exp(-1)`, logarithmic above it and linear (with the
/// slope of the logarithm at that point) below it. An activity of typical duration thus scores
/// `beta_performing * typicalDuration`. MATSim's deprecated `uniform` computation
/// (`typicalDuration * exp(-10h / typicalDuration)`) is not supported.
fn score_activity(duration_s: f64, typical_duration_s: f64, beta_performing_s: f64) -> f64 {
    // Since ln(d / (t * exp(-1))) = ln(d / t) + 1, the logarithmic branch is evaluated in that
    // form, which is mathematically identical and keeps the score at the typical duration exact.
    let zero_utility_duration_s = typical_duration_s * (-1.0_f64).exp();
    if duration_s >= zero_utility_duration_s {
        beta_performing_s * typical_duration_s * ((duration_s / typical_duration_s).ln() + 1.0)
    } else {
        let slope = beta_performing_s * typical_duration_s / zero_utility_duration_s;
        -slope * (zero_utility_duration_s - duration_s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::InternalAttributes;
    use crate::simulation::config::{ActivityParameter, ModeParameter};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Node};
    use crate::simulation::scenario::population::{
        InternalGenericRoute, InternalNetworkRoute, InternalPtRoute, InternalPtRouteDescription,
    };
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;
    use std::time::Duration;

    #[test]
    fn activity_score_is_linear_below_zero_utility_duration() {
        let typical = 3_600.0;
        let beta = 6.0 / SECONDS_PER_HOUR;
        // MATSim: typical * exp(-1) for its default relative typical duration score computation.
        let zero = typical * (-1.0_f64).exp();

        assert!(score_activity(zero, typical, beta).abs() < 1e-9);
        assert!((score_activity(typical, typical, beta) - 6.0).abs() < 1e-9);
        assert!(score_activity(zero / 2.0, typical, beta) < 0.0);
        assert!(score_activity(zero * 2.0, typical, beta) > 0.0);
    }

    #[test]
    fn activity_score_matches_matsim_zero_utility_duration_for_other_typical_durations() {
        let typical = 8.0 * SECONDS_PER_HOUR;
        let beta = 6.0 / SECONDS_PER_HOUR;
        // MATSim's relative computation: typical * exp(-1), not the deprecated uniform
        // typical * exp(-10h / 8h), which would score 60 instead of 48 at the typical duration.
        let zero = typical * (-1.0_f64).exp();

        assert!(score_activity(zero, typical, beta).abs() < 1e-9);
        // Slope of the logarithm at the zero-utility duration.
        let slope = beta * typical / zero;
        assert!((score_activity(0.0, typical, beta) + slope * zero).abs() < 1e-9);
        assert!((score_activity(typical, typical, beta) - 48.0).abs() < 1e-9);
    }

    #[deterministic_id_test]
    fn activity_score_is_zero_when_activity_scoring_is_ignore_and_no_mode_params_given() {
        let scorer_wo_act_scoring = make_scorer(
            vec![
                ("home", 12.0 * SECONDS_PER_HOUR, ActivityScoring::Ignore),
                ("work", 8.0 * SECONDS_PER_HOUR, ActivityScoring::Ignore),
            ],
            Vec::new(),
            Network::new(),
        );

        // Verify that the scorer with activity scoring enabled does not return zero for the same plans.
        let scorer_w_act_scoring = make_scorer(
            vec![
                (
                    "home",
                    12.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
                (
                    "work",
                    8.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
            ],
            Vec::new(),
            Network::new(),
        );

        let person = Id::create("person");

        let plan = plan(vec![
            activity("home", None, Some(6 * 3_600)),
            activity("work", Some(6 * 3_600), Some(18 * 3_600)),
            activity("home", Some(18 * 3_600), None),
        ]);

        let score_wo_act_scoring = scorer_wo_act_scoring
            .score(&person, "person", &plan)
            .unwrap();

        let score_w_act_scoring = scorer_w_act_scoring
            .score(&person, "person", &plan)
            .unwrap();

        assert_eq!(0.0, score_wo_act_scoring);
        assert_ne!(0.0, score_w_act_scoring);
    }

    #[deterministic_id_test]
    fn scores_overnight_different_boundary_and_single_activities() {
        let scorer = make_scorer(
            vec![
                (
                    "home",
                    12.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
                (
                    "work",
                    8.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
            ],
            Vec::new(),
            Network::new(),
        );
        let person = Id::create("person");

        let overnight = plan(vec![
            activity("home", None, Some(6 * 3_600)),
            activity("home", Some(18 * 3_600), None),
        ]);
        let overnight_score = scorer.score(&person, "person", &overnight).unwrap();
        // The wrapped duration is 12h = typical duration, so the score is beta * typical = 6 * 12.
        assert_approx_eq(72.0, overnight_score);

        let different = plan(vec![
            activity("home", None, Some(6 * 3_600)),
            activity("work", Some(18 * 3_600), None),
        ]);
        let expected = score_activity(
            6.0 * SECONDS_PER_HOUR,
            12.0 * SECONDS_PER_HOUR,
            6.0 / SECONDS_PER_HOUR,
        ) + score_activity(
            6.0 * SECONDS_PER_HOUR,
            8.0 * SECONDS_PER_HOUR,
            6.0 / SECONDS_PER_HOUR,
        );
        assert_approx_eq(
            expected,
            scorer.score(&person, "person", &different).unwrap(),
        );

        let experienced_single = plan(vec![activity("home", None, None)]);
        assert_approx_eq(
            score_activity(
                SECONDS_PER_DAY,
                12.0 * SECONDS_PER_HOUR,
                6.0 / SECONDS_PER_HOUR,
            ),
            scorer
                .score(&person, "person", &experienced_single)
                .unwrap(),
        );
        assert_eq!(
            scorer.score(&person, "person", &InternalPlan::default()),
            Ok(0.0)
        );
    }

    #[deterministic_id_test]
    fn scores_trip_modes_and_constants_once_per_trip() {
        let mut car = mode("car", -6.0, -0.01, -0.1, 2.0);
        car.daily_money_constant = 10.0;
        car.daily_utility_constant = 10.0;
        let walk = mode("walk", -3.0, 0.0, 0.0, 1.0);
        let scorer = make_scorer(
            vec![
                (
                    "home",
                    12.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
                (
                    "work",
                    8.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
            ],
            vec![car, walk],
            Network::new(),
        );
        let person = Id::create("person");
        let trip_plan = plan(vec![
            activity("home", None, Some(6 * 3_600)),
            generic_leg("car", 1_800, Some(100.0)),
            activity("car interaction", Some(6 * 3_600), Some(6 * 3_600)),
            generic_leg("car", 1_800, Some(200.0)),
            generic_leg("walk", 600, None),
            activity("work", Some(7 * 3_600 + 600), None),
        ]);

        let activities = score_activity(
            6.0 * SECONDS_PER_HOUR,
            12.0 * SECONDS_PER_HOUR,
            6.0 / SECONDS_PER_HOUR,
        ) + score_activity(
            SECONDS_PER_DAY - (7.0 * SECONDS_PER_HOUR + 600.0),
            8.0 * SECONDS_PER_HOUR,
            6.0 / SECONDS_PER_HOUR,
        );
        let car_score = -6.0 + 300.0 * -0.01 + 300.0 * -0.1 * 1.0 + 2.0 + 20.0;
        let walk_score = -0.5 + 1.0;
        assert_approx_eq(
            activities + car_score + walk_score,
            scorer.score(&person, "person", &trip_plan).unwrap(),
        );

        let time_only_scorer = make_scorer(
            vec![
                (
                    "home",
                    SECONDS_PER_DAY,
                    ActivityScoring::ConsiderFullActivity,
                ),
                (
                    "work",
                    SECONDS_PER_DAY,
                    ActivityScoring::ConsiderFullActivity,
                ),
            ],
            vec![mode("car", -6.0, 0.0, 0.0, 2.0)],
            Network::new(),
        );
        let two_trips = plan(vec![
            activity("home", None, Some(0)),
            generic_leg("car", 3_600, None),
            activity("work", Some(3_600), Some(3_600)),
            generic_leg("car", 3_600, None),
            activity("home", Some(7_200), None),
        ]);
        let params = time_only_scorer.default_params.as_ref().unwrap();
        assert_approx_eq(
            -8.0,
            time_only_scorer
                .score_trips(&person, &two_trips, params)
                .unwrap(),
        );
    }

    /// Like MATSim, the time until boarding counts as waiting instead of traveling, and each
    /// transit leg after the first in a trip is a line switch.
    #[deterministic_id_test]
    fn scores_waiting_for_transit_and_line_switches() {
        let mut scorer = make_scorer(
            vec![],
            vec![
                mode("pt", -6.0, 0.0, 0.0, 0.0),
                mode("walk", -3.0, 0.0, 0.0, 0.0),
            ],
            Network::new(),
        );
        let person = Id::create("person");
        // One trip with two rides, after 10 and 5 minutes of waiting.
        let trip_plan = plan(vec![
            activity("home", None, Some(0)),
            generic_leg("walk", 600, None),
            activity("pt interaction", Some(600), Some(600)),
            pt_leg(600, 1_800, 1_200),
            activity("pt interaction", Some(2_400), Some(2_400)),
            pt_leg(2_400, 900, 2_700),
            activity("work", Some(3_300), None),
        ]);
        // By default, waiting costs as much as riding.
        let traveling = -0.5 + (1_800.0 + 900.0) * -6.0 / SECONDS_PER_HOUR;
        let line_switch = -1.0;
        assert_approx_eq(
            traveling + line_switch,
            scorer
                .score_trips(&person, &trip_plan, scorer.default_params.as_ref().unwrap())
                .unwrap(),
        );

        scorer
            .default_params
            .as_mut()
            .unwrap()
            .agent
            .marginal_utility_of_waiting_pt_s = Some(-12.0 / SECONDS_PER_HOUR);
        let waiting = (600.0 + 300.0) * (-12.0 + 6.0) / SECONDS_PER_HOUR;
        assert_approx_eq(
            traveling + line_switch + waiting,
            scorer
                .score_trips(&person, &trip_plan, scorer.default_params.as_ref().unwrap())
                .unwrap(),
        );
    }

    #[deterministic_id_test]
    fn scores_daily_constants_once_per_mode_across_trips() {
        let mgn_utility_money = 2.5;
        let car_daily_money_constant = -4.0;
        let car_daily_utility_constant = 3.0;
        let car_constant = 2.0;
        let walk_daily_money_constant = 2.0;
        let walk_daily_utility_constant = -1.0;
        let walk_constant = 1.0;

        let mut car = mode("car", 0.0, 0.0, 0.0, car_constant);
        car.daily_money_constant = car_daily_money_constant;
        car.daily_utility_constant = car_daily_utility_constant;
        let mut walk = mode("walk", 0.0, 0.0, 0.0, walk_constant);
        walk.daily_money_constant = walk_daily_money_constant;
        walk.daily_utility_constant = walk_daily_utility_constant;

        let mut unused = mode("bike", 0.0, 0.0, 0.0, 100.0);
        unused.daily_money_constant = 100.0;
        unused.daily_utility_constant = 100.0;

        let mut scorer = make_scorer(
            vec![
                (
                    "home",
                    SECONDS_PER_DAY,
                    ActivityScoring::ConsiderFullActivity,
                ),
                (
                    "work",
                    SECONDS_PER_DAY,
                    ActivityScoring::ConsiderFullActivity,
                ),
            ],
            vec![car, walk, unused],
            Network::new(),
        );
        scorer
            .default_params
            .as_mut()
            .unwrap()
            .agent
            .marginal_utility_of_money = mgn_utility_money;
        let person = Id::create("person");
        let trip_plan = plan(vec![
            activity("home", None, Some(0)),
            generic_leg("car", 0, None),
            activity("car interaction", Some(0), Some(0)),
            generic_leg("car", 0, None),
            generic_leg("walk", 0, None),
            activity("work", Some(0), Some(0)),
            generic_leg("walk", 0, None),
            activity("walk interaction", Some(0), Some(0)),
            generic_leg("walk", 0, None),
            generic_leg("car", 0, None),
            activity("home", Some(0), None),
        ]);

        // Both modes occur in both trips, but their daily constants apply only once.
        let trip_constants = 2.0 * (car_constant + walk_constant);
        let daily_constants = (car_daily_utility_constant
            + car_daily_money_constant * mgn_utility_money)
            + (walk_daily_utility_constant + walk_daily_money_constant * mgn_utility_money);
        let params = scorer.default_params.as_ref().unwrap();
        assert_approx_eq(
            trip_constants + daily_constants,
            scorer.score_trips(&person, &trip_plan, params).unwrap(),
        );
        let activity_score = scorer
            .score_activities(&person, &trip_plan, false, params)
            .unwrap();
        let expected = activity_score + trip_constants + daily_constants;
        assert_approx_eq(
            expected,
            scorer.score(&person, "person", &trip_plan).unwrap(),
        );
        assert_approx_eq(
            expected,
            scorer.score(&person, "person", &trip_plan).unwrap(),
        );
    }

    #[deterministic_id_test]
    fn derives_network_distance_and_handles_same_link_route() {
        let (network, start, middle, end) = network();
        let scorer = make_scorer(
            vec![
                (
                    "home",
                    12.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
                (
                    "work",
                    8.0 * SECONDS_PER_HOUR,
                    ActivityScoring::ConsiderFullActivity,
                ),
            ],
            vec![mode("car", 0.0, -1.0, 0.0, 0.0)],
            network,
        );
        let person = Id::create("person");
        let routed = plan(vec![
            activity("home", None, Some(0)),
            network_leg(vec![start.clone(), middle, end]),
            activity("work", Some(1), None),
        ]);
        // With both endpoint positions at 1.0, the start link contributes zero.
        let activity_score = score_activity(0.0, 12.0 * SECONDS_PER_HOUR, 6.0 / SECONDS_PER_HOUR)
            + score_activity(
                SECONDS_PER_DAY - 1.0,
                8.0 * SECONDS_PER_HOUR,
                6.0 / SECONDS_PER_HOUR,
            );
        assert_approx_eq(
            activity_score - 500.0,
            scorer.score(&person, "person", &routed).unwrap(),
        );

        let same_link = plan(vec![
            activity("home", None, Some(0)),
            network_leg(vec![start.clone()]),
            activity("work", Some(1), None),
        ]);
        assert_approx_eq(
            activity_score,
            scorer.score(&person, "person", &same_link).unwrap(),
        );
    }

    #[deterministic_id_test]
    fn adds_end_time_abort_penalty_to_completed_prefix() {
        let scorer = make_scorer(
            vec![(
                "home",
                12.0 * SECONDS_PER_HOUR,
                ActivityScoring::ConsiderFullActivity,
            )],
            vec![mode("car", -6.0, 0.0, 0.0, 0.0)],
            Network::new(),
        );
        let person = Id::create("person");
        let mut incomplete_leg = generic_leg("car", 0, None);
        incomplete_leg.as_leg_mut().unwrap().trav_time = None;
        incomplete_leg
            .as_leg_mut()
            .unwrap()
            .attributes
            .insert("aborted", true);
        let aborted = plan(vec![
            activity("home", None, Some(6 * 3_600)),
            incomplete_leg,
        ]);
        let expected = score_activity(
            6.0 * SECONDS_PER_HOUR,
            12.0 * SECONDS_PER_HOUR,
            6.0 / SECONDS_PER_HOUR,
        ) - 18.0 * 24.0;

        assert_approx_eq(expected, scorer.score(&person, "person", &aborted).unwrap());
    }

    #[deterministic_id_test]
    fn reports_missing_mode_and_required_distance_with_person_context() {
        let scorer = make_scorer(
            vec![(
                "home",
                12.0 * SECONDS_PER_HOUR,
                ActivityScoring::ConsiderFullActivity,
            )],
            vec![mode("car", 0.0, -1.0, 0.0, 0.0)],
            Network::new(),
        );
        let person = Id::create("p-1");
        let missing_distance = plan(vec![
            activity("home", None, Some(0)),
            generic_leg("car", 1, None),
            activity("home", Some(1), None),
        ]);
        let error = scorer
            .score(&person, "person", &missing_distance)
            .unwrap_err();
        assert!(error.contains("p-1"));
        assert!(error.contains("route distance is missing"));

        let unknown_mode = plan(vec![
            activity("home", None, Some(0)),
            generic_leg("bike", 1, Some(1.0)),
            activity("home", Some(1), None),
        ]);
        let error = scorer.score(&person, "person", &unknown_mode).unwrap_err();
        assert!(error.contains("mode bike"));

        let mut missing_travel_time = generic_leg("car", 1, Some(1.0));
        missing_travel_time.as_leg_mut().unwrap().trav_time = None;
        let missing_travel_time = plan(vec![
            activity("home", None, Some(0)),
            missing_travel_time,
            activity("home", Some(1), None),
        ]);
        let error = scorer
            .score(&person, "person", &missing_travel_time)
            .unwrap_err();
        assert!(error.contains("p-1 trip 0 leg 0"));
        assert!(error.contains("travel time is missing"));
    }

    /// Like MATSim's `getScoringParametersOrDefault`, a subpopulation without a parameter set of its
    /// own takes the default set.
    #[deterministic_id_test]
    fn subpopulation_without_own_parameters_falls_back_to_default_set() {
        let set = |subpopulation: Option<&str>, performing: f64| ScoringParameterSet {
            subpopulation: subpopulation.map(str::to_string),
            performing,
            activity_params: vec![ActivityParameter {
                activity_type: "home".to_string(),
                typical_duration_s: SECONDS_PER_DAY,
                activity_scoring: ActivityScoring::ConsiderFullActivity,
                opening_time_s: None,
                closing_time_s: None,
            }],
            mode_params: Vec::new(),
            ..ScoringParameterSet::default()
        };
        let scorer = |sets: Vec<ScoringParameterSet>| {
            CharyparNagelScoringFunction::new(&Config::default(), &sets, Arc::new(Network::new()))
        };
        let person = Id::create("person");
        // A single activity lasts a day, its typical duration, so it scores performing * 24 h.
        let stay_home = plan(vec![activity("home", None, None)]);

        let with_default = scorer(vec![set(None, 6.0), set(Some("freight"), 2.0)]);
        assert_approx_eq(
            48.0,
            with_default.score(&person, "freight", &stay_home).unwrap(),
        );
        assert_approx_eq(
            144.0,
            with_default.score(&person, "person", &stay_home).unwrap(),
        );

        let without_default = scorer(vec![set(Some("freight"), 2.0)]);
        let error = without_default
            .score(&person, "person", &stay_home)
            .unwrap_err();
        assert!(error.contains("no scoring parameters configured for subpopulation person"));
    }

    /// Like MATSim's `calcActScore`, an activity is only performed while it is open, and the agent
    /// waits before and after.
    #[deterministic_id_test]
    fn activity_score_respects_opening_times_like_matsim() {
        let params =
            |activity_type: &str, typical_h: f64, open_h: Option<f64>, close_h: Option<f64>| {
                ActivityParameter {
                    activity_type: activity_type.to_string(),
                    typical_duration_s: typical_h * SECONDS_PER_HOUR,
                    activity_scoring: ActivityScoring::ConsiderFullActivity,
                    opening_time_s: open_h.map(|h| h * SECONDS_PER_HOUR),
                    closing_time_s: close_h.map(|h| h * SECONDS_PER_HOUR),
                }
            };
        let set = ScoringParameterSet {
            waiting: -3.0,
            activity_params: vec![
                params("home", 12.0, None, None),
                params("work", 8.0, Some(8.0), Some(17.0)),
                params("shop", 1.0, Some(8.0), Some(20.0)),
                params("leisure", 6.0, Some(9.0), Some(25.0)),
            ],
            mode_params: Vec::new(),
            ..ScoringParameterSet::default()
        };
        let scorer =
            CharyparNagelScoringFunction::new(&Config::default(), &[set], Arc::new(Network::new()));
        let person = Id::create("person");
        let score = |elements: Vec<InternalPlanElement>| {
            scorer.score(&person, "person", &plan(elements)).unwrap()
        };
        // With performing 6 per hour, d hours of an activity with typical duration t score
        // 6 t (ln(d / t) + 1).
        let performed = |d_h: f64, t_h: f64| 6.0 * t_h * ((d_h / t_h).ln() + 1.0);
        const H: u64 = 3_600;

        // Work is open from 8 to 17: the agent waits 2 h before and 2 h after 9 h of work.
        assert_approx_eq(
            performed(11.0, 12.0) + performed(9.0, 8.0) - 3.0 * 4.0,
            score(vec![
                activity("home", None, Some(6 * H)),
                activity("work", Some(6 * H), Some(19 * H)),
                activity("home", Some(19 * H), None),
            ]),
        );
        // The shop has closed when the agent arrives: it only waits, and the shop scores like a
        // duration of zero, i.e. -performing * typical duration.
        assert_approx_eq(
            performed(10.0, 12.0) - 6.0 * 1.0 - 3.0 * 1.0,
            score(vec![
                activity("home", None, Some(8 * H)),
                activity("shop", Some(21 * H), Some(22 * H)),
                activity("home", Some(22 * H), None),
            ]),
        );
        // Leisure closes at 25, i.e. at 1 at night: of the overnight leisure from 20 to 2, the
        // agent performs 5 h and then waits 1 h.
        assert_approx_eq(
            performed(18.0, 12.0) + performed(5.0, 6.0) - 3.0 * 1.0,
            score(vec![
                activity("leisure", None, Some(2 * H)),
                activity("home", Some(2 * H), Some(20 * H)),
                activity("leisure", Some(20 * H), None),
            ]),
        );
    }

    fn make_scorer(
        activity_params: Vec<(&str, f64, ActivityScoring)>,
        mode_params: Vec<ModeParameter>,
        network: Network,
    ) -> CharyparNagelScoringFunction {
        let set = ScoringParameterSet {
            activity_params: activity_params
                .into_iter()
                .map(
                    |(activity_type, typical_duration_s, activity_scoring)| ActivityParameter {
                        activity_type: activity_type.to_string(),
                        typical_duration_s,
                        activity_scoring,
                        opening_time_s: None,
                        closing_time_s: None,
                    },
                )
                .collect(),
            mode_params,
            ..ScoringParameterSet::default()
        };
        CharyparNagelScoringFunction::new(&Config::default(), &[set], Arc::new(network))
    }

    fn mode(
        mode: &str,
        traveling: f64,
        distance: f64,
        monetary_distance: f64,
        constant: f64,
    ) -> ModeParameter {
        ModeParameter {
            mode: mode.to_string(),
            marginal_utility_of_traveling: traveling,
            marginal_utility_of_distance: distance,
            monetary_distance_cost_rate: monetary_distance,
            daily_money_constant: 0.0,
            daily_utility_constant: 0.0,
            constant,
        }
    }

    fn plan(elements: Vec<InternalPlanElement>) -> InternalPlan {
        InternalPlan {
            elements,
            selected: true,
            ..Default::default()
        }
    }

    fn activity(
        activity_type: &str,
        start_s: Option<u64>,
        end_s: Option<u64>,
    ) -> InternalPlanElement {
        InternalPlanElement::Activity(InternalActivity::new(
            None,
            activity_type,
            Id::create("activity-link"),
            start_s.map(SimTime::from_secs),
            end_s.map(SimTime::from_secs),
            None,
        ))
    }

    fn pt_leg(departure_s: u64, travel_time_s: u64, boarding_s: u64) -> InternalPlanElement {
        let route = InternalPtRoute {
            generic_delegate: InternalGenericRoute::new(
                Id::create("start"),
                Id::create("end"),
                Some(Duration::from_secs(travel_time_s)),
                Some(0.0),
                None,
            ),
            description: InternalPtRouteDescription {
                transit_route_id: Id::create("route"),
                boarding_time: Some(SimTime::from_secs(boarding_s)),
                transit_line_id: Id::create("line"),
                access_facility_id: Id::create("access"),
                egress_facility_id: Id::create("egress"),
            },
        };
        InternalPlanElement::Leg(InternalLeg::new(
            InternalRoute::Pt(route),
            "pt",
            "pt",
            Duration::from_secs(travel_time_s),
            Some(SimTime::from_secs(departure_s)),
        ))
    }

    fn generic_leg(mode: &str, travel_time_s: u64, distance_m: Option<f64>) -> InternalPlanElement {
        let route = InternalGenericRoute::new(
            Id::create("start"),
            Id::create("end"),
            Some(Duration::from_secs(travel_time_s)),
            distance_m,
            None,
        );
        InternalPlanElement::Leg(InternalLeg::new(
            InternalRoute::Generic(route),
            mode,
            mode,
            Duration::from_secs(travel_time_s),
            Some(SimTime::from_secs(0)),
        ))
    }

    fn network_leg(route: Vec<Id<Link>>) -> InternalPlanElement {
        let delegate = InternalGenericRoute::new(
            route.first().unwrap().clone(),
            route.last().unwrap().clone(),
            Some(Duration::from_secs(1)),
            None,
            None,
        );
        InternalPlanElement::Leg(InternalLeg::new(
            InternalRoute::Network(InternalNetworkRoute::new(delegate, route)),
            "car",
            "car",
            Duration::from_secs(1),
            Some(SimTime::from_secs(0)),
        ))
    }

    fn network() -> (Network, Id<Link>, Id<Link>, Id<Link>) {
        let mut network = Network::new();
        let nodes = (0..4)
            .map(|index| Id::create(&format!("scoring-node-{index}")))
            .collect::<Vec<Id<Node>>>();
        for node in &nodes {
            network.add_node(Node::new(node.clone(), Coordinate::default(), 0, 1));
        }
        let start = add_link(&mut network, "scoring-start", &nodes[0], &nodes[1], 100.0);
        let middle = add_link(&mut network, "scoring-middle", &nodes[1], &nodes[2], 200.0);
        let end = add_link(&mut network, "scoring-end", &nodes[2], &nodes[3], 300.0);
        (network, start, middle, end)
    }

    fn add_link(
        network: &mut Network,
        id: &str,
        from: &Id<Node>,
        to: &Id<Node>,
        length: f64,
    ) -> Id<Link> {
        let id = Id::create(id);
        network.add_link(Link {
            id: id.clone(),
            from: from.clone(),
            to: to.clone(),
            length,
            capacity: 1.0,
            freespeed: 1.0,
            permlanes: 1.0,
            modes: IntSet::default(),
            partition: 0,
            attributes: InternalAttributes::default(),
        });
        id
    }

    fn assert_approx_eq(expected: f64, actual: f64) {
        assert!(
            (expected - actual).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }
}
