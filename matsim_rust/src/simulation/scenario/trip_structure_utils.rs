use crate::simulation::scenario::population::{InternalActivity, InternalLeg, InternalPlanElement};
use tracing::error;

/// Returns the main mode for a trip based on its leg sequence.
pub fn identify_main_mode(trip_elements: &[InternalPlanElement]) -> Option<String> {
    let legs: Vec<_> = trip_elements
        .iter()
        .filter_map(InternalPlanElement::as_leg)
        .collect();
    // Use the common routing mode even when access legs or stage activities come first.
    let mut routing_modes = Vec::new();
    for mode in legs.iter().filter_map(|leg| leg.routing_mode.as_ref()) {
        if !routing_modes.contains(&mode) {
            routing_modes.push(mode);
        }
    }

    // For a single leg without an unambiguous routing mode, use its leg mode.
    let mode = if routing_modes.len() == 1 {
        Some(routing_modes[0].external().to_string())
    } else if legs.len() == 1 {
        Some(legs[0].mode.external().to_string())
    } else {
        None
    };

    if mode.is_none() {
        error!("Could not find routing mode for trip {:?}", trip_elements);
    }

    mode
}

/// Identifies a trip inside a flat plan by its boundary activities.
///
/// We use boundary indices instead of separate `Trip` and `TripMut` views because trips are only a
/// derived structure over the flat plan. Indices make both read-only access and structural edits
/// work through the same abstraction, including replacements that would invalidate borrowed views.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub struct TripSpan {
    origin_index: usize,
    destination_index: usize,
}

impl TripSpan {
    pub fn origin_index(&self) -> usize {
        self.origin_index
    }

    pub fn destination_index(&self) -> usize {
        self.destination_index
    }

    /// Returns the origin activity of this trip.
    pub fn origin<'a>(&self, plan_elements: &'a [InternalPlanElement]) -> &'a InternalActivity {
        activity_at(plan_elements, self.origin_index)
    }

    /// Returns the destination activity of this trip.
    pub fn destination<'a>(
        &self,
        plan_elements: &'a [InternalPlanElement],
    ) -> &'a InternalActivity {
        activity_at(plan_elements, self.destination_index)
    }

    /// Returns the plan elements between origin and destination.
    pub fn trip_elements<'a>(
        &self,
        plan_elements: &'a [InternalPlanElement],
    ) -> &'a [InternalPlanElement] {
        &plan_elements[self.origin_index + 1..self.destination_index]
    }

    /// Returns mutable access to the origin activity.
    pub fn origin_mut<'a>(
        &self,
        plan_elements: &'a mut [InternalPlanElement],
    ) -> &'a mut InternalActivity {
        activity_at_mut(plan_elements, self.origin_index)
    }

    /// Returns mutable access to the destination activity.
    pub fn destination_mut<'a>(
        &self,
        plan_elements: &'a mut [InternalPlanElement],
    ) -> &'a mut InternalActivity {
        activity_at_mut(plan_elements, self.destination_index)
    }

    /// Returns mutable access to the plan elements between origin and destination.
    pub fn trip_elements_mut<'a>(
        &self,
        plan_elements: &'a mut [InternalPlanElement],
    ) -> &'a mut [InternalPlanElement] {
        &mut plan_elements[self.origin_index + 1..self.destination_index]
    }

    /// Replaces the plan elements between origin and destination.
    pub fn replace_trip_elements(
        &self,
        plan_elements: &mut Vec<InternalPlanElement>,
        new_elements: impl IntoIterator<Item = InternalPlanElement>,
    ) -> TripSpan {
        let new_elements: Vec<_> = new_elements.into_iter().collect();
        let new_destination_index = self.origin_index + new_elements.len() + 1;
        let splice_range_begin = self.origin_index + 1; // including
        let splice_range_end = self.destination_index; // excluding
        plan_elements.splice(splice_range_begin..splice_range_end, new_elements);
        TripSpan {
            origin_index: self.origin_index,
            destination_index: new_destination_index,
        }
    }

    /// Returns all legs between origin and destination.
    pub fn legs<'a>(
        &self,
        plan_elements: &'a [InternalPlanElement],
    ) -> impl Iterator<Item = &'a InternalLeg> {
        self.trip_elements(plan_elements)
            .iter()
            .filter_map(InternalPlanElement::as_leg)
    }

    /// Returns all legs mutable between origin and destination.
    pub fn legs_mut<'a>(
        &self,
        plan_elements: &'a mut [InternalPlanElement],
    ) -> impl Iterator<Item = &'a mut InternalLeg> {
        self.trip_elements_mut(plan_elements)
            .iter_mut()
            .filter_map(InternalPlanElement::as_leg_mut)
    }
}

