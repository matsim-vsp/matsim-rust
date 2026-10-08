use nohash_hasher::IntMap;
use rayon::prelude::*;
use serde::{Deserialize, Deserializer, Serialize};
use std::path::Path;
use std::time::Instant;
use tracing::{info, warn};

use crate::simulation::InternalAttributes;
use crate::simulation::id;
use crate::simulation::id::Id;
use crate::simulation::io::batch::{ReadAhead, read_in_batches};
use crate::simulation::io::xml;

use crate::simulation::io::xml::attributes::IOAttributes;
use crate::simulation::io::xml::element_splitter::ElementSplitter;
use crate::simulation::scenario::population::{
    InternalActivity, InternalLeg, InternalPerson, InternalPlan, InternalPlanElement,
    InternalRoute, Population, SUBPOPULATION, for_each_id_of_io_person,
};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::time::SimTime;

/// Number of persons whose missing ids are collected in parallel before they are created.
const ID_CREATION_CHUNK: usize = 4096;

/// Loads a population from XML.
///
/// Parsing and converting persons runs in parallel. All ids are created sequentially beforehand,
/// in the order in which converting the persons sorted by id one after another would create them.
/// Thus, the internal ids don't depend on the number of threads.
pub(crate) fn load_from_xml(
    path: impl AsRef<Path>,
    garage: &mut Garage,
) -> IntMap<Id<InternalPerson>, InternalPerson> {
    let start = Instant::now();
    let mut io_pop = IOPopulation::from_file_parallel(path);
    let parsed = Instant::now();

    info!("Sorting population by id.");
    io_pop.persons.par_sort_by(|a, b| a.id.cmp(&b.id));
    let sorted = Instant::now();

    create_ids(&io_pop, garage);
    create_remaining_ids(&io_pop);
    let created_ids = Instant::now();

    let population = create_population(io_pop);
    info!(
        "Finished loading population with {} persons in {:.2?} (parsing: {:.2?}, sorting: {:.2?}, creating ids: {:.2?}, converting: {:.2?}).",
        population.len(),
        start.elapsed(),
        parsed - start,
        sorted - parsed,
        created_ids - sorted,
        created_ids.elapsed()
    );
    population
}

pub(crate) fn write_to_xml(population: &Population, path: impl AsRef<Path>) {
    let io_population = IOPopulation::from(population);

    io_population.to_file(path);
}

