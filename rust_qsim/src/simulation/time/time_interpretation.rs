use crate::simulation::scenario::population::{
    InternalActivity, InternalLeg, InternalPlan, InternalPlanElement,
};
use crate::simulation::time::SimTime;
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TimeInterpretation {
    simulation_start_time: SimTime,
}

impl TimeInterpretation {
    /// Creates a time interpretation with the provided simulation start time.
    pub fn new(simulation_start_time: SimTime) -> Self {
        Self {
            simulation_start_time,
        }
    }

    /// Returns the configured simulation start time used for along-plan calculations.
    pub fn simulation_start_time(&self) -> SimTime {
        self.simulation_start_time
    }

    /// Decides when an activity ends by preferring a fixed end time over start time plus maximum duration.
    pub fn decide_on_activity_end_time(
        activity: &InternalActivity,
        start_time: &SimTime,
    ) -> Option<SimTime> {
        activity.end_time.or_else(|| {
            activity
                .max_dur
                .map(|duration| start_time.saturating_add(duration))
        })
    }

    /// Decides a leg's travel time by preferring the route travel time over the leg travel time.
    pub fn decide_on_leg_travel_time(leg: &InternalLeg) -> Option<Duration> {
        leg.route
            .as_ref()
            .and_then(|route| route.as_generic().trav_time())
            .or(leg.trav_time)
    }

    /// Decides the end time of one plan element from its start time.
    pub fn decide_on_element_end_time(
        element: &InternalPlanElement,
        start_time: &SimTime,
    ) -> Option<SimTime> {
        match element {
            InternalPlanElement::Activity(activity) => {
                Self::decide_on_activity_end_time(activity, start_time)
            }
            InternalPlanElement::Leg(leg) => Self::decide_on_leg_travel_time(leg)
                .map(|travel_time| start_time.saturating_add(travel_time)),
        }
    }

    /// Decides the end time after processing a sequence of plan elements in order.
    pub fn decide_on_elements_end_time(
        elements: &[InternalPlanElement],
        start_time: &SimTime,
    ) -> Option<SimTime> {
        let mut current_time = *start_time;
        for element in elements {
            current_time = Self::decide_on_element_end_time(element, &current_time)?;
        }
        Some(current_time)
    }

