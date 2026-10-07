use super::{PlanPreparationFailure, PrepareError, owned_plan, prepare_person};
use crate::simulation::config::ModalLinkSelection;
use crate::simulation::id::Id;
use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalActivity, InternalPlan};
use crate::simulation::scenario::{Coordinate, Scenario};
use rayon::prelude::*;
use std::borrow::Cow;
use thiserror::Error;

/// Prepares the loaded scenario for simulation. This runs once, before the controller shares the
/// scenario, e.g. with the routing modules:
///
/// 1. The facilities are connected to the network, see [`prepare_facilities`].
/// 2. Every activity gets a link and a coordinate, see [`resolve_activity_locations`].
///
/// Both steps process facilities and persons in parallel.
pub(crate) fn prepare_for_sim(scenario: &mut Scenario) -> Result<(), PrepareError> {
    let selection = scenario.config.facilities().modal_link_selection;
    prepare_facilities(&mut scenario.facilities, &scenario.network, selection);

    let network = &scenario.network;
    let facilities = &scenario.facilities;
    let issues: Vec<_> = scenario
        .population
        .persons
        .par_iter_mut()
        .flat_map(|(_, person)| {
            prepare_person(person, |_, plan| {
                let mut working_plan = Cow::Borrowed(plan);
                resolve_activity_locations(network, facilities, &mut working_plan).map_err(
                    |source| PlanPreparationFailure {
                        trip_index: None,
                        message: source.to_string(),
                    },
                )?;
                Ok(owned_plan(working_plan))
            })
        })
        .collect();

    PrepareError::from_issues(issues)
}

pub(crate) fn prepare_facilities(
    facilities: &mut ActivityFacilities,
    network: &Network,
    selection: ModalLinkSelection,
) {
    if facilities.facilities.is_empty() {
        return;
    }

    // Build the spatial index once up front instead of inside the parallel iteration.
    network.spatial_index();
    // Each facility is prepared independently, so the parallel iteration order does not matter.
    facilities
        .facilities
        .par_iter_mut()
        .for_each(|(_, facility)| prepare_facility(facility, network, selection));
}

fn prepare_facility(
    facility: &mut ActivityFacility,
    network: &Network,
    selection: ModalLinkSelection,
) {
    let base_link = match &facility.base_link {
        Some(link_id) => {
            assert!(
                network.links_with_ids().contains_key(link_id),
                "Facility with id {} references link {}, which is not part of the network.",
                facility.id,
                link_id
            );
            link_id.clone()
        }
        None => network
            .nearest_link(&facility.coord, None)
            .unwrap_or_else(|| {
                panic!(
                    "Facility with id {} has no link and the network has no link to assign.",
                    facility.id
                )
            }),
    };

    // The configured selection decides the modal link of each mode, see `Network::modal_link`.
    // Only modal links differing from the base link are stored; `Facility::modal_link` falls back
    // to the base link otherwise.
    facility.mode_to_link = network
        .spatial_index()
        .modes()
        .iter()
        .filter_map(|mode| {
            let modal_link = network.modal_link(&base_link, &facility.coord, mode, selection);
            (modal_link != base_link).then(|| (mode.clone(), modal_link))
        })
        .collect();
    facility.base_link = Some(base_link);
}

#[derive(Debug, Error)]
pub(super) enum ActivityLocationError {
    #[error("Activity at plan element {element_index} references unknown facility {facility}")]
    UnknownFacility {
        element_index: usize,
        facility: String,
    },
    #[error(
        "Activity at plan element {element_index} has neither a facility, a link nor a coordinate"
    )]
    MissingLocation { element_index: usize },
    #[error(
        "Activity at plan element {element_index} has no link, and the network has no link to assign"
    )]
    NoLinkForCoordinate { element_index: usize },
}

pub(super) fn resolve_activity_locations(
    network: &Network,
    facilities: &ActivityFacilities,
    working_plan: &mut Cow<'_, InternalPlan>,
) -> Result<(), ActivityLocationError> {
    for element_index in 0..working_plan.elements.len() {
        let Some(activity) = working_plan.elements[element_index].as_activity() else {
            continue;
        };
        let Some((link_id, coord)) =
            resolve_activity_location(network, facilities, element_index, activity)?
        else {
            continue;
        };

        let activity = working_plan.to_mut().elements[element_index]
            .as_activity_mut()
            .expect("element is an activity");
        activity.link_id = Some(link_id);
        activity.coord = Some(coord);
    }
    Ok(())
}