fn create_ids(io_pop: &IOPopulation, garage: &mut Garage) {
    info!("Creating person ids.");
    // create person ids and collect strings for vehicle ids
    let raw_veh: Vec<_> = io_pop
        .persons
        .iter()
        .map(|p| Id::<InternalPerson>::create(p.id.as_str()))
        .flat_map(|p_id| {
            garage
                .vehicle_types
                .keys()
                .map(move |type_id| (p_id.clone(), type_id.clone()))
        })
        .collect();

    info!("Creating interaction activity types");
    // add interaction activity type for each vehicle type
    for (_, id) in raw_veh.iter() {
        Id::<String>::create(&format!("{} interaction", id.external()));
    }

    info!("Creating vehicle ids");
    for (person_id, type_id) in raw_veh {
        garage.add_veh_by_type(&person_id, &type_id);
    }

    info!("Creating activity types");
    // now iterate over all plans to extract activity ids
    io_pop
        .persons
        .iter()
        .flat_map(|person| person.plans.iter())
        .flat_map(|plan| plan.elements.iter())
        .filter_map(|element| match element {
            IOPlanElement::Activity(a) => Some(a),
            IOPlanElement::Leg(_) => None,
        })
        .map(|act| &act.r#type)
        .for_each(|act_type| {
            Id::<String>::create(act_type.as_str());
        });
}

/// Creates all ids that converting the persons creates and which `create_ids` hasn't created.
fn create_remaining_ids(io_pop: &IOPopulation) {
    info!("Creating remaining ids");
    for chunk in io_pop.persons.chunks(ID_CREATION_CHUNK) {
        // The store is not modified while the missing ids of a chunk are collected. Creating them
        // afterwards in the order of the persons and their elements therefore assigns the same
        // internal ids as converting the persons one after another.
        let missing: Vec<_> = chunk
            .par_iter()
            .map(|io_person| {
                let mut missing = Vec::new();
                for_each_id_of_io_person(io_person, |id| {
                    if !id.exists() {
                        missing.push(id);
                    }
                });
                missing
            })
            .collect();
        for id in missing.iter().flatten() {
            id.create();
        }
    }
}

fn create_population(io_pop: IOPopulation) -> IntMap<Id<InternalPerson>, InternalPerson> {
    let num_ids = id::count_ids();
    let persons: Vec<_> = io_pop
        .persons
        .into_par_iter()
        .map(InternalPerson::from)
        .collect();
    // Ids created during the parallel conversion would get internal ids in a random order.
    assert_eq!(
        num_ids,
        id::count_ids(),
        "Converting persons created ids. All ids must be created beforehand, see for_each_id_of_io_person."
    );

    let mut result = IntMap::default();
    for person in persons {
        result.insert(person.id().clone(), person);
    }
    result
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct IOPTRouteDescription {
    pub transit_route_id: String,
    pub boarding_time: String,
    pub transit_line_id: String,
    pub access_facility_id: String,
    pub egress_facility_id: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IORoute {
    #[serde(rename = "@type")]
    pub r#type: Option<String>,
    #[serde(rename = "@start_link")]
    pub start_link: Option<String>,
    #[serde(rename = "@end_link")]
    pub end_link: Option<String>,
    #[serde(rename = "@trav_time", skip_serializing_if = "Option::is_none")]
    pub trav_time: Option<String>,
    #[serde(rename = "@distance", skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(
        rename = "@vehicleRefId",
        default,
        deserialize_with = "option_string_preserve_null"
    )]
    pub vehicle: Option<String>,

    // this needs to be parsed later
    #[serde(rename = "$value")]
    pub route: Option<String>,
}

impl From<&InternalRoute> for IORoute {
    fn from(route: &InternalRoute) -> Self {
        let generic_internal_route = route.as_generic();

        let r_type = match &route {
            InternalRoute::Generic(_) => "generic",
            InternalRoute::Network(_) => "links",
            InternalRoute::Pt(_) => "default_pt",
        };

        IORoute {
            r#type: Some(r_type.to_string()),
            start_link: Some(generic_internal_route.start_link().external().to_string()),
            end_link: Some(generic_internal_route.end_link().external().to_string()),
            trav_time: generic_internal_route
                .trav_time()
                .map(|t| SimTime::from_duration(t).format_hh_mm_ss_trimmed()),
            distance: generic_internal_route.distance(),
            vehicle: generic_internal_route
                .vehicle()
                .clone()
                .map(|v| v.external().to_string()),
            route: route.clone().get_route_description(),
        }
    }
}

fn option_string_preserve_null<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    let opt = Option::<String>::deserialize(deserializer)?;
    match opt {
        Some(ref s) if s == "null" => Ok(Some("null".to_string())),
        other => Ok(other),
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IOActivity {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(rename = "@type")]
    pub r#type: String,
    #[serde(rename = "@link", skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    #[serde(rename = "@facility", skip_serializing_if = "Option::is_none")]
    pub facility: Option<String>,
    #[serde(rename = "@x")]
    pub x: Option<f64>,
    #[serde(rename = "@y")]
    pub y: Option<f64>,
    #[serde(rename = "@start_time", skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(rename = "@end_time", skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(rename = "@max_dur", skip_serializing_if = "Option::is_none")]
    pub max_dur: Option<String>,
}

impl IOActivity {
    pub fn is_interaction(&self) -> bool {
        self.r#type.contains("interaction")
    }
}

impl From<&InternalActivity> for IOActivity {
    fn from(activity: &InternalActivity) -> Self {
        IOActivity {
            r#type: activity.act_type.external().to_string(),
            link: activity
                .link_id
                .as_ref()
                .map(|id| id.external().to_string()),
            facility: activity
                .facility_id
                .as_ref()
                .map(|id| id.external().to_string()),
            x: activity.coord.as_ref().map(|c| c.x),
            y: activity.coord.as_ref().map(|c| c.y),
            start_time: activity.start_time.map(|t| t.format_hh_mm_ss_trimmed()),
            end_time: activity.end_time.map(|t| t.format_hh_mm_ss_trimmed()),
            max_dur: activity
                .max_dur
                .map(|d| SimTime::from_duration(d).format_hh_mm_ss_trimmed()),
            attributes: IOAttributes::from_internal_none_if_empty(&activity.attributes),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IOLeg {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(rename = "@mode")]
    pub mode: String,
    #[serde(rename = "@dep_time")]
    pub dep_time: Option<String>,
    #[serde(rename = "@trav_time", skip_serializing_if = "Option::is_none")]
    pub trav_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<IORoute>,
}

impl From<&InternalLeg> for IOLeg {
    fn from(leg: &InternalLeg) -> Self {
        // get internal attributes from leg, possibly with added routing mode if currently missing
        let verified_internal_attrs = verify_internal_attrs(&leg);

        IOLeg {
            mode: leg.mode.external().to_string(),
            dep_time: leg.dep_time.map(|t| t.format_hh_mm_ss_trimmed()),
            trav_time: leg
                .trav_time
                .map(|t| SimTime::from_duration(t).format_hh_mm_ss_trimmed()),
            route: leg.route.clone().map(|r| IORoute::from(&r)),
            attributes: IOAttributes::from_internal_none_if_empty(&verified_internal_attrs),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
#[serde(rename_all = "lowercase")]
pub enum IOPlanElement {
    // the current matsim implementation has more logic with facility-id, link-id and coord.
    // Like in MATSim, an activity may specify any combination of facility-id, link-id and coord.
    // Missing link-ids and coords are derived in prepare_for_sim, where the facility takes
    // precedence over the activity's own link and coord.
    Activity(IOActivity),
    Leg(IOLeg),
}

impl IOPlanElement {
    pub fn get_activity(element: Option<&IOPlanElement>) -> Option<&IOActivity> {
        element.and_then(|e| {
            if let IOPlanElement::Activity(activity) = e {
                Some(activity)
            } else {
                None
            }
        })
    }

    pub fn get_leg(element: Option<&IOPlanElement>) -> Option<&IOLeg> {
        element.and_then(|e| {
            if let IOPlanElement::Leg(leg) = e {
                Some(leg)
            } else {
                None
            }
        })
    }
}

impl From<&InternalPlanElement> for IOPlanElement {
    fn from(element: &InternalPlanElement) -> Self {
        match element {
            InternalPlanElement::Activity(activity) => {
                IOPlanElement::Activity(IOActivity::from(activity))
            }
            InternalPlanElement::Leg(leg) => IOPlanElement::Leg(IOLeg::from(leg)),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IOPlan {
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(
        rename = "@selected",
        deserialize_with = "bool_from_yes_no",
        serialize_with = "bool_to_yes_no"
    )]
    pub selected: bool,
    #[serde(rename = "@score", skip_serializing_if = "Option::is_none")]
    pub score: Option<f64>,
    // https://users.rust-lang.org/t/serde-deserializing-a-vector-of-enums/51647/2
    #[serde(rename = "$value")]
    pub elements: Vec<IOPlanElement>,
}

impl From<&InternalPlan> for IOPlan {
    fn from(internal_plan: &InternalPlan) -> Self {
        let mut io_plan_elements = Vec::new();
        let selected = internal_plan.selected;

        // for current internal plan, go through all internal plan elements and convert to IOPlanElements
        for internal_plan_element in &internal_plan.elements {
            io_plan_elements.push(IOPlanElement::from(internal_plan_element));
        }

        IOPlan {
            attributes: IOAttributes::from_internal_none_if_empty(&internal_plan.attributes),
            selected,
            score: internal_plan.score,
            elements: io_plan_elements,
        }
    }
}
fn bool_from_yes_no<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    match s.to_lowercase().as_str() {
        "yes" => Ok(true),
        "no" => Ok(false),
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(serde::de::Error::custom(format!("invalid value: {}", s))),
    }
}

fn bool_to_yes_no<S>(value: &bool, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = if *value { "yes" } else { "no" };
    serializer.serialize_str(s)
}

/// when creating internal legs from io, we store the (optional) routing mode attribute separately
/// in the field leg.routing_mode. In principle, the routing mode is still also contained in the
/// attributes of the (internal) leg.
/// This function verifies that this is (still) the case:
///     - If routing mode field and "routing mode" entry in leg.attributes match (or are both
///         empty/not existing), return leg.attributes without modification
///     - If both exist but they don't match in value, panic
///     - If routing mode field is not None, but no "routing mode" entry is present in
///         leg.attributes, add the former to a copy of leg.attributes and return it
///     - If routing mode field is None, but "routing mode" entry is present in leg.attributes, panic
///
/// To be used when creating IOLegs from internal legs, as IOLegs store routing mode only in the
/// attributes.
fn verify_internal_attrs(leg: &InternalLeg) -> InternalAttributes {
    match (
        &leg.routing_mode,
        &leg.attributes.get::<String>("routingMode"),
    ) {
        // routing mode is not present in leg nor in attributes, return attributes without modification
        (None, None) => leg.attributes.clone(),

        // both routing mode field and entry in attributes exist, verify that they match
        (Some(field_routing_mode), Some(attr_routing_mode)) => {
            if field_routing_mode.external() == attr_routing_mode {
                // routing mode in leg and attributes match, return attributes without modification
                leg.attributes.clone()
            } else {
                // routing mode in leg and attributes don't match, this should not happen, panic
                warn!(
                    "Routing mode in leg and attributes don't match. Routing mode in leg: {:?}, \
                    routing mode in attributes: {:?}",
                    field_routing_mode.external().to_string(),
                    attr_routing_mode
                );
                leg.attributes.clone()
            }
        }

        // routing mode field exists but no entry in attributes
        (Some(routing_mode), None) => {
            // add routing mode to a copy of the attributes and return it
            let mut attrs = leg.attributes.clone();
            attrs.insert("routingMode", routing_mode.external().to_string());
            attrs
        }

        // routing mode is not present in leg but present in attributes, this should not happen, panic
        (None, Some(_)) => {
            warn!("Routing mode is not present in leg but present in attributes.");
            leg.attributes.clone()
        }
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct IOPerson {
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "plan")]
    pub plans: Vec<IOPlan>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename = "population")]
pub struct IOPopulation {
    #[serde(rename = "person", default)]
    pub persons: Vec<IOPerson>,
}

impl IOPopulation {
    pub fn from_file(file_path: impl AsRef<Path>) -> IOPopulation {
        info!(
            "IOPopulation: Reading population from file {}",
            file_path.as_ref().display()
        );
        let population: IOPopulation = xml::read_from_file(file_path);
        info!(
            "IOPopulation: Finished reading population. Population contains {} persons",
            population.persons.len()
        );
        population
    }

    /// Like [`IOPopulation::from_file`], but parses the persons in parallel. Separate threads
    /// decompress the file and split it into persons.
    pub(crate) fn from_file_parallel(file_path: impl AsRef<Path>) -> IOPopulation {
        info!(
            "IOPopulation: Reading population from file {} in parallel",
            file_path.as_ref().display()
        );
        let path = file_path.as_ref().to_path_buf();
        let mut persons = Vec::new();
        read_in_batches(
            // Decompressing and splitting the input run on separate threads.
            || ElementSplitter::new(ReadAhead::spawn(|| xml::open_xml_reader(path)), "person"),
            |splitter, buffer| splitter.next_element_into(buffer),
            |batch| {
                let first = persons.len();
                let parsed: Vec<IOPerson> = batch
                    .par_records()
                    .enumerate()
                    .map(|(i, bytes)| parse_person(bytes, first + i))
                    .collect();
                persons.extend(parsed);
            },
        );
        info!(
            "IOPopulation: Finished reading population. Population contains {} persons",
            persons.len()
        );
        IOPopulation { persons }
    }

    pub fn to_file(&self, file_path: impl AsRef<Path>) {
        xml::write_to_file(
            self,
            file_path,
            "<!DOCTYPE population SYSTEM \"https://www.matsim.org/files/dtd/population_v6.dtd\">",
        );
    }
}

impl From<&Population> for IOPopulation {
    fn from(internal_population: &Population) -> Self {
        let mut io_persons = Vec::new();

        // go through all persons in internal population
        for (ipers_id, internal_person) in &internal_population.persons {
            let mut io_plans = Vec::new();

            // for current internal person, go through all internal plans
            for internal_plan in internal_person.plans() {
                // convert to io_plan and add to the plans of the current person
                io_plans.push(IOPlan::from(internal_plan));
            }

            let io_person = IOPerson {
                id: ipers_id.to_string(),
                plans: io_plans,
                attributes: io_person_attributes(internal_person),
            };

            io_persons.push(io_person);
        }

        IOPopulation {
            persons: io_persons,
        }
    }
}

/// Parses the XML of a single person. `index` is the position of the person in the file.
fn parse_person(bytes: &[u8], index: usize) -> IOPerson {
    let xml = std::str::from_utf8(bytes)
        .unwrap_or_else(|e| panic!("Person number {index} in the file is not valid UTF-8: {e}"));
    let mut de = quick_xml::de::Deserializer::from_str(xml);
    serde_path_to_error::deserialize(&mut de).unwrap_or_else(|err| {
        panic!("Failed to deserialize person number {index} in the file:\n{err:#?}")
    })
}

fn io_person_attributes(internal_person: &InternalPerson) -> Option<IOAttributes> {
    let mut attributes = internal_person.attributes().clone();
    attributes.insert(
        SUBPOPULATION,
        internal_person.subpopulation().external().to_string(),
    );
    IOAttributes::from_internal_none_if_empty(&attributes)
}

#[cfg(test)]
mod tests {
    use std::fs::create_dir_all;
    use std::path::PathBuf;

    use crate::simulation::config::{MetisOptions, PartitionMethod};
    use crate::simulation::id;
    use crate::simulation::id::Id;
    use crate::simulation::id::serializable_type::StableTypeId;
    use crate::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
    use crate::simulation::io::xml::population::{
        IOActivity, IOLeg, IOPerson, IOPlan, IOPlanElement, IOPopulation, create_ids,
        load_from_xml, write_to_xml,
    };
    use crate::simulation::logging::init_std_out_logging_thread_local;
    use crate::simulation::scenario::facilities::ActivityFacility;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::{InternalPerson, InternalPlan, Population};
    use crate::simulation::scenario::vehicles::Garage;
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use quick_xml::de::from_str;
    use quick_xml::se::to_string;
    use std::collections::BTreeMap;
    use std::path::Path;

    /**
    This tests against the first person from the equil mod. Probably this doesn't cover all
    possibilities and needs to improved later.
     */
    #[test]
    fn read_population_from_string() {
        let xml = "<?xml version=\"1.0\" encoding=\"utf-8\"?>
<!DOCTYPE population SYSTEM \"http://www.matsim.org/files/dtd/population_v6.dtd\">

    <population>
        <attributes>
            <attribute name=\"coordinateReferenceSystem\" class=\"java.lang.String\">Atlantis</attribute>
        </attributes>

        <person id=\"1\">
            <attributes>
                <attribute name=\"vehicles\" class=\"org.matsim.vehicles.PersonVehicles\">{\"car\":\"1\"}</attribute>
            </attributes>
            <plan selected=\"yes\">
                <activity type=\"h\" link=\"1\" x=\"-25000.0\" y=\"0.0\" end_time=\"06:00:00\" >
                </activity>
                <leg mode=\"car\">
                    <attributes>
                        <attribute name=\"routingMode\" class=\"java.lang.String\">car</attribute>
                    </attributes>
                    <route type=\"links\" start_link=\"1\" end_link=\"20\" trav_time=\"undefined\" distance=\"25000.0\" vehicleRefId=\"null\">1 6 15 20</route>
                </leg>
                <activity type=\"w\" link=\"20\" x=\"10000.0\" y=\"0.0\" max_dur=\"00:10:00\" >
                </activity>
                <leg mode=\"car\">
                    <attributes>
                        <attribute name=\"routingMode\" class=\"java.lang.String\">car</attribute>
                    </attributes>
                    <route type=\"links\" start_link=\"20\" end_link=\"20\" trav_time=\"undefined\" distance=\"0.0\" vehicleRefId=\"null\">20</route>
                </leg>
                <activity type=\"w\" link=\"20\" x=\"10000.0\" y=\"0.0\" max_dur=\"03:30:00\" >
                </activity>
                <leg mode=\"car\">
                    <attributes>
                        <attribute name=\"routingMode\" class=\"java.lang.String\">car</attribute>
                    </attributes>
                    <route type=\"links\" start_link=\"20\" end_link=\"1\" trav_time=\"undefined\" distance=\"65000.0\" vehicleRefId=\"null\">20 21 22 23 1</route>
                </leg>
                <activity type=\"h\" link=\"1\" x=\"-25000.0\" y=\"0.0\" >
                </activity>
            </plan>
        </person>

    </population>";

        let population: IOPopulation = from_str(xml).unwrap();

        //test overall structure of population
        assert_eq!(1, population.persons.len());

        let person = population.persons.first().unwrap();
        assert_eq!("1", person.id);
        assert_eq!(1, person.plans.len());

        let plan = person.plans.first().unwrap();
        assert!(plan.selected);
        assert_eq!(None, plan.score);
        assert_eq!(7, plan.elements.len());

        // probe for first leg and second activity
        let leg1 = plan.elements.get(1).unwrap();
        match leg1 {
            IOPlanElement::Activity { .. } => {
                panic!("Plan Element at index 1 was expected to be a leg, but was Activity")
            }
            IOPlanElement::Leg(leg) => {
                // <leg mode=\"car\">
                //     <route type=\"links\" start_link=\"1\" end_link=\"20\" trav_time=\"undefined\" distance=\"25000.0\" vehicleRefId=\"null\">1 6 15 20</route>
                // </leg>
                assert_eq!("car", leg.mode);
                assert_eq!(None, leg.trav_time);
                assert_eq!(None, leg.dep_time);
                let route = leg.route.as_ref().unwrap();
                assert_eq!(Some("links".to_string()), route.r#type);
                assert_eq!(Some("1".to_string()), route.start_link);
                assert_eq!(Some("20".to_string()), route.end_link);
                assert_eq!("undefined", route.trav_time.as_ref().unwrap());
                assert_eq!(25000.0, route.distance.unwrap());
                assert_eq!("null", route.vehicle.as_ref().unwrap());
                assert_eq!("1 6 15 20", route.route.as_ref().unwrap())
            }
        }

        let activity2 = plan.elements.get(4).unwrap();
        match activity2 {
            IOPlanElement::Activity(activity) => {
                //<activity type=\"w\" link=\"20\" x=\"10000.0\" y=\"0.0\" max_dur=\"03:30:00\" >
                assert_eq!("w", activity.r#type);
                assert_eq!("20", activity.link.as_ref().unwrap());
                assert_eq!(10000.0, activity.x.unwrap());
                assert_eq!(0.0, activity.y.unwrap());
                assert_eq!(Some(String::from("03:30:00")), activity.max_dur);
                assert_eq!(None, activity.start_time);
                assert_eq!(None, activity.end_time);
            }
            IOPlanElement::Leg { .. } => {
                panic!("Plan element at inded 6 was expected to be an activity but was a Leg.")
            }
        }
    }

    #[test]
    fn reads_optional_plan_score() {
        let xml = r#"
            <population>
                <person id="1">
                    <plan selected="yes" score="-12.5">
                        <activity type="home" link="1" />
                    </plan>
                    <plan selected="no">
                        <activity type="home" link="1" />
                    </plan>
                </person>
            </population>
        "#;

        let population: IOPopulation = from_str(xml).unwrap();
        let plans = &population.persons[0].plans;

        assert_eq!(Some(-12.5), plans[0].score);
        assert_eq!(None, plans[1].score);
    }

    #[test]
    fn writes_plan_score_only_when_present() {
        let with_score = to_string(&IOPlan {
            attributes: None,
            selected: true,
            score: Some(7.25),
            elements: Vec::new(),
        })
        .unwrap();
        let without_score = to_string(&IOPlan {
            attributes: None,
            selected: true,
            score: None,
            elements: Vec::new(),
        })
        .unwrap();

        assert!(with_score.contains("score=\"7.25\""));
        assert!(!without_score.contains("score="));
    }

    #[deterministic_id_test]
    fn writes_default_subpopulation_attribute() {
        let population = Population::from_persons(vec![InternalPerson::new(
            Id::create("1"),
            InternalPlan::default(),
        )]);
        let io_population = IOPopulation::from(&population);
        let attributes = io_population.persons[0].attributes.as_ref().unwrap();

        assert_eq!(Some("person"), attributes.find("subpopulation"));
    }

    #[deterministic_id_test]
    fn writes_non_default_subpopulation_attribute() {
        let person = InternalPerson::from(IOPerson {
            attributes: Some(IOAttributes {
                attributes: vec![IOAttribute::new_with_class(
                    "subpopulation".to_string(),
                    "java.lang.String".to_string(),
                    "freight".to_string(),
                )],
            }),
            id: "1".to_string(),
            plans: vec![IOPlan {
                attributes: None,
                selected: true,
                score: None,
                elements: Vec::new(),
            }],
        });
        let population = Population::from_persons(vec![person]);
        let io_population = IOPopulation::from(&population);
        let attributes = io_population.persons[0].attributes.as_ref().unwrap();

        assert_eq!(Some("freight"), attributes.find("subpopulation"));
    }

    #[test]
    fn test_read_leg() {
        let xml = "<leg mode=\"walk\" dep_time=\"00:00:00\">
                                <attributes>
                                        <attribute name=\"routingMode\" class=\"java.lang.String\">car</attribute>
                                </attributes>
                                <route type=\"generic\" start_link=\"4410448#0\" end_link=\"4410448#0\" trav_time=\"00:00:46\" distance=\"57.23726831365165\"></route>
                        </leg>";

        let leg = from_str::<IOLeg>(xml).unwrap();
        assert_eq!(leg.mode, "walk");
        assert_eq!(leg.dep_time, Some(String::from("00:00:00")));
        assert_eq!(leg.trav_time, None);
        let route = leg.route.as_ref().unwrap();
        assert_eq!(route.r#type, Some("generic".to_string()));
        assert_eq!(route.start_link, Some("4410448#0".to_string()));
        assert_eq!(route.end_link, Some("4410448#0".to_string()));
        assert_eq!(route.trav_time, Some(String::from("00:00:46")));
        assert_eq!(route.distance.unwrap(), 57.23726831365165);
        assert_eq!(route.vehicle, None);
        assert_eq!(route.route, None);
    }

    #[test]
    fn test_read_leg_with_pt() {
        let xml = "<leg mode=\"pt\" trav_time=\"00:10:01\">
				<attributes>
					<attribute name=\"routingMode\" class=\"java.lang.String\">pt</attribute>
				</attributes>
				<route type=\"default_pt\" start_link=\"33\" end_link=\"11\" trav_time=\"00:10:01\" distance=\"NaN\">{\"transitRouteId\":\"3to1\",\"boardingTime\":\"undefined\",\"transitLineId\":\"Blue Line\",\"accessFacilityId\":\"3\",\"egressFacilityId\":\"1\"}</route>
			</leg>";
        let leg = from_str::<IOLeg>(xml).unwrap();
        assert_eq!(leg.mode, "pt");
        assert_eq!(leg.dep_time, None);
        assert_eq!(leg.trav_time, Some(String::from("00:10:01")));
        let route = leg.route.as_ref().unwrap();
        assert_eq!(route.r#type, Some("default_pt".to_string()));
        assert_eq!(route.start_link, Some("33".to_string()));
        assert_eq!(route.end_link, Some("11".to_string()));
        assert_eq!(route.trav_time, Some(String::from("00:10:01")));
        assert!(route.distance.unwrap().is_nan());
        assert_eq!(route.vehicle, None);
        assert_eq!(
            route.route,
            Some(String::from(
                "{\"transitRouteId\":\"3to1\",\"boardingTime\":\"undefined\",\"transitLineId\":\"Blue Line\",\"accessFacilityId\":\"3\",\"egressFacilityId\":\"1\"}"
            ))
        );
    }

    #[test]
    fn read_example_file() {
        let population = IOPopulation::from_file("./assets/population-v6-34-persons.xml");
        assert_eq!(34, population.persons.len())
    }

    #[test]
    fn read_example_file_gzipped() {
        let population = IOPopulation::from_file("./assets/population-v6-34-persons.xml.gz");
        assert_eq!(34, population.persons.len())
    }

    #[test]
    fn write_and_read_population_zstd() {
        let population = IOPopulation::from_file("./assets/population-v6-34-persons.xml");
        let path = PathBuf::from("./test_output/io/xml_population/population.xml.zst");
        population.to_file(&path);

        let result = IOPopulation::from_file(&path);
        assert_eq!(population.persons.len(), result.persons.len());
    }

    #[deterministic_id_test]
    fn test_conversion() {
        let _net = Network::from_file(
            "./assets/equil/equil-network.xml",
            2,
            &PartitionMethod::Metis(MetisOptions::default()),
        );
        let mut garage = Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"));

        let persons = load_from_xml(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &mut garage,
        );
        assert_eq!(persons.len(), 100);

        for i in 1u32..101 {
            assert!(persons.get(&Id::get_from_ext(&format!("{}", i))).is_some());
        }
    }

    #[test]
    fn test_activity_attributes() {
        let xml = "<activity type=\"home_86400\" link=\"-150731516#0\" x=\"789538.61\" y=\"5813719.01\" end_time=\"07:47:35\" >
                                <attributes>
                                        <attribute name=\"initialEndTime\" class=\"java.lang.Double\">26455.0</attribute>
                                        <attribute name=\"orig_dist\" class=\"java.lang.Double\">0.0</attribute>
                                </attributes>
                        </activity>";
        let attributes = from_str::<IOActivity>(xml)
            .unwrap()
            .attributes
            .unwrap()
            .attributes;
        assert_eq!(attributes.len(), 2);
        assert_eq!(
            attributes.get(0).unwrap(),
            &IOAttribute::new_with_class(
                String::from("initialEndTime"),
                String::from("java.lang.Double"),
                String::from("26455.0")
            )
        );
        assert_eq!(
            attributes.get(1).unwrap(),
            &IOAttribute::new_with_class(
                String::from("orig_dist"),
                String::from("java.lang.Double"),
                String::from("0.0")
            )
        );
    }

    /// Sorts given (optional) IOAttributes by name and changes any attribute class "Integer" to
    /// "Long"
    fn canonicalize_attributes(attrs: &mut Option<IOAttributes>) -> &Option<IOAttributes> {
        match attrs {
            Some(attrs) => {
                // sort attributes by name
                attrs.attributes.sort_by(|a, b| a.name.cmp(&b.name));

                // change any attribute class "Integer" to "Long"
                // (since when writing, we always write integers as "Long")
                for attr in attrs.attributes.iter_mut() {
                    if attr.class == "java.lang.Integer" {
                        attr.class = "java.lang.Long".to_string();
                    }
                }
            }
            None => {} // if no attributes present, do nothing
        }

        attrs
    }

    /// goes through all plans of the given person and looks for legs containing routes with
    /// vehicle=None.
    /// For those, generates a vehicle id based on the person id and the mode of transport of the
    /// leg, and sets that as the vehicle of the route.
    /// This matches the approach done when creating (internal) populations.
    fn replace_none_vehicles_with_default(person: &mut IOPerson) -> &mut IOPerson {
        for plan in person.plans.iter_mut() {
            for element in plan.elements.iter_mut() {
                // if plan element is a leg
                if let IOPlanElement::Leg(leg) = element {
                    // and it has a route
                    if let Some(ref mut route) = leg.route {
                        // which has vehicle=None
                        if route.vehicle.is_none() {
                            // generate vehicle id based on person id and mode of transport
                            let generated_vehicle_id = format!("{}_{}", person.id, leg.mode);
                            route.vehicle = Some(generated_vehicle_id);
                        }
                    }
                }
            }
        }
        person
    }

    /// compare input population XML to result of writing the same population to XML.
    /// Works via parsing both XMLs into IOPopulations and comparing those.
    #[deterministic_id_test]
    fn test_xml_writer() {
        let _guard = init_std_out_logging_thread_local();

        // Load example population from XML, convert to internal and write to xml again:

        let input_pop_file = PathBuf::from("./assets/population-v6-34-persons.xml");
        let internal_pop = Population::from_file(&input_pop_file, &mut Garage::default());
        let output_pop_file =
            PathBuf::from("./test_output/io/population/34-persons-xml_output.xml");
        create_dir_all(output_pop_file.parent().unwrap()).unwrap();
        // write internal population to output XML file
        write_to_xml(&internal_pop, &output_pop_file);

        // read the written XML population file as IOPopulation
        let mut io_pop_from_written_output = IOPopulation::from_file(&output_pop_file);

        // read the original XML data as IOPopulation as well, to compare with the written XML
        let mut io_pop = IOPopulation::from_file(&input_pop_file);

        // Before comparing the two IOPopulations, we need to perform some minor modifications,
        // to remove possible differences that we don't want to catch:

        // sort persons by id in both files
        io_pop.persons.sort_by(|p1, p2| p1.id.cmp(&p2.id));
        io_pop_from_written_output
            .persons
            .sort_by(|p1, p2| p1.id.cmp(&p2.id));

        // for each person in both files...
        for person in io_pop
            .persons
            .iter_mut()
            .chain(io_pop_from_written_output.persons.iter_mut())
        {
            // canonicalize attributes if present
            canonicalize_attributes(&mut person.attributes);

            // when vehicle is None in an IORoute, generate a vehicle id based on the person id and
            // the mode of transport, as is done when generating (internal) Routes from IORoutes with
            // vehicle=None.
            replace_none_vehicles_with_default(person);
        }
        assert_eq!(io_pop, io_pop_from_written_output);
    }

    const ID_ORDER_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE population SYSTEM "http://www.matsim.org/files/dtd/population_v6.dtd">
<population>
    <attributes>
        <attribute name="coordinateReferenceSystem" class="java.lang.String">Atlantis</attribute>
    </attributes>
    <!-- persons are not sorted by id, and <person id="ignored"></person> is a comment -->
    <person id="b">
        <attributes>
            <attribute name="subpopulation" class="java.lang.String">freight</attribute>
        </attributes>
        <plan selected="yes" score="1.5">
            <activity type="home" link="l1" x="0.0" y="0.0" end_time="08:00:00"/>
            <leg mode="drt" dep_time="08:00:00">
                <attributes>
                    <attribute name="routingMode" class="java.lang.String">drt_routing</attribute>
                </attributes>
                <route type="links" start_link="l1" end_link="l3" vehicleRefId="null">l1 new-2 l3</route>
            </leg>
            <activity type="work" facility="f1" x="1.0" y="1.0" max_dur="01:00:00"/>
            <leg mode="pt">
                <route type="default_pt" start_link="l3" end_link="new-4" trav_time="00:10:00" vehicleRefId="bus 1">{"transitRouteId":"r1","boardingTime":"08:10:00","transitLineId":"line1","accessFacilityId":"s1","egressFacilityId":"s2"}</route>
            </leg>
            <activity type="pt interaction" link="new-4" x="2.0" y="2.0" max_dur="00:00:00"/>
            <leg mode="walk">
                <route type="generic" start_link="new-4" end_link="new-5" trav_time="00:05:00" distance="10.0"/>
            </leg>
            <activity type="home" link="new-5" facility="f2" x="0.0" y="0.0"/>
        </plan>
        <plan selected="no">
            <activity type="home" link="l1" x="0.0" y="0.0" end_time="09:00:00"/>
            <leg mode="bike" trav_time="00:20:00"/>
            <activity type="home" link="l1" x="0.0" y="0.0"/>
        </plan>
    </person>
    <person id="a">
        <attributes>
            <attribute name="subpopulation" class="java.lang.Integer">3</attribute>
        </attributes>
        <plan selected="yes">
            <activity type="shop" link="new-6" end_time="10:00:00"/>
            <leg mode="car">
                <route type="links" start_link="new-6" end_link="new-6" vehicleRefId="">new-6</route>
            </leg>
            <activity type="home" link="l1"/>
        </plan>
    </person>
    <person id="c"><plan selected="yes"><activity type="home" link="l3"/></plan></person>
</population>
"#;

    fn write_id_order_xml(test: &str) -> PathBuf {
        let path = PathBuf::from(format!(
            "./test_output/simulation/io/xml/population/{test}/plans.xml"
        ));
        create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, ID_ORDER_XML).unwrap();
        path
    }

    type Loaded = (
        BTreeMap<u64, Vec<String>>,
        IntMap<Id<InternalPerson>, InternalPerson>,
    );

    /// Loads the population like the sequential implementation did before persons were converted
    /// in parallel.
    fn load_sequentially(path: &Path, setup: &dyn Fn() -> Garage) -> Loaded {
        id::reset_store();
        let mut garage = setup();
        let mut io_pop = IOPopulation::from_file(path);
        io_pop.persons.sort_by(|a, b| a.id.cmp(&b.id));
        create_ids(&io_pop, &mut garage);
        let persons = io_pop
            .persons
            .into_iter()
            .map(InternalPerson::from)
            .map(|p| (p.id().clone(), p))
            .collect();
        (id::snapshot_store(), persons)
    }

    fn load_in_parallel(path: &Path, setup: &dyn Fn() -> Garage, threads: usize) -> Loaded {
        id::reset_store();
        let mut garage = setup();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let persons = pool.install(|| load_from_xml(path, &mut garage));
        (id::snapshot_store(), persons)
    }

    fn assert_parallel_matches_sequential(
        path: &Path,
        setup: &dyn Fn() -> Garage,
        thread_counts: &[usize],
    ) {
        let (expected_ids, expected_persons) = load_sequentially(path, setup);
        assert!(!expected_persons.is_empty());
        for &threads in thread_counts {
            let (ids, persons) = load_in_parallel(path, setup, threads);
            assert_eq!(expected_ids, ids, "ids with {threads} threads");
            assert_eq!(expected_persons, persons, "persons with {threads} threads");
        }
    }

    #[deterministic_id_test]
    fn parallel_loading_creates_ids_in_sequential_order() {
        let path = write_id_order_xml("parallel_loading_creates_ids_in_sequential_order");
        let setup = || {
            // Links which exist before the population is loaded, like the links of a network.
            for link in ["l1", "l3"] {
                Id::<Link>::create(link);
            }
            Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"))
        };
        assert_parallel_matches_sequential(&path, &setup, &[1, 8]);

        // Check the interleaving explicitly for the ids which are created by the conversion.
        let (ids, persons) = load_in_parallel(&path, &setup, 8);
        // The garage creates the modes of the vehicle types and `create_ids` the interaction and
        // activity types. Afterwards, the ids of each person are created in the order of the
        // sorted persons, with the subpopulation after all plans.
        assert_eq!(
            vec![
                "car",
                "walk",
                "car interaction",
                "walk interaction",
                "shop",
                "home",
                "work",
                "pt interaction",
                "person",
                "drt_routing",
                "drt",
                "pt",
                "bike",
                "freight"
            ],
            ids[&String::stable_type_id()].as_slice()
        );
        let links = &ids[&Link::stable_type_id()];
        let new_links: Vec<_> = links.iter().filter(|l| l.starts_with("new-")).collect();
        assert_eq!(vec!["new-6", "new-2", "new-4", "new-5"], new_links);
        assert_eq!(
            vec!["f1", "f2"],
            ids[&ActivityFacility::stable_type_id()].as_slice()
        );

        assert_eq!(3, persons.len());
        let b = &persons[&Id::get_from_ext("b")];
        assert_eq!("freight", b.subpopulation().external());
        assert_eq!(
            "person",
            persons[&Id::get_from_ext("a")].subpopulation().external()
        );
        let pt_route = b.plans()[0].legs()[1].route.as_ref().unwrap();
        assert_eq!(
            "bus 1",
            pt_route.as_generic().vehicle().as_ref().unwrap().external()
        );
        let drt_route = b.plans()[0].legs()[0].route.as_ref().unwrap();
        assert_eq!(
            "b_drt",
            drt_route
                .as_generic()
                .vehicle()
                .as_ref()
                .unwrap()
                .external()
        );
    }

    #[deterministic_id_test]
    fn parallel_loading_matches_sequential_loading_of_equil() {
        let setup = || {
            Network::from_file_as_is(&PathBuf::from("./assets/equil/equil-network.xml"));
            Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"))
        };
        assert_parallel_matches_sequential(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &setup,
            &[1, 8],
        );
    }

    #[deterministic_id_test]
    fn parallel_loading_matches_sequential_loading_of_berlin() {
        // No network is loaded, so that all links of routes are created while loading.
        let setup = || {
            Garage::from_file(&PathBuf::from(
                "./assets/berlin-v6.4/berlin-v6.4-vehicleTypes.xml",
            ))
        };
        assert_parallel_matches_sequential(
            &PathBuf::from("./assets/berlin-v6.4/berlin-v6.4-0.1pct.plans-filtered.xml.gz"),
            &setup,
            // Only one parallel run, since loading is slow in debug builds.
            &[8],
        );
    }

    #[deterministic_id_test]
    fn parallel_loading_matches_sequential_loading_of_34_persons() {
        assert_parallel_matches_sequential(
            &PathBuf::from("./assets/population-v6-34-persons.xml"),
            &Garage::default,
            &[1, 8],
        );
    }

    #[test]
    fn parallel_parsing_matches_sequential_parsing() {
        let path = write_id_order_xml("parallel_parsing_matches_sequential_parsing");
        assert_eq!(
            IOPopulation::from_file(&path),
            IOPopulation::from_file_parallel(&path)
        );
    }
}
