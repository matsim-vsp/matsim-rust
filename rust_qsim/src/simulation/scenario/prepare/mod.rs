//! Preparation of the scenario for simulation, split into two steps:
//!
//! - [`prepare_for_sim::prepare_for_sim`] runs once, before the controller shares the scenario. It
//!   connects the facilities to the network and resolves the activity locations.
//! - [`prepare_for_mobsim::prepare_for_mobsim`] runs before every mobsim iteration. It validates
//!   and repairs plans, e.g. by routing trips.

pub mod prepare_for_mobsim;
pub mod prepare_for_sim;

use crate::simulation::scenario::population::{InternalPerson, InternalPlan};
use rayon::prelude::*;
use std::borrow::Cow;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("scenario preparation failed")]
pub struct PrepareError {
    issues: Vec<PrepareIssue>,
}

impl PrepareError {
    fn from_issues(issues: Vec<PrepareIssue>) -> Result<(), Self> {
        if issues.is_empty() {
            Ok(())
        } else {
            Err(Self::new(issues))
        }
    }

    fn new(mut issues: Vec<PrepareIssue>) -> Self {
        issues.sort_by(|a, b| {
            (&a.person_id, a.plan_index, a.trip_index).cmp(&(
                &b.person_id,
                b.plan_index,
                b.trip_index,
            ))
        });
        Self { issues }
    }

    pub fn issues(&self) -> &[PrepareIssue] {
        &self.issues
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrepareIssue {
    pub person_id: String,
    pub plan_index: usize,
    pub trip_index: Option<usize>,
    pub message: String,
}

#[derive(Debug)]
struct PlanPreparationFailure {
    /// `None` if the failure does not belong to a trip, e.g. an unresolvable activity location.
    trip_index: Option<usize>,
    message: String,
}

/// Prepares a single person for simulation by validating and potentially repairing their plans.
/// `prepare_plan` returns `Some` with the prepared plan if the plan changed.
/// This function works in two stages:
/// (1) check if preparation is needed and perform it on a clone of the plan,
/// (2) replace the old plans by the new ones.
///
/// This two-stage approach is necessary to avoid data races when multiple plans of the same person are being prepared in parallel.
fn prepare_person<F>(person: &mut InternalPerson, prepare_plan: F) -> Vec<PrepareIssue>
where
    F: Fn(&InternalPerson, &InternalPlan) -> Result<Option<InternalPlan>, PlanPreparationFailure>
        + Sync,
{
    // Stage 1: check if preparation is needed and perform it on a clone of the plan
    // This stage is parallelized by rayon
    let outcomes: Vec<_> = person
        .plans()
        .par_iter()
        .enumerate()
        .map(|(plan_index, plan)| (plan_index, prepare_plan(person, plan)))
        .collect();

    // Stage 2: replace the old plans by the new ones
    let mut issues = Vec::new();
    let person_id = person.id().external().to_string();
    for (plan_index, outcome) in outcomes {
        match outcome {
            Ok(Some(plan)) => person.plans_mut()[plan_index] = plan,
            Ok(None) => {}
            Err(failure) => {
                issues.push(PrepareIssue {
                    person_id: person_id.clone(),
                    plan_index,
                    trip_index: failure.trip_index,
                    message: failure.message,
                });
            }
        }
    }

    issues
}

/// Returns the plan if it was changed, i.e. cloned, during preparation.
fn owned_plan(working_plan: Cow<'_, InternalPlan>) -> Option<InternalPlan> {
    match working_plan {
        Cow::Borrowed(_) => None,
        Cow::Owned(plan) => Some(plan),
    }
}

/// Test fixtures shared by the tests of both preparation steps.
#[cfg(test)]
mod test_utils {
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use crate::simulation::scenario::network::{Link, Network, Node};
    use crate::simulation::scenario::population::InternalActivity;
    use nohash_hasher::{IntMap, IntSet};

    /// Three horizontal links spanning x=0..100: a car link at y=0, a bike link at y=10 and a
    /// car and bike link at y=20.
    pub(super) fn layered_network() -> Network {
        let mut network = Network::new();
        let links = [
            ("car-0", 0.0, &["car"][..]),
            ("bike-10", 10.0, &["bike"][..]),
            ("car-bike-20", 20.0, &["car", "bike"][..]),
        ];
        for (link_id, y, modes) in links {
            let from = Node::new(
                Id::create(&format!("{link_id}-from")),
                Coordinate::new_2d(0.0, y),
                0,
                1,
            );
            let to = Node::new(
                Id::create(&format!("{link_id}-to")),
                Coordinate::new_2d(100.0, y),
                0,
                1,
            );
            let modes: IntSet<Id<String>> = modes.iter().map(|mode| Id::create(mode)).collect();
            let link = Link::new(
                Id::create(link_id),
                from.id.clone(),
                to.id.clone(),
                100.0,
                1.0,
                1.0,
                1.0,
                modes,
                0,
            );
            network.add_node(from);
            network.add_node(to);
            network.add_link(link);
        }
        network
    }

    pub(super) fn activity_facility(
        id: &str,
        x: f64,
        y: f64,
        base_link: Option<&str>,
    ) -> ActivityFacility {
        ActivityFacility {
            id: Id::create(id),
            coord: Coordinate::new_2d(x, y),
            base_link: base_link.map(Id::get_from_ext),
            mode_to_link: IntMap::default(),
            desc: None,
            activities: Vec::new(),
            attributes: InternalAttributes::default(),
        }
    }

    pub(super) fn facilities(list: Vec<ActivityFacility>) -> ActivityFacilities {
        let mut facilities = ActivityFacilities::default();
        for facility in list {
            facilities.add_facility(facility);
        }
        facilities
    }

    pub(super) fn located_activity(
        link: Option<&str>,
        coord: Option<Coordinate>,
        facility: Option<&str>,
    ) -> InternalActivity {
        InternalActivity {
            act_type: Id::create("act"),
            link_id: link.map(Id::get_from_ext),
            coord,
            facility_id: facility.map(Id::get_from_ext),
            start_time: None,
            end_time: None,
            max_dur: None,
            attributes: InternalAttributes::default(),
        }
    }
}