fn activity_at(plan_elements: &[InternalPlanElement], index: usize) -> &InternalActivity {
    plan_elements[index]
        .as_activity()
        .expect("Trip boundary must be an activity")
}

fn activity_at_mut(
    plan_elements: &mut [InternalPlanElement],
    index: usize,
) -> &mut InternalActivity {
    match &mut plan_elements[index] {
        InternalPlanElement::Activity(activity) => activity,
        InternalPlanElement::Leg(_) => panic!("Trip boundary must be an activity"),
    }
}

fn trip_spans<F>(plan_elements: &[InternalPlanElement], is_stage_activity: F) -> Vec<TripSpan>
where
    F: Fn(&InternalActivity) -> bool,
{
    let mut trips = Vec::new();
    let mut origin_activity_index: isize = -1;
    let mut current_index: isize = -1;

    for pe in plan_elements.iter() {
        current_index += 1;
        let act = match pe.as_activity() {
            Some(a) => a,
            None => continue,
        };
        if is_stage_activity(act) {
            continue;
        }

        if origin_activity_index == -1 {
            origin_activity_index = current_index;
            continue;
        }

        if current_index - origin_activity_index > 1 {
            trips.push(TripSpan {
                origin_index: origin_activity_index as usize,
                destination_index: current_index as usize,
            });
        }
        origin_activity_index = current_index;
    }

    trips
}

/// Returns the spans of all trips in the plan.
pub fn get_trip_spans<F>(
    plan_elements: &[InternalPlanElement],
    is_stage_activity: F,
) -> Vec<TripSpan>
where
    F: Fn(&InternalActivity) -> bool,
{
    trip_spans(plan_elements, is_stage_activity)
}

/// Returns the spans of all trips using the default stage-activity rule.
pub fn get_trip_spans_default(plan_elements: &[InternalPlanElement]) -> Vec<TripSpan> {
    get_trip_spans(plan_elements, |a| a.is_interaction())
}

/// Finds the next trip span starting at the given activity index.
pub fn find_trip_span_starting_at_activity<F>(
    plan_elements: &[InternalPlanElement],
    start_index: usize,
    is_stage_activity: F,
) -> Option<TripSpan>
where
    F: Fn(&InternalActivity) -> bool,
{
    trip_spans(&plan_elements[start_index..], &is_stage_activity)
        .into_iter()
        .next()
        .map(|span| TripSpan {
            origin_index: start_index + span.origin_index,
            destination_index: start_index + span.destination_index,
        })
}

