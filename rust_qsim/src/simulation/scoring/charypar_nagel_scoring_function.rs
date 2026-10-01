use crate::simulation::config::Config;
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
    mode_params: BTreeMap<String, ModeScoringParams>,
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
        let mode_params = config
            .scoring()
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
                        constant: params.constant,
                    },
                )
            })
            .collect();
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
        if !params.typical_duration_s.is_finite() || params.typical_duration_s < 0.0 {
            return Err(format!(
                "Cannot score person {} activity {}: typical duration must be finite and non-negative, got {}.",
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

        Ok(score_activity(
            duration_s,
            params.typical_duration_s,
            agent_params.marginal_utility_of_performing_s,
        ))
    }

    fn score_trips(
        &self,
        person_id: &Id<InternalPerson>,
        plan: &InternalPlan,
        agent_params: &AgentScoringParams,
    ) -> Result<f64, String> {
        let mut score = 0.0;

        for (trip_index, trip) in get_trip_spans_default(&plan.elements)
            .into_iter()
            .enumerate()
        {
            let mut seen_modes = BTreeSet::new();
            for (leg_index, leg) in trip.legs(&plan.elements).enumerate() {
                score += self.score_leg(
                    person_id,
                    trip_index,
                    leg_index,
                    leg,
                    agent_params,
                    &mut seen_modes,
                )?;
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
        agent_params: &AgentScoringParams,
        seen_modes: &mut BTreeSet<String>,
    ) -> Result<f64, String> {
        let mode = leg.mode.external();
        let params = self.mode_params.get(mode).ok_or_else(|| {
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
        if seen_modes.insert(mode.to_string()) {
            score += params.constant;
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
        let trip_score = self.score_trips(person_id, experienced_plan, agent_params)?;
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

fn score_activity(duration_s: f64, typical_duration_s: f64, beta_performing_s: f64) -> f64 {
    let zero_utility_duration_s = typical_duration_s * (-1.0_f64).exp();
    if duration_s >= zero_utility_duration_s {
        beta_performing_s * typical_duration_s * (duration_s / zero_utility_duration_s).ln()
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
        let zero = typical / std::f64::consts::E;

        assert_eq!(0.0, score_activity(zero, typical, beta));
        assert!((score_activity(typical, typical, beta) - 6.0).abs() < 1e-12);
        assert!(score_activity(zero / 2.0, typical, beta) < 0.0);
        assert!(score_activity(zero * 2.0, typical, beta) > 0.0);
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
        car.daily_money_constant = 100.0;
        car.daily_utility_constant = 100.0;
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
        let car_score = -6.0 + 300.0 * -0.01 + 300.0 * -0.1 * 1.0 + 2.0;
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
                .score_trips(&person, &two_trips, params)
                .unwrap(),
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
}
