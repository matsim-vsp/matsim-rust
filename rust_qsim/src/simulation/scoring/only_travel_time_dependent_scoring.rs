use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan};
use crate::simulation::scenario::trip_structure_utils::get_trip_spans_default;
use crate::simulation::scoring::PlanScorer;

/// Scores completed trips by their negative elapsed travel time in seconds.
#[derive(Debug, Default, Clone, Copy)]
pub struct OnlyTravelTimeDependentScoring;

impl PlanScorer for OnlyTravelTimeDependentScoring {
    fn score(
        &self,
        person_id: &Id<InternalPerson>,
        _subpopulation: &str,
        experienced_plan: &InternalPlan,
    ) -> Result<f64, String> {
        let mut score = 0.0;

        for (trip_index, trip) in get_trip_spans_default(&experienced_plan.elements)
            .into_iter()
            .enumerate()
        {
            let mut first_departure = None;
            let mut previous_arrival = None;

            for (leg_index, leg) in trip.legs(&experienced_plan.elements).enumerate() {
                let error = |reason: &str| {
                    format!(
                        "Cannot score person {} trip {trip_index} leg {leg_index}: {reason}",
                        person_id.external()
                    )
                };
                let departure = leg
                    .dep_time
                    .ok_or_else(|| error("departure time is missing."))?
                    .as_duration();
                let travel_time = leg
                    .trav_time
                    .ok_or_else(|| error("travel time is missing."))?;
                let arrival = departure
                    .checked_add(travel_time)
                    .ok_or_else(|| error("arrival time overflows."))?;

                if previous_arrival.is_some_and(|previous| departure < previous) {
                    return Err(error("departure precedes the previous leg's arrival."));
                }

                first_departure.get_or_insert(departure);
                previous_arrival = Some(arrival);
            }

            if let (Some(first_departure), Some(last_arrival)) = (first_departure, previous_arrival)
            {
                // Elapsed time also includes waits between legs of the same trip.
                score -= (last_arrival - first_departure).as_secs_f64();
            }
        }

        Ok(score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalLeg, InternalPlanElement,
    };
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::time::Duration;

    #[deterministic_id_test]
    fn scores_completed_trips_with_transfer_waits_but_no_activities() {
        let person = Id::create("person");
        let plan = plan(vec![
            activity("home", None, Some(100)),
            leg("walk", Some(100), Some(10)),
            activity("pt interaction", Some(111), Some(120)),
            leg("pt", Some(120), Some(20)),
            activity("work", Some(141), Some(200)),
            leg("walk", Some(200), Some(10)),
            activity("home", Some(211), None),
        ]);

        assert_eq!(
            OnlyTravelTimeDependentScoring.score(&person, "person", &plan),
            Ok(-50.0)
        );
    }

    #[deterministic_id_test]
    fn empty_and_incomplete_plans_have_no_completed_trip_score() {
        let person = Id::create("person");
        assert_eq!(
            OnlyTravelTimeDependentScoring.score(&person, "person", &InternalPlan::default()),
            Ok(0.0)
        );

        let incomplete = plan(vec![
            activity("home", None, Some(100)),
            leg("walk", Some(100), None),
        ]);
        assert_eq!(
            OnlyTravelTimeDependentScoring.score(&person, "person", &incomplete),
            Ok(0.0)
        );
    }

    #[deterministic_id_test]
    fn reports_missing_and_inconsistent_leg_times() {
        let person = Id::create("person");
        let scorer = OnlyTravelTimeDependentScoring;
        let cases = [
            (
                plan(vec![
                    activity("home", None, Some(100)),
                    leg("walk", None, Some(10)),
                    activity("work", Some(111), None),
                ]),
                "departure time is missing",
            ),
            (
                plan(vec![
                    activity("home", None, Some(100)),
                    leg("walk", Some(100), None),
                    activity("work", Some(111), None),
                ]),
                "travel time is missing",
            ),
            (
                plan(vec![
                    activity("home", None, Some(100)),
                    leg("walk", Some(100), Some(20)),
                    activity("pt interaction", Some(121), Some(110)),
                    leg("pt", Some(110), Some(10)),
                    activity("work", Some(121), None),
                ]),
                "departure precedes the previous leg's arrival",
            ),
        ];

        for (plan, expected) in cases {
            let error = scorer.score(&person, "person", &plan).unwrap_err();
            assert!(error.contains("person trip 0"), "{error}");
            assert!(error.contains(expected), "{error}");
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
            Id::create("link"),
            start_s.map(SimTime::from_secs),
            end_s.map(SimTime::from_secs),
            None,
        ))
    }

    fn leg(
        mode: &str,
        departure_s: Option<u64>,
        travel_time_s: Option<u64>,
    ) -> InternalPlanElement {
        InternalPlanElement::Leg(InternalLeg {
            mode: Id::create(mode),
            routing_mode: None,
            dep_time: departure_s.map(SimTime::from_secs),
            trav_time: travel_time_s.map(Duration::from_secs),
            route: None,
            attributes: Default::default(),
        })
    }
}
