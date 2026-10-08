use crate::generated::general::Coordinate;
use crate::generated::population::leg::Route;
use crate::generated::population::{
    Activity, GenericRoute, Header, Leg, NetworkRoute, Person, Plan, PtRoute, PtRouteDescription,
};
use crate::simulation::io::batch::read_in_batches;
use crate::simulation::io::proto::{next_delimiter_length, read_length_delimited};
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlan, InternalPtRoute, InternalPtRouteDescription, InternalRoute, Population,
    ProtoPersonDraft,
};
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use prost::Message;
use rayon::prelude::*;
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::info;

fn duration_to_u64_nanos(duration: std::time::Duration) -> u64 {
    duration
        .as_nanos()
        .try_into()
        .expect("duration exceeds u64::MAX nanoseconds for proto encoding")
}

/// Loads a population written by [`write_to_proto`]. The ids must already be loaded into the id
/// store.
///
/// A reader thread reads the length delimited persons in batches. The persons of a batch are
/// decoded and converted in parallel without creating ids. Afterwards, the remaining ids are
/// created and the filter is applied sequentially in file order, so that the result and the id
/// assignment don't depend on the number of threads.
pub fn load_from_proto<F>(path: impl AsRef<Path>, filter: F) -> Population
where
    F: Fn(&InternalPerson) -> bool,
{
    info!("Loading population from file at: {:?}", path.as_ref());
    let start = Instant::now();
    let file = File::open(path.as_ref())
        .unwrap_or_else(|_| panic!("Could not open File at {:?}", path.as_ref()));
    let mut reader = BufReader::with_capacity(1024 * 1024, file);

    if let Some(header_delim) = next_delimiter_length(&mut reader) {
        let mut buffer = vec![0; header_delim];
        reader
            .read_exact(&mut buffer)
            .expect("Failed to read delimited buffer.");
        let header = Header::decode(buffer.as_slice()).expect("oh nono");
        info!("Header Info: {header:?}");
    }

    let mut persons = IntMap::default();
    let mut decoding = Duration::ZERO;
    let mut merging = Duration::ZERO;

    read_in_batches(
        move || reader,
        |reader, buffer| read_length_delimited(reader, buffer),
        |batch| {
            let decode_start = Instant::now();
            let drafts: Vec<ProtoPersonDraft> = batch
                .par_records()
                .map(|bytes| {
                    let person = Person::decode(bytes).expect("Failed to decode person");
                    ProtoPersonDraft::from(person)
                })
                .collect();
            decoding += decode_start.elapsed();

            let merge_start = Instant::now();
            for draft in drafts {
                let internal_person = draft.into_person();
                if filter(&internal_person) {
                    persons.insert(internal_person.id().clone(), internal_person);
                }
            }
            merging += merge_start.elapsed();
        },
    );

    info!(
        "Finished loading population with {} persons in {:.2?} (decoding: {decoding:.2?}, merging: {merging:.2?}).",
        persons.len(),
        start.elapsed()
    );

    Population { persons }
}

pub fn write_to_proto(population: &Population, path: &Path) {
    info!("Converting Population into wire format");

    let prefix = path.parent().unwrap();
    fs::create_dir_all(prefix).unwrap();
    let file = File::create(path).unwrap_or_else(|_| panic!("Failed to create file at: {path:?}"));
    let mut writer = BufWriter::new(file);
    //write header
    let header = Header {
        version: 1,
        size: population.persons.len() as u32,
    };
    let mut bytes = Vec::new();
    header
        .encode_length_delimited(&mut bytes)
        .expect("TODO: panic message");
    writer.write_all(&bytes).expect("Failed to write");

    for person in population.persons.values() {
        bytes.clear();
        Person::from(person)
            .encode_length_delimited(&mut bytes)
            .expect("Failed to encode person");
        writer.write_all(&bytes).expect("failed to write buffer");
    }

    writer.flush().expect("Failed to flush buffer");
}

impl Person {
    pub fn from(value: &InternalPerson) -> Self {
        Self {
            id: value.id().external().to_string(),
            plan: value.plans().iter().map(Plan::from).collect(),
            attributes: value.attributes().as_cloned_map(),
            subpopulation: Some(value.subpopulation().external().to_string()),
        }
    }
}

impl Plan {
    fn from(value: &InternalPlan) -> Self {
        Self {
            attributes: value.attributes.as_cloned_map(),
            selected: value.selected,
            legs: value.legs().iter().map(|p| Leg::from(p)).collect(),
            acts: value.acts().iter().map(|l| Activity::from(l)).collect(),
            score: value.score,
        }
    }
}

