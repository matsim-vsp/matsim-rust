use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan};

/// The person of an agent during mobsim. Plan changes apply to the current iteration only, so the
/// agent can read its person but not change it. An agent that replans works on a copy of its plan.
#[derive(Debug, PartialEq, Clone)]
pub(super) struct ReadOnlyPerson(InternalPerson);

impl ReadOnlyPerson {
    pub(super) fn new(person: InternalPerson) -> Self {
        Self(person)
    }

    pub(super) fn id(&self) -> &Id<InternalPerson> {
        self.0.id()
    }

    pub(super) fn plans(&self) -> &[InternalPlan] {
        self.0.plans()
    }

    /// Hands the person back after mobsim.
    pub(super) fn into_inner(self) -> InternalPerson {
        self.0
    }
}
