use crate::simulation::id::Id;
use crate::simulation::scenario::population::{InternalPerson, InternalPlan};

/// Scores one person's experienced plan after a mobsim iteration.
pub trait PlanScorer: Send + Sync {
    fn score(
        &self,
        person_id: &Id<InternalPerson>,
        subpopulation: &str,
        experienced_plan: &InternalPlan,
    ) -> Result<f64, String>;
}
