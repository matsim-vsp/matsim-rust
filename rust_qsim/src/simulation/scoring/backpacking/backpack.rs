use crate::simulation::events::EventTrait;
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan};
use crate::simulation::scoring::partial_plans::PartialPlan;

/// Backpacks store the Events as well as a partial plan ([BackpackPlan]) for each agent.
/// The Backpack is not managed by the agent itself but by the [BackpackDataCollector], which exists
/// once for each partition. If an agent leaves the current partition, the Backpack is transmitted
/// to the partition the agent is currently entering.
pub(crate) struct Backpack {
    person_id: Id<InternalPerson>,
    events: Vec<Box<dyn EventTrait>>,
    backpack_plan: PartialPlan,
}

#[allow(dead_code)]
pub(crate) struct PersonExperience {
    dummy_person: InternalPerson,
    events: Vec<Box<dyn EventTrait>>,
}

impl Backpack {
    pub fn new(person_id: Id<InternalPerson>) -> Self {
        Self {
            person_id,
            events: Default::default(),
            backpack_plan: PartialPlan::default(),
        }
    }

    fn relevant_event_for_scoring(_: &dyn EventTrait) -> Option<Box<dyn EventTrait>> {
        /*
        Keep scoring-only events in the backpack so they migrate with the affected person.
        Additional event types can be added here without coupling them to partial-plan creation.
        (aleks May'26)
         */
        None
    }

    pub(crate) fn person_id(&self) -> &Id<InternalPerson> {
        &self.person_id
    }

    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(e) = Self::relevant_event_for_scoring(event) {
            self.events.push(e);
            return;
        }

        self.backpack_plan.handle_event(event);
    }

    pub(crate) fn finish(mut self) -> PersonExperience {
        PersonExperience {
            dummy_person: InternalPerson::new(self.person_id, self.backpack_plan.finish()),
            events: std::mem::take(&mut self.events),
        }
    }
}

impl PersonExperience {
    pub(crate) fn plan(&self) -> &InternalPlan {
        &self.dummy_person.selected_plan().unwrap()
    }

    pub(crate) fn plan_mut(&mut self) -> &mut InternalPlan {
        self.dummy_person.selected_plan_mut()
    }

    #[allow(dead_code)]
    pub(crate) fn events(&self) -> &[Box<dyn EventTrait>] {
        &self.events
    }

    pub(crate) fn convert_to_person(mut self, original: &InternalPerson) -> InternalPerson {
        self.dummy_person.copy_metadata_from(original);
        self.plan_mut().attributes = original.selected_plan().unwrap().attributes.clone();
        self.dummy_person
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::simulation::events::{ActivityEndEventBuilder, PersonStuckEventBuilder};
    use crate::simulation::io::xml::population::IOPerson;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::population::InternalPlanElement;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn conversion_preserves_metadata_and_experienced_plan() {
        let original = InternalPerson::from(
            quick_xml::de::from_str::<IOPerson>(
                r#"<person id="freight-driver">
                    <attributes>
                        <attribute name="subpopulation" class="java.lang.String">freight</attribute>
                        <attribute name="company" class="java.lang.String">carrier</attribute>
                    </attributes>
                    <plan selected="yes" score="-1">
                        <attributes>
                            <attribute name="priority" class="java.lang.Integer">7</attribute>
                        </attributes>
                        <activity type="home" link="link" x="0" y="0" end_time="00:00:05" />
                    </plan>
                </person>"#,
            )
            .unwrap(),
        );
        let mut backpack = Backpack::new(original.id().clone());
        backpack.handle_event(
            &ActivityEndEventBuilder::default()
                .time(SimTime::from_secs(10))
                .person(original.id().clone())
                .link(Id::create("link"))
                .coordinate(Coordinate::default())
                .act_type(Id::create("home"))
                .build()
                .unwrap(),
        );
        let mut experience = backpack.finish();
        experience.plan_mut().score = Some(42.5);

        let person = experience.convert_to_person(&original);

        assert_eq!(person.id(), original.id());
        assert_eq!(person.subpopulation().external(), "freight");
        assert_eq!(person.attributes(), original.attributes());
        assert_eq!(person.plans().len(), 1);
        let plan = person.selected_plan().unwrap();
        assert_eq!(
            plan.attributes,
            original.selected_plan().unwrap().attributes
        );
        assert_eq!(plan.score, Some(42.5));
        assert_eq!(plan.elements.len(), 1);
        let activity = plan.elements[0].as_activity().unwrap();
        assert_eq!(activity.act_type.external(), "home");
        assert_eq!(activity.end_time, Some(SimTime::from_secs(10)));
        assert_eq!(original.selected_plan().unwrap().score, Some(-1.0));
        assert_eq!(
            original
                .plan_element_at(0)
                .unwrap()
                .as_activity()
                .unwrap()
                .end_time,
            Some(SimTime::from_secs(5))
        );
    }

    #[deterministic_id_test]
    fn stuck_event_marks_the_reconstructed_plan_as_aborted() {
        let person = Id::create("person");
        let link = Id::create("link");
        let mut backpack = Backpack::new(person.clone());
        backpack.handle_event(
            &ActivityEndEventBuilder::default()
                .time(SimTime::from_secs(10))
                .person(person.clone())
                .link(link.clone())
                .coordinate(Coordinate::default())
                .act_type(Id::create("home"))
                .build()
                .unwrap(),
        );
        backpack.handle_event(
            &PersonStuckEventBuilder::default()
                .time(SimTime::from_secs(20))
                .person(person)
                .link(Some(link))
                .leg_mode(Some(Id::create("car")))
                .reason(Some("test abort".to_string()))
                .build()
                .unwrap(),
        );

        let experienced_plan = backpack.finish();
        let InternalPlanElement::Activity(activity) = &experienced_plan.plan().elements[0] else {
            panic!("Expected reconstructed activity");
        };
        assert_eq!(activity.attributes.get::<bool>("aborted"), Some(true));
    }
}
