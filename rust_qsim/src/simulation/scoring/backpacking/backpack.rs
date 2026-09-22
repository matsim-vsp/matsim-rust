use crate::simulation::events::EventTrait;
use crate::simulation::framework_events::QSimId;
use crate::simulation::id::Id;
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::scoring::partial_plans::PartialPlan;

/// Backpacks store the Events as well as a partial plan ([BackpackPlan]) for each agent.
/// The Backpack is not managed by the agent itself but by the [BackpackDataCollector], which exists
/// once for each partition. If an agent leaves the current partition, the Backpack is transmitted
/// to the partition the agent is currently entering.
pub struct Backpack {
    person_id: Id<InternalPerson>,
    events: Vec<Box<dyn EventTrait>>,
    backpack_plan: PartialPlan,
    #[allow(unused)]
    starting_partition: QSimId,
}

impl Backpack {
    pub fn new(person_id: Id<InternalPerson>, starting_partition: QSimId) -> Self {
        Self {
            person_id,
            events: Default::default(),
            backpack_plan: PartialPlan::default(),
            starting_partition,
        }
    }

    #[allow(unused)]
    fn relevant_event_for_scoring(event: &dyn EventTrait) -> Option<Box<dyn EventTrait>> {
        /*
        Currently, this function is not needed, as there are no relevant events for scoring.
        However, I implemented it so that future relevant events can be simply added to the Backpack.
        An example implementation for LinkEnterEvent is given below.
        (aleks May'26)
         */

        // if let Some(e) = event.as_any().downcast_ref::<LinkEnterEvent>() {
        //     return Some(Box::new(e.clone()))
        // }
        None
    }

    pub fn get_starting_partion(&self) -> QSimId {
        self.starting_partition
    }

    pub(crate) fn handle_event(&mut self, event: &dyn EventTrait) {
        if let Some(e) = Self::relevant_event_for_scoring(event) {
            self.events.push(e);
        }

        self.backpack_plan.handle_event(event);
    }

    pub(crate) fn finish(self) -> InternalPerson {
        InternalPerson::new(self.person_id, self.backpack_plan.finish())
    }
}