/// Finds the next trip span using the default stage-activity rule.
pub fn find_trip_span_starting_at_activity_default(
    plan_elements: &[InternalPlanElement],
    start_index: usize,
) -> Option<TripSpan> {
    find_trip_span_starting_at_activity(plan_elements, start_index, |a| a.is_interaction())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::population::{InternalLeg, InternalPlanElement};
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn test_identify_main_mode_routing_mode() {
        let leg = InternalLeg {
            mode: Id::create("car"),
            routing_mode: Some(Id::create("car")),
            dep_time: None,
            trav_time: None,
            route: None,
            attributes: Default::default(),
        };
        let trip = vec![InternalPlanElement::Leg(leg)];
        let mode = identify_main_mode(&trip);
        assert_eq!(mode, Some("car".to_string()));
    }

    #[deterministic_id_test]
    fn test_identify_main_mode_single_leg_mode() {
        let leg = InternalLeg {
            mode: Id::create("bike"),
            routing_mode: None,
            dep_time: None,
            trav_time: None,
            route: None,
            attributes: Default::default(),
        };
        let trip = vec![InternalPlanElement::Leg(leg)];
        let mode = identify_main_mode(&trip);
        assert_eq!(mode, Some("bike".to_string()));
    }

    #[deterministic_id_test]
    fn test_identify_main_mode_no_mode() {
        let trip = vec![];
        let mode = identify_main_mode(&trip);
        assert_eq!(mode, None);
    }

    #[deterministic_id_test]
    fn identify_main_mode_uses_unique_routing_mode_after_access_leg() {
        let mut access = make_leg("walk");
        access.as_leg_mut().unwrap().routing_mode = None;
        let trip = vec![
            access,
            make_activity("car interaction", "1"),
            make_leg("car"),
        ];

        assert_eq!(Some("car".to_string()), identify_main_mode(&trip));
    }

    #[deterministic_id_test]
    fn identify_main_mode_rejects_conflicting_routing_modes() {
        let trip = vec![make_leg("walk"), make_leg("car")];

        assert_eq!(None, identify_main_mode(&trip));
    }

    fn make_activity(act_type: &str, link: &str) -> InternalPlanElement {
        InternalPlanElement::Activity(InternalActivity {
            act_type: Id::create(act_type),
            link_id: Some(Id::create(link)),
            coord: Some(Coordinate::default()),
            facility_id: None,
            start_time: None,
            end_time: None,
            max_dur: None,
            attributes: Default::default(),
        })
    }

    fn make_leg(mode: &str) -> InternalPlanElement {
        InternalPlanElement::Leg(InternalLeg {
            mode: Id::create(mode),
            routing_mode: Some(Id::create(mode)),
            dep_time: None,
            trav_time: None,
            route: None,
            attributes: Default::default(),
        })
    }

    #[deterministic_id_test]
    fn test_get_trips_basic() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];
        let spans = get_trip_spans_default(&plan);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].origin(&plan).act_type.external(), "home");
        assert_eq!(spans[0].destination(&plan).act_type.external(), "work");
        assert_eq!(spans[0].trip_elements(&plan).len(), 1);
        assert_eq!(spans[1].origin(&plan).act_type.external(), "work");
        assert_eq!(spans[1].destination(&plan).act_type.external(), "shop");
        assert_eq!(spans[1].trip_elements(&plan).len(), 1);
    }

    #[deterministic_id_test]
    fn test_get_trips_with_stage_activity() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("car interaction", "2"),
            make_leg("car"),
            make_activity("work", "3"),
        ];
        let spans = get_trip_spans_default(&plan);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].origin(&plan).act_type.external(), "home");
        assert_eq!(spans[0].destination(&plan).act_type.external(), "work");
        assert_eq!(spans[0].trip_elements(&plan).len(), 3);
    }

    #[deterministic_id_test]
    fn test_get_trips_no_trips() {
        let plan = vec![make_activity("home", "1"), make_activity("work", "2")];
        let spans = get_trip_spans_default(&plan);
        assert!(spans.is_empty());
    }

    #[deterministic_id_test]
    fn test_get_trips_empty() {
        let plan: Vec<InternalPlanElement> = vec![];
        let spans = get_trip_spans_default(&plan);
        assert!(spans.is_empty());
    }

    #[deterministic_id_test]
    fn test_get_trip_spans_default_basic() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];

        let spans = get_trip_spans_default(&plan);
        assert_eq!(spans.len(), 2);
        assert_eq!(
            spans,
            vec![
                TripSpan {
                    origin_index: 0,
                    destination_index: 2,
                },
                TripSpan {
                    origin_index: 2,
                    destination_index: 4,
                },
            ]
        );
        assert_eq!(spans[0].origin(&plan).act_type.external(), "home");
        assert_eq!(spans[1].destination(&plan).act_type.external(), "shop");
    }

    #[deterministic_id_test]
    fn test_trip_span_legs_single_leg() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
        ];

        let span = find_trip_span_starting_at_activity_default(&plan, 0).unwrap();
        let modes: Vec<_> = span
            .legs(&plan)
            .map(|leg| leg.mode.external().to_string())
            .collect();

        assert_eq!(modes, vec!["car"]);
    }

    #[deterministic_id_test]
    fn test_trip_span_legs_with_stage_activity() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("car interaction", "2"),
            make_leg("walk"),
            make_activity("work", "3"),
        ];

        let span = find_trip_span_starting_at_activity_default(&plan, 0).unwrap();
        let modes: Vec<_> = span
            .legs(&plan)
            .map(|leg| leg.mode.external().to_string())
            .collect();

        assert_eq!(modes, vec!["car", "walk"]);
    }

    #[deterministic_id_test]
    fn test_trip_span_legs_stays_within_span() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];

        let spans = get_trip_spans_default(&plan);
        let modes: Vec<_> = spans[0]
            .legs(&plan)
            .map(|leg| leg.mode.external().to_string())
            .collect();

        assert_eq!(modes, vec!["car"]);
    }

    #[deterministic_id_test]
    fn test_trip_span_mutation_helpers() {
        let mut plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];

        let span = find_trip_span_starting_at_activity_default(&plan, 2).unwrap();
        span.origin_mut(&mut plan).act_type = Id::create("office");
        span.trip_elements_mut(&mut plan)[0] = make_leg("bike");
        span.destination_mut(&mut plan).act_type = Id::create("mall");

        assert_eq!(plan[2].as_activity().unwrap().act_type.external(), "office");
        assert_eq!(plan[3].as_leg().unwrap().mode.external(), "bike");
        assert_eq!(plan[4].as_activity().unwrap().act_type.external(), "mall");
    }

    #[deterministic_id_test]
    fn test_trip_span_replace_middle() {
        let mut plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];

        let span = find_trip_span_starting_at_activity_default(&plan, 0).unwrap();
        span.replace_trip_elements(
            &mut plan,
            vec![
                make_leg("pt"),
                make_activity("pt interaction", "99"),
                make_leg("pt"),
            ],
        );

        assert_eq!(plan.len(), 7);
        assert_eq!(plan[0].as_activity().unwrap().act_type.external(), "home");
        assert_eq!(plan[1].as_leg().unwrap().mode.external(), "pt");
        assert_eq!(
            plan[2].as_activity().unwrap().act_type.external(),
            "pt interaction"
        );
        assert_eq!(plan[3].as_leg().unwrap().mode.external(), "pt");
        assert_eq!(plan[4].as_activity().unwrap().act_type.external(), "work");
    }

    #[deterministic_id_test]
    fn test_trip_span_replace_middle_5_elements() {
        let mut plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];

        let span = find_trip_span_starting_at_activity_default(&plan, 2).unwrap();
        span.replace_trip_elements(
            &mut plan,
            vec![
                make_leg("walk"),
                make_activity("pt interaction", "99"),
                make_leg("pt"),
                make_activity("pt interaction", "99"),
                make_leg("walk"),
            ],
        );

        assert_eq!(plan.len(), 9);
        assert_eq!(plan[2].as_activity().unwrap().act_type.external(), "work");
        assert_eq!(plan[3].as_leg().unwrap().mode.external(), "walk");
        assert_eq!(
            plan[4].as_activity().unwrap().act_type.external(),
            "pt interaction"
        );
        assert_eq!(plan[5].as_leg().unwrap().mode.external(), "pt");
        assert_eq!(
            plan[6].as_activity().unwrap().act_type.external(),
            "pt interaction"
        );
        assert_eq!(plan[7].as_leg().unwrap().mode.external(), "walk");
        assert_eq!(plan[8].as_activity().unwrap().act_type.external(), "shop");
    }

    #[deterministic_id_test]
    fn test_find_trip_span_starting_at_activity_default_basic_read_only() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("work", "2"),
            make_leg("walk"),
            make_activity("shop", "3"),
        ];
        let span = find_trip_span_starting_at_activity_default(&plan, 0).unwrap();
        assert_eq!(span.origin(&plan).act_type.external(), "home");
        assert_eq!(span.destination(&plan).act_type.external(), "work");
        assert_eq!(span.trip_elements(&plan).len(), 1);

        let span2 = find_trip_span_starting_at_activity_default(&plan, 2).unwrap();
        assert_eq!(span2.origin(&plan).act_type.external(), "work");
        assert_eq!(span2.destination(&plan).act_type.external(), "shop");
        assert_eq!(span2.trip_elements(&plan).len(), 1);

        assert!(find_trip_span_starting_at_activity_default(&plan, 4).is_none());
    }

    #[deterministic_id_test]
    fn test_find_trip_span_starting_at_activity_default_with_stage() {
        let plan = vec![
            make_activity("home", "1"),
            make_leg("car"),
            make_activity("car interaction", "2"),
            make_leg("car"),
            make_activity("work", "3"),
        ];
        let span = find_trip_span_starting_at_activity_default(&plan, 0).unwrap();
        assert_eq!(span.origin(&plan).act_type.external(), "home");
        assert_eq!(span.destination(&plan).act_type.external(), "work");
        assert_eq!(span.trip_elements(&plan).len(), 3);
        assert!(find_trip_span_starting_at_activity_default(&plan, 2).is_none());
    }

    #[deterministic_id_test]
    fn test_find_trip_span_starting_at_activity_default_empty() {
        let plan: Vec<InternalPlanElement> = vec![];
        assert!(find_trip_span_starting_at_activity_default(&plan, 0).is_none());
    }
}