/// Returns the resolved link and coordinate of the activity, or `None` if they are unchanged.
fn resolve_activity_location(
    network: &Network,
    facilities: &ActivityFacilities,
    element_index: usize,
    activity: &InternalActivity,
) -> Result<Option<(Id<Link>, Coordinate)>, ActivityLocationError> {
    let (link_id, coord) = if let Some(facility_id) = &activity.facility_id {
        let facility =
            facilities
                .get(facility_id)
                .ok_or_else(|| ActivityLocationError::UnknownFacility {
                    element_index,
                    facility: facility_id.external().to_string(),
                })?;
        (facility.base_link().clone(), facility.coord.clone())
    } else {
        match (&activity.link_id, &activity.coord) {
            (Some(_), Some(_)) => return Ok(None),
            (Some(link_id), None) => {
                let link = network.get_link(link_id);
                let from = network.get_node(&link.from);
                let to = network.get_node(&link.to);
                (link_id.clone(), Coordinate::middle(&from.coord, &to.coord))
            }
            (None, Some(coord)) => {
                let link_id = network
                    .nearest_link(coord, None)
                    .ok_or(ActivityLocationError::NoLinkForCoordinate { element_index })?;
                (link_id, coord.clone())
            }
            (None, None) => {
                return Err(ActivityLocationError::MissingLocation { element_index });
            }
        }
    };

    let unchanged =
        activity.link_id.as_ref() == Some(&link_id) && activity.coord.as_ref() == Some(&coord);
    Ok((!unchanged).then_some((link_id, coord)))
}

#[cfg(test)]
mod tests {
    use super::{prepare_facilities, prepare_for_sim};
    use crate::simulation::config::{Config, ModalLinkSelection};
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::Facility;
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
    use crate::simulation::scenario::prepare::test_utils::{
        activity_facility, facilities, layered_network, located_activity,
    };
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::{Coordinate, Scenario};
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::sync::Arc;

    // Before: facilities with and without input links; after: base links and modal links are set.
    #[deterministic_id_test]
    fn prepare_facilities_assigns_base_and_modal_links() {
        let network = layered_network();
        let mut facilities = facilities(vec![
            // The input link is kept as base link, although other links are closer. It is the
            // car link, but bike uses the nearest bike link.
            activity_facility("input-link", 50.0, 19.0, Some("car-0")),
            // Without an input link, the nearest link of any mode becomes the base link.
            activity_facility("no-link", 50.0, 11.0, None),
            // The base link is the modal link of every mode it allows, even if others are closer.
            activity_facility("base-allows-all", 50.0, 11.0, Some("car-bike-20")),
        ]);
        let walk = Id::<String>::create("walk");

        prepare_facilities(&mut facilities, &network, ModalLinkSelection::BaseLinkFirst);

        let links = |id: &str| {
            let facility =
                Facility::ActivityFacility(facilities.get(&Id::get_from_ext(id)).unwrap());
            [
                facility.base_link().external().to_string(),
                facility
                    .modal_link(&Id::get_from_ext("car"))
                    .external()
                    .to_string(),
                facility
                    .modal_link(&Id::get_from_ext("bike"))
                    .external()
                    .to_string(),
                facility.modal_link(&walk).external().to_string(),
            ]
        };
        assert_eq!(
            ["car-0", "car-0", "car-bike-20", "car-0"],
            links("input-link")
        );
        assert_eq!(
            ["bike-10", "car-bike-20", "bike-10", "bike-10"],
            links("no-link")
        );
        assert_eq!(
            ["car-bike-20", "car-bike-20", "car-bike-20", "car-bike-20"],
            links("base-allows-all")
        );
        // Only modes that the base link does not allow are stored.
        let no_link = facilities.get(&Id::get_from_ext("no-link")).unwrap();
        assert_eq!(1, no_link.mode_to_link.len());
        let base_allows_all = facilities
            .get(&Id::get_from_ext("base-allows-all"))
            .unwrap();
        assert!(base_allows_all.mode_to_link.is_empty());

        let prepared = facilities.clone();
        prepare_facilities(&mut facilities, &network, ModalLinkSelection::BaseLinkFirst);
        assert_eq!(prepared, facilities);
    }

    #[deterministic_id_test]
    #[should_panic(expected = "which is not part of the network")]
    fn prepare_facilities_rejects_unknown_input_link() {
        let network = layered_network();
        Id::<Link>::create("unknown");
        let mut facilities = facilities(vec![activity_facility("f", 0.0, 0.0, Some("unknown"))]);

        prepare_facilities(&mut facilities, &network, ModalLinkSelection::BaseLinkFirst);
    }

