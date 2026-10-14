use crate::simulation::config::{Config, ModeParameter};
use crate::simulation::id::Id;
use crate::simulation::replanning::routing::utils::calc_distance;
use crate::simulation::scenario::network::Network;
use crate::simulation::scenario::population::{
    InternalActivity, InternalLeg, InternalPerson, InternalPlan, InternalPlanElement, InternalRoute,
};
use crate::simulation::scenario::trip_structure_utils::get_trip_spans_default;
use crate::simulation::scoring::PlanScorer;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const SECONDS_PER_HOUR: f64 = 3_600.0;
const SECONDS_PER_DAY: f64 = 86_400.0;

#[derive(Debug, Clone, Copy)]
struct ActivityScoringParams {
    typical_duration_s: f64,
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
    marginal_utility_of_money: f64,
    aborted_plan_score_h: f64,
}

/// Scores the event-reconstructed plan of one person.
///
/// Configuration values are deliberately not validated up front. Missing parameters and plan data
/// are reported only when the corresponding activity or leg is actually scored.
#[derive(Debug)]
pub struct CharyparNagelScoringFunction {
    activity_params: BTreeMap<String, ActivityScoringParams>,
    /// Per-subpopulation mode parameters, keyed by subpopulation first (empty string = applies to
    /// every subpopulation without its own override) and mode name second. The nested layout is
    /// required so that two `ModeParameter` entries sharing a `mode` field (e.g. a global "rail"
    /// alongside a subpopulation-specific "rail") keep their distinct marginal utilities; a flat
    /// `BTreeMap<mode, _>` would let the later entry silently overwrite the earlier and disagree
    /// with the routing cost that already saw the per-subpopulation value.
    mode_params: BTreeMap<String, BTreeMap<String, ModeScoringParams>>,
    agent_params: BTreeMap<String, AgentScoringParams>,
    network: Arc<Network>,
    qsim_end_time_s: f64,
}

impl CharyparNagelScoringFunction {
    pub fn new(config: &Config, network: Arc<Network>) -> Self {
        let activity_params = config
            .scoring()
            .activity_params
            .iter()
            .map(|params| {
                (
                    params.activity_type.clone(),
                    ActivityScoringParams {
                        typical_duration_s: params.typical_duration_s,
                    },
                )
            })
            .collect();
        let mode_params = build_mode_params_by_subpopulation(&config.scoring().mode_params);
        let agent_params = config
            .scoring()
            .agent_params
            .iter()
            .map(|params| {
                (
                    params.subpopulation.clone(),
                    AgentScoringParams {
                        marginal_utility_of_performing_s: params.performing / SECONDS_PER_HOUR,
                        marginal_utility_of_money: params.marginal_utility_of_money,
                        aborted_plan_score_h: params.aborted_plan_score,
                    },
                )
            })
            .collect();

        Self {
            activity_params,
            mode_params,
            agent_params,
            network,
            qsim_end_time_s: f64::from(config.qsim().end_time),
        }
    }