impl Activity {
    fn from(value: &InternalActivity) -> Self {
        Self {
            act_type: value.act_type.external().to_string(),
            link_id: value.link_id.as_ref().map(|id| id.external().to_string()),
            coordinate: value.coord.as_ref().map(|c| Coordinate {
                x: c.x,
                y: c.y,
                z: c.z,
            }),
            start_time_ns: value.start_time.map(SimTime::as_nanos),
            end_time_ns: value.end_time.map(SimTime::as_nanos),
            max_dur_ns: value.max_dur.map(duration_to_u64_nanos),
            attributes: value.attributes.as_cloned_map(),
            facility_id: value
                .facility_id
                .as_ref()
                .map(|id| id.external().to_string()),
        }
    }
}

impl Leg {
    fn from(value: &InternalLeg) -> Self {
        Self {
            mode: value.mode.external().to_string(),
            routing_mode: value
                .routing_mode
                .as_ref()
                .map(|r| r.external().to_string()),
            dep_time_ns: value.dep_time.map(SimTime::as_nanos),
            trav_time_ns: value.trav_time.map(duration_to_u64_nanos),
            attributes: value.attributes.as_cloned_map(),
            route: value.route.as_ref().map(Route::from),
        }
    }
}

impl Route {
    fn from(value: &InternalRoute) -> Self {
        match value {
            InternalRoute::Generic(g) => Route::GenericRoute(GenericRoute::from(g)),
            InternalRoute::Network(n) => Route::NetworkRoute(NetworkRoute::from(n)),
            InternalRoute::Pt(p) => Route::PtRoute(PtRoute::from(p)),
        }
    }
}

impl GenericRoute {
    fn from(value: &InternalGenericRoute) -> Self {
        Self {
            start_link: value.start_link().external().to_string(),
            end_link: value.end_link().external().to_string(),
            trav_time_ns: value.trav_time().map(duration_to_u64_nanos),
            distance: value.distance(),
            veh_id: value.vehicle().as_ref().map(|v| v.external().to_string()),
        }
    }
}

impl NetworkRoute {
    fn from(value: &InternalNetworkRoute) -> Self {
        Self {
            delegate: Some(GenericRoute::from(value.generic_delegate())),
            route: value
                .route()
                .iter()
                .map(|id| id.external().to_string())
                .collect(),
        }
    }
}

impl PtRoute {
    fn from(value: &InternalPtRoute) -> Self {
        Self {
            delegate: Some(GenericRoute::from(value.generic_delegate())),
            information: Some(PtRouteDescription::from(value.description())),
        }
    }
}