    // Before: a facility without link in the loaded scenario; after: the facility is prepared.
    #[deterministic_id_test]
    fn prepare_for_sim_prepares_scenario_facilities() {
        let mut scenario = scenario(
            layered_network(),
            facilities(vec![activity_facility("f", 50.0, 11.0, None)]),
            Population::new(),
        );

        prepare_for_sim(&mut scenario).unwrap();

        let facility = scenario.facilities.get(&Id::get_from_ext("f")).unwrap();
        assert_eq!(Some(Id::get_from_ext("bike-10")), facility.base_link);
    }

    // Before: activities with partial locations; after: every activity has a link and a coordinate.
    #[deterministic_id_test]
    fn prepare_for_sim_resolves_all_location_variants() {
        let network = layered_network();
        let facilities = facilities(vec![activity_facility("f", 50.0, 19.0, Some("car-0"))]);
        let mut plan = InternalPlan::default();
        // Only a facility.
        plan.add_act(located_activity(None, None, Some("f")));
        // The facility takes precedence over the activity's own link and coordinate.
        plan.add_act(located_activity(
            Some("bike-10"),
            Some(Coordinate::new_2d(1.0, 1.0)),
            Some("f"),
        ));
        // Only a coordinate: the nearest link of any mode.
        plan.add_act(located_activity(
            None,
            Some(Coordinate::new_2d(50.0, 9.0)),
            None,
        ));
        // Only a link: the link's midpoint.
        plan.add_act(located_activity(Some("bike-10"), None, None));
        let person_id = Id::create("person-1");
        let mut persons = IntMap::default();
        persons.insert(
            person_id.clone(),
            InternalPerson::new(person_id.clone(), plan),
        );
        let mut scenario = scenario(network, facilities, Population { persons });

        prepare_for_sim(&mut scenario).unwrap();

        let plan = scenario.population.persons[&person_id]
            .selected_plan()
            .unwrap()
            .clone();
        let locations: Vec<_> = plan
            .acts()
            .iter()
            .map(|act| (act.link_id().external().to_string(), act.coord().clone()))
            .collect();
        assert_eq!(
            vec![
                ("car-0".to_string(), Coordinate::new_2d(50.0, 19.0)),
                ("car-0".to_string(), Coordinate::new_2d(50.0, 19.0)),
                ("bike-10".to_string(), Coordinate::new_2d(50.0, 9.0)),
                ("bike-10".to_string(), Coordinate::new_2d(50.0, 10.0)),
            ],
            locations
        );
        assert_eq!(Some(Id::get_from_ext("f")), plan.acts()[0].facility_id);

        // Resolved plans are left untouched.
        prepare_for_sim(&mut scenario).unwrap();
        assert_eq!(
            &plan,
            scenario.population.persons[&person_id]
                .selected_plan()
                .unwrap()
        );
    }

    // Before: activities without any location or with an unknown facility; after: plan-level issues.
    #[deterministic_id_test]
    fn unresolvable_activity_locations_are_reported_without_trip_index() {
        Id::<ActivityFacility>::create("missing");
        let mut persons = IntMap::default();
        for (person, activity) in [
            ("person-1", located_activity(None, None, None)),
            ("person-2", located_activity(None, None, Some("missing"))),
        ] {
            let mut plan = InternalPlan::default();
            plan.add_act(activity);
            let person_id = Id::create(person);
            persons.insert(person_id.clone(), InternalPerson::new(person_id, plan));
        }
        let mut scenario = scenario(
            layered_network(),
            ActivityFacilities::default(),
            Population { persons },
        );

        let error = prepare_for_sim(&mut scenario).unwrap_err();

        let issues = error.issues();
        assert_eq!(2, issues.len());
        assert!(issues.iter().all(|issue| issue.trip_index.is_none()));
        assert!(
            issues[0]
                .message
                .contains("neither a facility, a link nor a coordinate")
        );
        assert!(issues[1].message.contains("unknown facility missing"));
    }

    fn scenario(
        network: Network,
        facilities: ActivityFacilities,
        population: Population,
    ) -> Scenario {
        Scenario {
            network,
            garage: Garage::default(),
            population,
            transit_schedule: TransitSchedule::default(),
            facilities,
            config: Arc::new(Config::default()),
            signals: Default::default(),
        }
    }
}