    /// Decides an activity's end time along a plan starting at the configured simulation start time.
    pub fn decide_on_activity_end_time_along_plan(
        &self,
        activity: &InternalActivity,
        plan: &InternalPlan,
    ) -> Option<SimTime> {
        let mut current_time = self.simulation_start_time;
        for element in &plan.elements {
            match element {
                InternalPlanElement::Activity(plan_activity)
                    if std::ptr::eq(plan_activity, activity) =>
                {
                    return Self::decide_on_activity_end_time(plan_activity, &current_time);
                }
                _ => {
                    current_time = Self::decide_on_element_end_time(element, &current_time)?;
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::TimeInterpretation;
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPlan, InternalPlanElement,
        InternalRoute,
    };
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::time::Duration;

    fn activity(end_time: Option<SimTime>, max_dur: Option<Duration>) -> InternalActivity {
        InternalActivity::new(
            None,
            "home",
            Id::<Link>::create("link"),
            None,
            end_time,
            max_dur,
        )
    }

    fn route_travel_time(trav_time: Option<Duration>) -> InternalRoute {
        InternalRoute::Generic(InternalGenericRoute::new(
            Id::<Link>::create("from"),
            Id::<Link>::create("to"),
            trav_time,
            None,
            None,
        ))
    }

    fn leg(route_time: Option<Duration>, leg_time: Option<Duration>) -> InternalLeg {
        InternalLeg {
            mode: Id::<String>::create("car"),
            routing_mode: Some(Id::<String>::create("car")),
            dep_time: None,
            trav_time: leg_time,
            route: Some(route_travel_time(route_time)),
            attributes: InternalAttributes::default(),
        }
    }

    fn leg_without_route(leg_time: Option<Duration>) -> InternalLeg {
        InternalLeg {
            mode: Id::<String>::create("walk"),
            routing_mode: Some(Id::<String>::create("walk")),
            dep_time: None,
            trav_time: leg_time,
            route: None,
            attributes: InternalAttributes::default(),
        }
    }

    #[deterministic_id_test]
    fn activity_end_time_wins_over_max_duration() {
        let activity = activity(Some(SimTime::from_secs(10)), Some(Duration::from_secs(100)));

        assert_eq!(
            Some(SimTime::from_secs(10)),
            TimeInterpretation::decide_on_activity_end_time(&activity, &SimTime::from_secs(5))
        );
    }

    #[deterministic_id_test]
    fn activity_without_end_time_uses_max_duration() {
        let activity = activity(None, Some(Duration::from_secs(7)));

        assert_eq!(
            Some(SimTime::from_secs(12)),
            TimeInterpretation::decide_on_activity_end_time(&activity, &SimTime::from_secs(5))
        );
    }

    #[deterministic_id_test]
    fn activity_without_end_time_or_max_duration_is_undefined() {
        let activity = activity(None, None);

        assert_eq!(
            None,
            TimeInterpretation::decide_on_activity_end_time(&activity, &SimTime::from_secs(5))
        );
    }

    #[deterministic_id_test]
    fn fixed_activity_end_time_is_not_shifted_by_late_arrival() {
        let activity = activity(Some(SimTime::from_secs(10)), None);

        assert_eq!(
            Some(SimTime::from_secs(10)),
            TimeInterpretation::decide_on_activity_end_time(&activity, &SimTime::from_secs(15))
        );
    }

    #[deterministic_id_test]
    fn leg_prefers_route_travel_time_over_leg_travel_time() {
        let leg = leg(Some(Duration::from_secs(3)), Some(Duration::from_secs(9)));

        assert_eq!(
            Some(Duration::from_secs(3)),
            TimeInterpretation::decide_on_leg_travel_time(&leg)
        );
    }

    #[deterministic_id_test]
    fn leg_uses_leg_travel_time_when_route_travel_time_is_missing() {
        let leg = leg(None, Some(Duration::from_secs(9)));

        assert_eq!(
            Some(Duration::from_secs(9)),
            TimeInterpretation::decide_on_leg_travel_time(&leg)
        );
    }

    #[deterministic_id_test]
    fn leg_without_any_travel_time_is_undefined() {
        let leg = leg_without_route(None);

        assert_eq!(None, TimeInterpretation::decide_on_leg_travel_time(&leg));
    }

    #[deterministic_id_test]
    fn element_end_time_handles_activities_and_legs() {
        let activity = InternalPlanElement::Activity(activity(None, Some(Duration::from_secs(4))));
        let leg = InternalPlanElement::Leg(leg(Some(Duration::from_secs(6)), None));

        assert_eq!(
            Some(SimTime::from_secs(14)),
            TimeInterpretation::decide_on_element_end_time(&activity, &SimTime::from_secs(10))
        );
        assert_eq!(
            Some(SimTime::from_secs(16)),
            TimeInterpretation::decide_on_element_end_time(&leg, &SimTime::from_secs(10))
        );
    }

    #[deterministic_id_test]
    fn elements_end_time_runs_chain_until_end() {
        let elements = vec![
            InternalPlanElement::Activity(activity(None, Some(Duration::from_secs(4)))),
            InternalPlanElement::Leg(leg(Some(Duration::from_secs(6)), None)),
            InternalPlanElement::Activity(activity(Some(SimTime::from_secs(20)), None)),
        ];

        assert_eq!(
            Some(SimTime::from_secs(20)),
            TimeInterpretation::decide_on_elements_end_time(&elements, &SimTime::from_secs(10))
        );
    }

    #[deterministic_id_test]
    fn elements_end_time_stops_on_undefined_time() {
        let elements = vec![
            InternalPlanElement::Activity(activity(None, Some(Duration::from_secs(4)))),
            InternalPlanElement::Leg(leg_without_route(None)),
            InternalPlanElement::Activity(activity(Some(SimTime::from_secs(20)), None)),
        ];

        assert_eq!(
            None,
            TimeInterpretation::decide_on_elements_end_time(&elements, &SimTime::from_secs(10))
        );
    }

    #[deterministic_id_test]
    fn activity_end_time_along_plan_uses_previous_elements_to_compute_start() {
        let plan = InternalPlan {
            attributes: Default::default(),
            score: None,
            selected: true,
            elements: vec![
                InternalPlanElement::Activity(activity(Some(SimTime::from_secs(5)), None)),
                InternalPlanElement::Leg(leg(Some(Duration::from_secs(7)), None)),
                InternalPlanElement::Activity(activity(None, Some(Duration::from_secs(3)))),
            ],
        };
        let target = plan.elements[2].as_activity().unwrap();

        assert_eq!(
            Some(SimTime::from_secs(15)),
            TimeInterpretation::default().decide_on_activity_end_time_along_plan(target, &plan)
        );
    }

    #[deterministic_id_test]
    fn activity_end_time_along_plan_uses_configured_simulation_start() {
        let time_interpretation = TimeInterpretation::new(SimTime::from_secs(10));
        let plan = InternalPlan {
            attributes: Default::default(),
            score: None,
            selected: true,
            elements: vec![
                InternalPlanElement::Activity(activity(None, Some(Duration::from_secs(5)))),
                InternalPlanElement::Leg(leg(Some(Duration::from_secs(7)), None)),
                InternalPlanElement::Activity(activity(None, Some(Duration::from_secs(3)))),
            ],
        };
        let target = plan.elements[2].as_activity().unwrap();

        assert_eq!(
            Some(SimTime::from_secs(25)),
            time_interpretation.decide_on_activity_end_time_along_plan(target, &plan)
        );
    }

    #[deterministic_id_test]
    fn activity_end_time_along_plan_returns_none_for_activity_not_in_plan() {
        let plan = InternalPlan {
            attributes: Default::default(),
            score: None,
            selected: true,
            elements: vec![InternalPlanElement::Activity(activity(
                Some(SimTime::from_secs(5)),
                None,
            ))],
        };
        let outside_activity = activity(Some(SimTime::from_secs(10)), None);

        assert_eq!(
            None,
            TimeInterpretation::default()
                .decide_on_activity_end_time_along_plan(&outside_activity, &plan)
        );
    }
}