    fn score_activities(
        &self,
        person_id: &Id<InternalPerson>,
        experienced_plan: &InternalPlan,
        aborted: bool,
        agent_params: &AgentScoringParams,
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
                self.score_activity_duration(person_id, activity, end_s - start_s, agent_params)
                    .map(|activity_score| score + activity_score)
            });
        }

        if activities.len() == 1 {
            return self.score_activity_duration(
                person_id,
                activities[0],
                SECONDS_PER_DAY,
                agent_params,
            );
        }

        let first = activities[0];
        let last = activities[activities.len() - 1];
        let mut score = 0.0;

        if first.act_type == last.act_type {
            let first_end_s = required_time(person_id, first.end_time, "first activity end")?;
            let last_start_s = required_time(person_id, last.start_time, "last activity start")?;
            score += self.score_activity_duration(
                person_id,
                first,
                first_end_s + SECONDS_PER_DAY - last_start_s,
                agent_params,
            )?;
        } else {
            let first_end_s = required_time(person_id, first.end_time, "first activity end")?;
            score += self.score_activity_duration(person_id, first, first_end_s, agent_params)?;

            let last_start_s = required_time(person_id, last.start_time, "last activity start")?;
            score += self.score_activity_duration(
                person_id,
                last,
                SECONDS_PER_DAY - last_start_s,
                agent_params,
            )?;
        }

        for activity in &activities[1..activities.len() - 1] {
            let start_s = required_time(person_id, activity.start_time, "activity start")?;
            let end_s = required_time(person_id, activity.end_time, "activity end")?;
            score +=
                self.score_activity_duration(person_id, activity, end_s - start_s, agent_params)?;
        }

        Ok(score)
    }

    fn score_activity_duration(
        &self,
        person_id: &Id<InternalPerson>,
        activity: &InternalActivity,
        duration_s: f64,
        agent_params: &AgentScoringParams,
    ) -> Result<f64, String> {
        let params = self
            .activity_params
            .get(activity.act_type.external())
            .ok_or_else(|| {
                format!(
                    "Cannot score person {}: no scoring parameters configured for activity type {}.",
                    person_id.external(),
                    activity.act_type.external()
                )
            })?;

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

        let score = score_activity(
            duration_s,
            params.typical_duration_s,
            agent_params.marginal_utility_of_performing_s,
        );
        // A very short typical duration makes the zero-utility duration underflow to 0, so a
        // duration of 0 (or below) can no longer be scored.
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
        subpopulation: &str,
        plan: &InternalPlan,
        agent_params: &AgentScoringParams,
    ) -> Result<f64, String> {
        let mut score = 0.0;
        let mut seen_modes_in_plan = BTreeSet::new();

        for (trip_index, trip) in get_trip_spans_default(&plan.elements)
            .into_iter()
            .enumerate()
        {
            let mut seen_modes_in_trip = BTreeSet::new();
            for (leg_index, leg) in trip.legs(&plan.elements).enumerate() {
                score += self.score_leg(
                    person_id,
                    subpopulation,
                    trip_index,
                    leg_index,
                    leg,
                    agent_params,
                    &mut seen_modes_in_trip,
                    &mut seen_modes_in_plan,
                )?;
            }
        }

        Ok(score)
    }

    fn score_leg(
        &self,
        person_id: &Id<InternalPerson>,
        subpopulation: &str,
        trip_index: usize,
        leg_index: usize,
        leg: &InternalLeg,
        agent_params: &AgentScoringParams,
        seen_modes_in_trip: &mut BTreeSet<String>,
        seen_modes_in_plan: &mut BTreeSet<String>,
    ) -> Result<f64, String> {
        let mode = leg.mode.external();
        let params = mode_params_for(&self.mode_params, subpopulation, mode).ok_or_else(|| {
            format!(
                "Cannot score person {} trip {trip_index} leg {leg_index}: no scoring parameters configured for mode {mode} in subpopulation {subpopulation}.",
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

        let agent_params = self.agent_params.get(subpopulation).ok_or_else(|| {
            format!(
                "Cannot score person {}: no scoring parameters configured for subpopulation {subpopulation}.",
                person_id.external()
            )
        })?;

        let aborted = is_aborted(experienced_plan);
        let activity_score =
            self.score_activities(person_id, experienced_plan, aborted, agent_params)?;
        let trip_score =
            self.score_trips(person_id, subpopulation, experienced_plan, agent_params)?;
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

/// Build the nested `subpopulation → mode → params` map used by the scorer. A flat
/// `BTreeMap<mode, _>` would let two entries with the same `mode` field but different
/// `subpopulation` fields overwrite each other on insertion, after which the routing layer and
/// the scoring layer could disagree on the same person's marginal utility of traveling. Resolution
/// of duplicates inside a single subpopulation keeps the first occurrence, matching the
/// controller-side `validate()` ordering.
fn build_mode_params_by_subpopulation(
    mode_params: &[ModeParameter],
) -> BTreeMap<String, BTreeMap<String, ModeScoringParams>> {
    let mut nested: BTreeMap<String, BTreeMap<String, ModeScoringParams>> = BTreeMap::new();
    for params in mode_params {
        nested
            .entry(params.subpopulation.clone())
            .or_default()
            .entry(params.mode.clone())
            .or_insert(ModeScoringParams {
                marginal_utility_of_traveling_s: params.marginal_utility_of_traveling
                    / SECONDS_PER_HOUR,
                marginal_utility_of_distance_m: params.marginal_utility_of_distance,
                monetary_distance_cost_rate: params.monetary_distance_cost_rate,
                daily_money_constant: params.daily_money_constant,
                daily_utility_constant: params.daily_utility_constant,
                constant: params.constant,
            });
    }
    nested
}

/// Look up a mode's scoring parameters for a given subpopulation. Order is exact subpopulation
/// first, then the empty-subpopulation entry, matching the routing layer's resolution in
/// `TransitRoutingModule::resolve_routing_params`.
fn mode_params_for<'a>(
    mode_params: &'a BTreeMap<String, BTreeMap<String, ModeScoringParams>>,
    subpopulation: &str,
    mode: &str,
) -> Option<&'a ModeScoringParams> {
    mode_params
        .get(subpopulation)
        .and_then(|by_mode| by_mode.get(mode))
        .or_else(|| mode_params.get("").and_then(|by_mode| by_mode.get(mode)))
}

/// Scores the duration of one activity like MATSim's `ActivityUtilityParameters` with priority 1:
/// `zeroUtilityDuration = typicalDuration * exp(-10h / typicalDuration)`, logarithmic above it and
/// linear (with the slope of the logarithm at that point) below it.
fn score_activity(duration_s: f64, typical_duration_s: f64, beta_performing_s: f64) -> f64 {
    // exp(-10h / typicalDuration) underflows to 0 for very short typical durations. Since
    // ln(d / (t * exp(-x))) = ln(d / t) + x, the logarithmic branch is evaluated in that form,
    // which is mathematically identical but never divides by an underflowed zero-utility duration.
    let zero_utility_exponent = 10.0 * SECONDS_PER_HOUR / typical_duration_s;
    let zero_utility_duration_s = typical_duration_s * (-zero_utility_exponent).exp();
    if duration_s >= zero_utility_duration_s {
        beta_performing_s
            * typical_duration_s
            * ((duration_s / typical_duration_s).ln() + zero_utility_exponent)
    } else {
        let slope = beta_performing_s * typical_duration_s / zero_utility_duration_s;
        -slope * (zero_utility_duration_s - duration_s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::InternalAttributes;
    use crate::simulation::config::{ActivityParameter, AgentParameter, ModeParameter};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Node};
    use crate::simulation::scenario::population::{InternalGenericRoute, InternalNetworkRoute};
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use nohash_hasher::IntSet;
    use std::time::Duration;

    #[test]
    fn activity_score_is_linear_below_zero_utility_duration() {
        let typical = 3_600.0;
        let beta = 6.0 / SECONDS_PER_HOUR;
        // MATSim: typical * exp(-10h / typical) = typical * exp(-10) for a typical duration of 1h.
        let zero = typical * (-10.0_f64).exp();

        assert!(score_activity(zero, typical, beta).abs() < 1e-9);
        assert!((score_activity(typical, typical, beta) - 60.0).abs() < 1e-9);
        assert!(score_activity(zero / 2.0, typical, beta) < 0.0);
        assert!(score_activity(zero * 2.0, typical, beta) > 0.0);
    }

    #[test]
    fn activity_score_matches_matsim_zero_utility_duration_for_other_typical_durations() {
        let typical = 8.0 * SECONDS_PER_HOUR;
        let beta = 6.0 / SECONDS_PER_HOUR;
        // MATSim: typical * exp(-10h / 8h), i.e. not the exp(-1) of a 10h typical duration.
        let zero = typical * (-10.0_f64 / 8.0).exp();

        assert!(score_activity(zero, typical, beta).abs() < 1e-9);
        // Slope of the logarithm at the zero-utility duration.
        let slope = beta * typical / zero;
        assert!((score_activity(0.0, typical, beta) + slope * zero).abs() < 1e-9);
        assert!(
            (score_activity(typical, typical, beta) - beta * typical * 10.0 / 8.0).abs() < 1e-9
        );
    }

    #[test]
    fn activity_score_does_not_divide_by_an_underflowed_zero_utility_duration() {
        // exp(-36000) underflows to 0, but the logarithmic branch stays finite.
        let score = score_activity(60.0, 1.0, 6.0 / SECONDS_PER_HOUR);
        assert!(score.is_finite() && score > 0.0, "{score}");
    }

    #[deterministic_id_test]
    fn scores_overnight_different_boundary_and_single_activities() {
        let scorer = make_scorer(
            vec![
                ("home", 12.0 * SECONDS_PER_HOUR),
                ("work", 8.0 * SECONDS_PER_HOUR),
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
        // The wrapped duration is 12h = typical duration, so the score is beta * typical * 10h / typical = 6 * 10.
        assert_approx_eq(60.0, overnight_score);

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
                ("home", 12.0 * SECONDS_PER_HOUR),
                ("work", 8.0 * SECONDS_PER_HOUR),
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
            vec![("home", SECONDS_PER_DAY), ("work", SECONDS_PER_DAY)],
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
        let params = time_only_scorer.agent_params.get("person").unwrap();
        assert_approx_eq(
            -8.0,
            time_only_scorer
                .score_trips(&person, "person", &two_trips, params)
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
            vec![("home", SECONDS_PER_DAY), ("work", SECONDS_PER_DAY)],
            vec![car, walk, unused],
            Network::new(),
        );
        scorer
            .agent_params
            .get_mut("person")
            .unwrap()
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
        let agent_params = scorer.agent_params.get("person").unwrap();
        assert_approx_eq(
            trip_constants + daily_constants,
            scorer
                .score_trips(&person, "person", &trip_plan, agent_params)
                .unwrap(),
        );
        let activity_score = scorer
            .score_activities(&person, &trip_plan, false, agent_params)
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
                ("home", 12.0 * SECONDS_PER_HOUR),
                ("work", 8.0 * SECONDS_PER_HOUR),
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
            vec![("home", 12.0 * SECONDS_PER_HOUR)],
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
            vec![("home", 12.0 * SECONDS_PER_HOUR)],
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

    fn make_scorer(
        activity_params: Vec<(&str, f64)>,
        mode_params: Vec<ModeParameter>,
        network: Network,
    ) -> CharyparNagelScoringFunction {
        let mut config = Config::default();
        config.scoring_mut().activity_params = activity_params
            .into_iter()
            .map(|(activity_type, typical_duration_s)| ActivityParameter {
                activity_type: activity_type.to_string(),
                typical_duration_s,
            })
            .collect();
        config.scoring_mut().mode_params = mode_params;
        config.scoring_mut().agent_params = vec![AgentParameter::default()];
        CharyparNagelScoringFunction::new(&config, Arc::new(network))
    }

    fn mode(
        mode: &str,
        traveling: f64,
        distance: f64,
        monetary_distance: f64,
        constant: f64,
    ) -> ModeParameter {
        ModeParameter {
            subpopulation: String::new(),
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
            score: None,
            selected: true,
            elements,
            attributes: Default::default(),
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

    /// Regression test: when two `ModeParameter` entries share a `mode` field but live in different
    /// subpopulations (e.g. global `rail` vs. freight-specific `rail`), the scorer must keep both
    /// marginal utilities. A flat `BTreeMap<mode, _>` would silently overwrite one with the other
    /// and let routing and scoring disagree on the same passenger's cost.
    #[test]
    fn per_subpopulation_mode_utility_wins_over_global_default() {
        let mut global_rail = mode("rail", -2.0, 0.0, 0.0, 0.0);
        global_rail.subpopulation = String::new();
        let mut freight_rail = mode("rail", -20.0, 0.0, 0.0, 0.0);
        freight_rail.subpopulation = "freight".to_string();
        let mut scorer = make_scorer(
            vec![("home", SECONDS_PER_DAY), ("work", SECONDS_PER_DAY)],
            vec![global_rail, freight_rail],
            Network::new(),
        );
        // `make_scorer` configures a default `person` agent_param; add a `freight` one too so
        // `PlanScorer::score` can resolve agent_params for the second subpopulation.
        scorer
            .agent_params
            .insert("freight".to_string(), scorer.agent_params["person"]);

        let trip_plan = plan(vec![
            activity("home", None, Some(0)),
            generic_leg("rail", 3_600, None),
            activity("work", Some(3_600), None),
        ]);

        let person_trips = scorer
            .score_trips(
                &Id::create("p-person"),
                "person",
                &trip_plan,
                scorer.agent_params.get("person").unwrap(),
            )
            .unwrap();
        let freight_trips = scorer
            .score_trips(
                &Id::create("p-freight"),
                "freight",
                &trip_plan,
                scorer.agent_params.get("freight").unwrap(),
            )
            .unwrap();
        // `person` falls back to the empty-subpopulation `rail` (-2.0 utils/h × 1 h = -2.0) and
        // `freight` uses its own override (-20.0 utils/h × 1 h = -20.0). The 18-unit gap proves
        // both entries survive insertion: a flat mode-keyed map would have collapsed them.
        assert_approx_eq(-2.0, person_trips);
        assert_approx_eq(-20.0, freight_trips);
        assert_approx_eq(freight_trips - person_trips, -18.0);
    }
}
