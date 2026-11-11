use super::{PlanPreparationFailure, PrepareError, owned_plan, prepare_person};
use crate::simulation::config::{Config, ModalLinkSelection, VehiclesSource};
use crate::simulation::id::Id;
use crate::simulation::pt::schedule::give_each_departure_its_own_vehicle;
use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
use crate::simulation::scenario::network::{Link, Network};
use crate::simulation::scenario::population::{InternalActivity, InternalPlan, VEHICLES};
use crate::simulation::scenario::vehicles::{Garage, InternalVehicleType};
use crate::simulation::scenario::{Coordinate, Scenario};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

/// Prepares the loaded scenario for simulation. This runs once, before the controller shares the
/// scenario, e.g. with the routing modules:
///
/// Person vehicles and routing interaction ids are prepared sequentially first, so that their
/// ids are independent of parallel plan preparation.
///
/// 1. The facilities are connected to the network, see [`prepare_facilities`].
/// 2. Every activity gets a link and a coordinate, see [`resolve_activity_locations`].
/// 3. If the transit simulation has no vehicle blocks, each departure gets a vehicle of its own.
///
/// The first two steps process facilities and persons in parallel.
pub(crate) fn prepare_for_sim(scenario: &mut Scenario) -> Result<(), PrepareError> {
    prepare_interaction_activity_ids(&scenario.config);
    prepare_vehicles(scenario).map_err(PrepareError::for_scenario)?;
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

    if scenario
        .config
        .transit()
        .simulation
        .needs_vehicle_per_departure()
    {
        give_each_departure_its_own_vehicle(
            &mut scenario.transit_schedule,
            &mut scenario.transit_garage,
        );
    }

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

fn prepare_vehicles(scenario: &mut Scenario) -> Result<(), String> {
    let mode_types = vehicle_types_for_network_modes(&scenario.config, &mut scenario.garage)?;

    let garage = &mut scenario.garage;

    let mut persons: Vec<_> = scenario.population.persons.values_mut().collect();
    persons.sort_by(|a, b| a.id().external().cmp(b.id().external()));

    for person in persons {
        let mut assignments = BTreeMap::new();
        for (mode, fallback_type_id) in &mode_types {
            let override_type_id = if let Some(external) = person.vehicle_type_for_mode(mode) {
                let vehicle_type_id = Id::try_get_from_ext(external)
                    .filter(|id| garage.vehicle_types.contains_key(id))
                    .ok_or_else(|| {
                        format!(
                            "Vehicle type {external} specified for person {} and mode {mode} \
                             not found in vehicle types",
                            person.id().external()
                        )
                    })?;
                Some(vehicle_type_id)
            } else {
                None
            };

            let external = format!("{}_{mode}", person.id().external());
            let id = Id::create(&external);
            garage.ensure_vehicle(id, override_type_id.as_ref().unwrap_or(fallback_type_id));
            assignments.insert(mode.clone(), external);
        }
        // Generated assignments replace the same mode's input entry; other entries survive.
        // Commit once per person, including an empty map, only after all its modes succeeded.
        person
            .attributes_mut()
            .merge_string_map_entries(VEHICLES, assignments);
    }
    Ok(())
}

fn network_modes(config: &Config) -> BTreeSet<String> {
    config
        .qsim()
        .main_modes
        .iter()
        .chain(&config.routing().network_modes)
        .cloned()
        .collect()
}

fn prepare_interaction_activity_ids(config: &Config) {
    // Routers may create these activities concurrently, including in fromVehiclesData mode.
    for mode in network_modes(config) {
        Id::<String>::create(&mode);
        Id::<String>::create(&format!("{mode} interaction"));
    }
}

fn vehicle_types_for_network_modes(
    config: &Config,
    garage: &mut Garage,
) -> Result<Vec<(String, Id<InternalVehicleType>)>, String> {
    match config.qsim().vehicles_source {
        VehiclesSource::FromVehiclesData => {
            // Nothing to create if we load vehicles from data. Persons still receive an empty merge.
            Ok(Vec::new())
        }
        VehiclesSource::DefaultVehicle => {
            let modes = network_modes(config);
            if modes.is_empty() {
                return Ok(Vec::new());
            }
            // add default vehicle type to garage if it doesn't exist yet, so that the vehicles can be created
            let default_type = InternalVehicleType::default();
            let default_type_id = default_type.id.clone();
            if !garage.vehicle_types.contains_key(&default_type_id) {
                garage.add_veh_type(default_type);
            }
            Ok(modes
                .into_iter()
                .map(|mode| (mode, default_type_id.clone()))
                .collect())
        }
        VehiclesSource::ModeVehicleTypesFromVehiclesData => {
            // Java requires the base type for every mode even if all persons have supplied vehicles
            // or person-specific overrides. Keep only their ids; the garage resolves vehicle properties.
            network_modes(config).into_iter().map(|mode| {
                let vehicle_type_id = Id::try_get_from_ext(&mode)
                    .filter(|id| garage.vehicle_types.contains_key(id))
                    .ok_or_else(|| format!(
                        "Missing vehicle type {mode} for qsim.vehicles_source=modeVehicleTypesFromVehiclesData"
                    ))?;
                Ok((mode, vehicle_type_id))
            }).collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{prepare_facilities, prepare_for_sim};
    use crate::simulation::config::{Config, ModalLinkSelection, VehiclesSource};
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::Facility;
    use crate::simulation::scenario::facilities::{ActivityFacilities, ActivityFacility};
    use crate::simulation::scenario::network::{Link, Network};
    use crate::simulation::scenario::population::{
        InternalPerson, InternalPlan, Population, VEHICLE_TYPES, VEHICLES,
    };
    use crate::simulation::scenario::prepare::test_utils::{
        activity_facility, facilities, layered_network, located_activity,
    };
    use crate::simulation::scenario::transit::TransitSchedule;
    use crate::simulation::scenario::vehicles::{Garage, InternalVehicle};
    use crate::simulation::scenario::{Coordinate, Scenario};
    use crate::test_utils::create_vehicle_type;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::collections::BTreeMap;
    use std::sync::Arc;

    // Facilities with and without supplied links receive base and modal links according to
    // BaseLinkFirst selection. Modes without a stored modal link fall back to the base link.
    // Preparing the facilities again must leave these assignments unchanged.
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

    // A facility references a link whose id exists in the ID store but not in the network.
    // Preparation must reject that reference rather than assigning a nearby replacement link.
    #[deterministic_id_test]
    #[should_panic(expected = "which is not part of the network")]
    fn prepare_facilities_rejects_unknown_input_link() {
        let network = layered_network();
        Id::<Link>::create("unknown");
        let mut facilities = facilities(vec![activity_facility("f", 0.0, 0.0, Some("unknown"))]);

        prepare_facilities(&mut facilities, &network, ModalLinkSelection::BaseLinkFirst);
    }

    // The loaded scenario contains a facility with coordinates but no base link.
    // Calling prepare_for_sim must connect it to the nearest link, verifying that the scenario
    // entry point invokes facility preparation even when the population is empty.
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

    // Activities specify a facility, coordinates, or a link, including conflicting location data.
    // Preparation fills in each activity's link and coordinates, giving the facility precedence
    // and preserving its reference. A second preparation must leave the resolved plan unchanged.
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

    // Two persons have activities with no location or with a reference to an unknown facility.
    // Preparation reports both problems as plan-level issues without a trip index, since location
    // resolution happens before trip preparation.
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

    // modeVehicleTypesFromVehiclesData: person "b" has no vehicle attributes, person "a" selects
    // a more specific car type and already maps car and truck to vehicles of its own; "a_bike"
    // already exists in the garage. Every person gets `<person>_<mode>` for car and bike. Only the
    // car entry of "a" is replaced, and the existing vehicle keeps its type.
    #[deterministic_id_test]
    fn mode_vehicle_types_assign_one_vehicle_per_person_and_mode() {
        let mut garage = vehicle_types(&["car", "bike", "sportscar"]);
        garage.add_veh(vehicle("a_bike", "sportscar"));
        // "b" is created first, so its internal id is smaller than the one of "a".
        let b = person("b");
        let mut a = person("a");
        a.attributes_mut()
            .insert_string_map_entry(VEHICLE_TYPES, "car", "sportscar");
        a.attributes_mut()
            .insert_string_map_entry(VEHICLES, "car", "own-car");
        a.attributes_mut()
            .insert_string_map_entry(VEHICLES, "truck", "fleet-1");
        let mut scenario = vehicle_scenario(
            VehiclesSource::ModeVehicleTypesFromVehiclesData,
            garage,
            vec![b, a],
        );

        prepare_for_sim(&mut scenario).unwrap();

        // This might seem odd, but this is how ModeVehicleTypesFromVehiclesData works in Java:
        // it creates a vehicle for every person and every mode, even if the person already has a vehicle for that mode.
        // The existing vehicle is kept in the routes, but the new vehicle is created anyway.
        assert_eq!(
            string_map(&[("bike", "a_bike"), ("car", "a_car"), ("truck", "fleet-1")]),
            person_vehicles(&scenario, "a")
        );
        assert_eq!(
            string_map(&[("bike", "b_bike"), ("car", "b_car")]),
            person_vehicles(&scenario, "b")
        );
        assert_eq!(4, scenario.garage.vehicles.len());
        assert_eq!("sportscar", vehicle_type_of(&scenario, "a_car"));
        assert_eq!("sportscar", vehicle_type_of(&scenario, "a_bike"));
        assert_eq!("car", vehicle_type_of(&scenario, "b_car"));
        assert_eq!("bike", vehicle_type_of(&scenario, "b_bike"));
        // Vehicle ids are created in the order of the external person ids, not of the map.
        assert!(
            Id::<InternalVehicle>::get_from_ext("a_car").internal()
                < Id::<InternalVehicle>::get_from_ext("b_car").internal()
        );
    }

    // modeVehicleTypesFromVehiclesData needs a vehicle type for every main and network mode, and a
    // type selected by a person must exist. In both cases preparation fails and the person
    // receives no vehicle assignment.
    #[deterministic_id_test]
    fn mode_vehicle_types_reject_missing_vehicle_types() {
        let source = VehiclesSource::ModeVehicleTypesFromVehiclesData;
        // No type for the network mode bike.
        let mut scenario = vehicle_scenario(source, vehicle_types(&["car"]), vec![person("p")]);
        let error = prepare_for_sim(&mut scenario).unwrap_err();
        assert!(error.to_string().contains("Missing vehicle type bike"));
        assert!(error.issues().is_empty());
        assert!(scenario.garage.vehicles.is_empty());

        // The person selects an unknown car type.
        let mut p = person("p");
        p.attributes_mut()
            .insert_string_map_entry(VEHICLE_TYPES, "car", "unknown");
        let mut scenario = vehicle_scenario(source, vehicle_types(&["car", "bike"]), vec![p]);
        let error = prepare_for_sim(&mut scenario).unwrap_err();
        assert!(error.to_string().contains(
            "Vehicle type unknown specified for person p and mode car not found in vehicle types"
        ));
        let p = &scenario.population.persons[&Id::get_from_ext("p")];
        assert_eq!(
            None,
            p.attributes().get::<BTreeMap<String, String>>(VEHICLES)
        );
    }

    // defaultVehicle: without any vehicle types, MATSim's defaultVehicleType is registered and
    // used for the vehicles of all main and network modes.
    #[deterministic_id_test]
    fn default_vehicle_registers_default_vehicle_type() {
        let mut scenario = vehicle_scenario(
            VehiclesSource::DefaultVehicle,
            Garage::new(),
            vec![person("p")],
        );

        prepare_for_sim(&mut scenario).unwrap();

        assert_eq!(1, scenario.garage.vehicle_types.len());
        assert_eq!(
            string_map(&[("bike", "p_bike"), ("car", "p_car")]),
            person_vehicles(&scenario, "p")
        );
        assert_eq!("defaultVehicleType", vehicle_type_of(&scenario, "p_car"));
        assert_eq!("defaultVehicleType", vehicle_type_of(&scenario, "p_bike"));
    }

    // fromVehiclesData: no vehicles are generated and supplied assignments stay as they are.
    // A person without assignments gets an empty map, and the interaction activity types of the
    // network modes still exist for routing.
    #[deterministic_id_test]
    fn from_vehicles_data_keeps_supplied_vehicles() {
        let mut garage = vehicle_types(&["car"]);
        garage.add_veh(vehicle("fleet-1", "car"));
        let mut a = person("a");
        a.attributes_mut()
            .insert_string_map_entry(VEHICLES, "car", "fleet-1");
        let mut scenario = vehicle_scenario(
            VehiclesSource::FromVehiclesData,
            garage,
            vec![a, person("b")],
        );

        prepare_for_sim(&mut scenario).unwrap();

        assert_eq!(1, scenario.garage.vehicles.len());
        assert_eq!(
            string_map(&[("car", "fleet-1")]),
            person_vehicles(&scenario, "a")
        );
        assert_eq!(BTreeMap::new(), person_vehicles(&scenario, "b"));
        assert!(Id::<String>::try_get_from_ext("car interaction").is_some());
        assert!(Id::<String>::try_get_from_ext("bike interaction").is_some());
    }

    /// A scenario with main mode car, network routing mode bike and the given vehicle source.
    fn vehicle_scenario(
        source: VehiclesSource,
        garage: Garage,
        persons: Vec<InternalPerson>,
    ) -> Scenario {
        let mut config = Config::default();
        config.qsim_mut().main_modes = vec!["car".to_string()];
        config.qsim_mut().vehicles_source = source;
        config.routing_mut().network_modes = vec!["bike".to_string()];
        let persons = persons.into_iter().map(|p| (p.id().clone(), p)).collect();
        let mut scenario = scenario(
            layered_network(),
            ActivityFacilities::default(),
            Population { persons },
        );
        scenario.garage = garage;
        scenario.config = Arc::new(config);
        scenario
    }

    fn person(id: &str) -> InternalPerson {
        InternalPerson::new(Id::create(id), InternalPlan::default())
    }

    fn vehicle_types(ids: &[&str]) -> Garage {
        let mut garage = Garage::new();
        for id in ids {
            garage.add_veh_type(create_vehicle_type(&Id::create(id), Id::create(id)));
        }
        garage
    }

    fn vehicle(id: &str, vehicle_type: &str) -> InternalVehicle {
        InternalVehicle {
            id: Id::create(id),
            max_v: 10.0,
            pce: 1.0,
            vehicle_type: Id::get_from_ext(vehicle_type),
            attributes: Default::default(),
        }
    }

    fn string_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn person_vehicles(scenario: &Scenario, person: &str) -> BTreeMap<String, String> {
        scenario.population.persons[&Id::get_from_ext(person)]
            .attributes()
            .get(VEHICLES)
            .expect("person has a vehicles attribute")
    }

    fn vehicle_type_of(scenario: &Scenario, vehicle: &str) -> String {
        scenario.garage.vehicles[&Id::get_from_ext(vehicle)]
            .vehicle_type
            .external()
            .to_string()
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
            transit_garage: Garage::default(),
            facilities,
            scoring_parameters: Vec::new(),
            config: Arc::new(Config::default()),
        }
    }
}