impl PtRouteDescription {
    fn from(value: &InternalPtRouteDescription) -> Self {
        Self {
            transit_route_id: value.transit_route_id.clone(),
            boarding_time_ns: value.boarding_time.map(SimTime::as_nanos),
            transit_line_id: value.transit_line_id.clone(),
            access_facility_id: value.access_facility_id.clone(),
            egress_facility_id: value.egress_facility_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::generated::population::Activity;
    use crate::generated::population::{Header, Leg, Person, Plan, PtRouteDescription};
    use crate::simulation::id;
    use crate::simulation::id::Id;
    use crate::simulation::id::serializable_type::StableTypeId;
    use crate::simulation::io::proto::proto_population::load_from_proto;
    use crate::simulation::io::xml::population::{IOActivity, IOPlan, IOPopulation};
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::facilities::ActivityFacility;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
        InternalPtRouteDescription, InternalRoute, Population,
    };
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use prost::Message;
    use quick_xml::{de::from_str, se::to_string};
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::Duration;

    #[deterministic_id_test]
    fn person_and_plan_attributes_survive_xml_and_proto_round_trip() {
        let typed = r#"
            <attribute name="integer" class="java.lang.Integer">-7</attribute>
            <attribute name="long" class="java.lang.Long">9223372036854775807</attribute>
            <attribute name="double" class="java.lang.Double">-3.25</attribute>
            <attribute name="boolean" class="java.lang.Boolean">true</attribute>
            <attribute name="empty" class="java.lang.String"></attribute>
            <attribute name="escaped" class="java.lang.String">a &amp; &lt;b&gt;</attribute>
        "#;
        let xml = format!(
            r#"
            <population>
                <person id="attribute-person">
                    <attributes>
                        {typed}
                        <attribute name="subpopulation" class="java.lang.String">freight</attribute>
                        <attribute name="label" class="java.lang.String">person</attribute>
                    </attributes>
                    <plan selected="yes" score="-12.5">
                        <attributes>
                            {typed}
                            <attribute name="label" class="java.lang.String">plan</attribute>
                        </attributes>
                        <activity type="home" link="start" x="0" y="0">
                            <attributes>
                                <attribute name="label" class="java.lang.String">activity</attribute>
                            </attributes>
                        </activity>
                    </plan>
                </person>
            </population>
        "#
        );
        let io_population: IOPopulation = from_str(&xml).unwrap();
        let mut persons: Vec<_> = io_population
            .persons
            .into_iter()
            .map(InternalPerson::from)
            .collect();
        let person = &mut persons[0];
        person.attributes_mut().insert("added", "programmatic");
        let plan = &person.plans()[0];
        for attributes in [person.attributes(), &plan.attributes] {
            assert_eq!(attributes.get::<i64>("integer"), Some(-7));
            assert_eq!(attributes.get::<i64>("long"), Some(i64::MAX));
            assert_eq!(attributes.get::<f64>("double"), Some(-3.25));
            assert_eq!(attributes.get::<bool>("boolean"), Some(true));
            assert_eq!(attributes.get::<String>("empty").as_deref(), Some(""));
            assert_eq!(
                attributes.get::<String>("escaped").as_deref(),
                Some("a & <b>")
            );
        }
        assert_eq!(
            person.attributes().get::<String>("label").as_deref(),
            Some("person")
        );
        assert_eq!(person.subpopulation().external(), "freight");
        assert_eq!(
            plan.attributes.get::<String>("label").as_deref(),
            Some("plan")
        );
        assert_eq!(plan.score, Some(-12.5));
        assert!(plan.selected);
        assert_eq!(plan.elements.len(), 1);
        assert_eq!(
            plan.elements[0]
                .as_activity()
                .unwrap()
                .attributes
                .get::<String>("label")
                .as_deref(),
            Some("activity")
        );

        let proto_persons = persons
            .iter()
            .map(|person| {
                let bytes = Person::from(person).encode_to_vec();
                let decoded = Person::decode(bytes.as_slice()).unwrap();
                let round_trip = InternalPerson::from(decoded);
                assert_eq!(&round_trip, person);
                round_trip
            })
            .collect();
        let population = Population::from_persons(proto_persons);
        let written = to_string(&IOPopulation::from(&population)).unwrap();
        let reread: IOPopulation = from_str(&written).unwrap();
        assert_eq!(reread.persons.len(), persons.len());
        for io_person in reread.persons {
            let round_trip = InternalPerson::from(io_person);
            let original = persons
                .iter()
                .find(|person| person.id() == round_trip.id())
                .unwrap();
            // XML output always includes subpopulation, even when absent in the input.
            let mut expected = original.clone();
            let subpopulation = expected.subpopulation().external().to_string();
            expected
                .attributes_mut()
                .insert("subpopulation", subpopulation);
            assert_eq!(round_trip, expected);
        }
    }

    #[test]
    fn legacy_proto_plan_without_attributes_defaults_to_empty() {
        // The legacy schema encodes selected=true at field 1 and has no field 5.
        let wire = Plan::decode(&[0x08, 0x01][..]).unwrap();
        assert!(wire.attributes.is_empty());
        let plan = InternalPlan::from(wire);
        assert!(plan.selected);
        assert_eq!(plan.attributes, Default::default());
    }

    #[test]
    fn empty_plan_attributes_are_omitted_from_xml() {
        let plan = InternalPlan::default();
        let xml = to_string(&IOPlan::from(&plan)).unwrap();
        assert!(!xml.contains("<attributes"));
    }

    #[deterministic_id_test]
    fn activity_coordinate_round_trip_preserves_none_z() {
        Id::<String>::create("home");
        let activity = InternalActivity::new(
            Some(Coordinate::new_2d(10.0, 20.0)),
            "home",
            Id::create("1"),
            Some(SimTime::from_nanos(1_500_000)),
            Some(SimTime::from_nanos(2_250_000)),
            Some(Duration::from_nanos(3_500_000)),
        );

        let wire = Activity::from(&activity);
        let encoded = wire.encode_to_vec();
        let decoded = Activity::decode(encoded.as_slice()).unwrap();
        let round_trip = InternalActivity::from(decoded);

        assert_eq!(10.0, round_trip.coord.as_ref().unwrap().x);
        assert_eq!(20.0, round_trip.coord.as_ref().unwrap().y);
        assert_eq!(0., round_trip.coord.as_ref().unwrap().z);
        assert_eq!(Some(SimTime::from_nanos(1_500_000)), round_trip.start_time);
        assert_eq!(Some(SimTime::from_nanos(2_250_000)), round_trip.end_time);
        assert_eq!(Some(Duration::from_nanos(3_500_000)), round_trip.max_dur);
    }

    #[deterministic_id_test]
    fn activity_at_facility_without_link_survives_xml_and_proto_round_trip() {
        Id::<ActivityFacility>::create("f1");
        let mut activity = InternalActivity::new(
            Some(Coordinate::new_2d(10.0, 20.0)),
            "home",
            Id::create("1"),
            None,
            Some(SimTime::from_secs(60)),
            None,
        );
        activity.link_id = None;
        activity.facility_id = Some(Id::get_from_ext("f1"));

        let wire = Activity::from(&activity);
        let decoded = Activity::decode(wire.encode_to_vec().as_slice()).unwrap();
        assert_eq!(activity, InternalActivity::from(decoded));

        let xml = to_string(&IOActivity::from(&activity)).unwrap();
        assert!(xml.contains(r#"facility="f1""#), "{xml}");
        assert!(!xml.contains("link="), "{xml}");
        let from_xml = InternalActivity::from(from_str::<IOActivity>(&xml).unwrap());
        assert_eq!(activity, from_xml);
    }

    #[deterministic_id_test]
    fn leg_round_trip_preserves_sub_millisecond_times() {
        Id::<String>::create("walk");
        let route = InternalRoute::Generic(InternalGenericRoute::new(
            Id::create("start"),
            Id::create("end"),
            Some(Duration::from_nanos(4_750_000)),
            Some(42.0),
            None,
        ));
        let leg = InternalLeg::new(
            route,
            "walk",
            "walk",
            Duration::from_nanos(3_250_000),
            Some(SimTime::from_nanos(1_500_000)),
        );

        let wire = Leg::from(&leg);
        let round_trip = InternalLeg::from(wire);

        assert_eq!(Some(SimTime::from_nanos(1_500_000)), round_trip.dep_time);
        assert_eq!(Some(Duration::from_nanos(3_250_000)), round_trip.trav_time);
        assert_eq!(
            Some(Duration::from_nanos(4_750_000)),
            round_trip.route.unwrap().as_generic().trav_time()
        );
    }

    #[test]
    fn pt_route_description_round_trip_preserves_sub_millisecond_boarding_time() {
        let description = InternalPtRouteDescription {
            transit_route_id: "route-1".to_string(),
            boarding_time: Some(SimTime::from_nanos(750_000)),
            transit_line_id: "line-1".to_string(),
            access_facility_id: "access-1".to_string(),
            egress_facility_id: "egress-1".to_string(),
        };

        let wire = PtRouteDescription::from(&description);
        let round_trip = InternalPtRouteDescription::from(wire);

        assert_eq!(Some(SimTime::from_nanos(750_000)), round_trip.boarding_time);
    }

    #[test]
    fn plan_round_trip_preserves_score() {
        let plan = InternalPlan {
            attributes: Default::default(),
            score: Some(42.5),
            selected: true,
            elements: Vec::new(),
        };

        let wire = Plan::from(&plan);
        let round_trip = InternalPlan::from(wire);

        assert_eq!(Some(42.5), round_trip.score);
    }

    #[deterministic_id_test]
    fn person_to_proto_always_writes_subpopulation() {
        let person = InternalPerson::new(Id::create("1"), InternalPlan::default());

        let wire = Person::from(&person);

        assert_eq!(Some("person".to_string()), wire.subpopulation);
    }

    #[deterministic_id_test]
    fn person_from_proto_preserves_subpopulation() {
        Id::<InternalPerson>::create("proto-subpopulation-freight");
        let person = InternalPerson::from(Person {
            id: "proto-subpopulation-freight".to_string(),
            plan: Vec::new(),
            attributes: Default::default(),
            subpopulation: Some("freight".to_string()),
        });

        assert_eq!("freight", person.subpopulation().external());
    }

    #[deterministic_id_test]
    fn person_from_proto_defaults_missing_subpopulation_to_person() {
        Id::<InternalPerson>::create("proto-subpopulation-default");
        let person = InternalPerson::from(Person {
            id: "proto-subpopulation-default".to_string(),
            plan: Vec::new(),
            attributes: Default::default(),
            subpopulation: None,
        });

        assert_eq!("person", person.subpopulation().external());
    }

    #[deterministic_id_test]
    fn test_proto() {
        let _net = Network::from_file_as_is(&PathBuf::from("./assets/equil/equil-network.xml"));
        let mut garage = Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"));
        let pop = Population::from_file(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &mut garage,
        );

        let file_path =
            PathBuf::from("./test_output/simulation/population/io/test_proto/plans.binpb");
        pop.to_file(&file_path);

        let proto_pop = Population::from_file(&file_path, &mut garage);

        for (id, person) in pop.persons {
            assert!(proto_pop.persons.contains_key(&id));
            let proto_person = proto_pop.persons.get(&id).unwrap();
            assert_eq!(person.id(), proto_person.id());
        }
    }

    #[deterministic_id_test]
    fn test_filtered_proto() {
        let _net = Network::from_file_as_is(&PathBuf::from("./assets/equil/equil-network.xml"));
        let mut garage = Garage::from_file(&PathBuf::from("./assets/equil/equil-vehicles.xml"));
        let pop = Population::from_file(
            &PathBuf::from("./assets/equil/equil-plans.xml.gz"),
            &mut garage,
        );

        let file_path =
            PathBuf::from("./test_output/simulation/population/io/test_filtered_proto/plans.binpb");
        pop.to_file(&file_path);

        let proto_pop =
            Population::from_file_filtered(&file_path, &mut garage, |p| p.id().external() == "1");

        let expected_id: Id<InternalPerson> = Id::get_from_ext("1");
        assert_eq!(1, proto_pop.persons.len());
        assert!(proto_pop.persons.contains_key(&expected_id));
    }

    fn write_persons(path: &Path, persons: &[Person]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut bytes = Header {
            version: 1,
            size: persons.len() as u32,
        }
        .encode_length_delimited_to_vec();
        for person in persons {
            person.encode_length_delimited(&mut bytes).unwrap();
        }
        std::fs::write(path, bytes).unwrap();
    }

    fn proto_person(id: &str, subpopulation: Option<&str>) -> Person {
        Person {
            id: id.to_string(),
            plan: vec![Plan {
                selected: true,
                acts: vec![Activity {
                    act_type: "home".to_string(),
                    link_id: Some("l1".to_string()),
                    ..Default::default()
                }],
                ..Default::default()
            }],
            attributes: Default::default(),
            subpopulation: subpopulation.map(str::to_string),
        }
    }

    #[deterministic_id_test]
    fn proto_loading_creates_missing_subpopulations_in_file_order() {
        let folder = PathBuf::from(
            "./test_output/simulation/io/proto/proto_population/proto_loading_creates_missing_subpopulations_in_file_order",
        );
        let plans = folder.join("plans.binpb");
        let ids = folder.join("ids.binpb");

        // Many persons, so that they are converted on different threads. Only the subpopulations
        // are missing from the id store.
        let subpopulations = ["zeta", "alpha", "freight", "alpha"];
        let persons: Vec<_> = (0..1000)
            .map(|i| {
                let subpopulation = (i % 7 == 0).then(|| subpopulations[(i / 7) % 4]);
                proto_person(&format!("p{i:04}"), subpopulation)
            })
            .collect();
        for person in &persons {
            Id::<InternalPerson>::create(&person.id);
        }
        Id::<String>::create("home");
        Id::<Link>::create("l1");
        id::store_to_file(&ids);
        write_persons(&plans, &persons);

        let mut results = Vec::new();
        for threads in [1, 8] {
            id::reset_store();
            id::load_from_file(&ids);
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap();
            let population = pool.install(|| load_from_proto(&plans, |_| true));
            assert_eq!(1000, population.persons.len());
            results.push((id::snapshot_store(), population));
        }

        // Person p0000 has subpopulation "zeta", p0001 none, i.e., "person".
        assert_eq!(
            vec!["home", "zeta", "person", "alpha", "freight"],
            results[0].0[&String::stable_type_id()].as_slice()
        );
        assert_eq!(results[0], results[1]);
    }

    #[deterministic_id_test]
    fn proto_loading_reads_short_last_person_and_empty_population() {
        let folder = PathBuf::from(
            "./test_output/simulation/io/proto/proto_population/proto_loading_reads_short_last_person_and_empty_population",
        );
        Id::<InternalPerson>::create("a");
        Id::<InternalPerson>::create("b");
        Id::<String>::create("home");
        Id::<String>::create("person");
        Id::<Link>::create("l1");

        // The last person encodes to fewer bytes than the maximum length of a length delimiter.
        let short = Person {
            id: "b".to_string(),
            ..Default::default()
        };
        assert!(short.encode_length_delimited_to_vec().len() < 10);
        let plans = folder.join("plans.binpb");
        write_persons(&plans, &[proto_person("a", None), short]);
        let population = load_from_proto(&plans, |_| true);
        assert_eq!(2, population.persons.len());
        assert!(
            population.persons[&Id::get_from_ext("b")]
                .plans()
                .is_empty()
        );

        let empty = folder.join("empty.binpb");
        write_persons(&empty, &[]);
        assert!(load_from_proto(&empty, |_| true).persons.is_empty());
    }
}
