use crate::generated;
use crate::generated::MessageIter;
use crate::generated::general::Coordinate;
use crate::generated::population::leg::Route;
use crate::generated::population::{
    Activity, GenericRoute, Header, Leg, NetworkRoute, Person, Plan, PtRoute, PtRouteDescription,
};
use crate::simulation::id::Id;
use crate::simulation::scenario::population::{
    InternalActivity, InternalGenericRoute, InternalLeg, InternalNetworkRoute, InternalPerson,
    InternalPlan, InternalPtRoute, InternalPtRouteDescription, InternalRoute, Population,
};
use crate::simulation::time::SimTime;
use nohash_hasher::IntMap;
use prost::Message;
use std::fs;
use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;
use tracing::info;

fn duration_to_u64_nanos(duration: std::time::Duration) -> u64 {
    duration
        .as_nanos()
        .try_into()
        .expect("duration exceeds u64::MAX nanoseconds for proto encoding")
}

pub fn load_from_proto<F>(path: impl AsRef<Path>, filter: F) -> Population
where
    F: Fn(&InternalPerson) -> bool,
{
    info!("Loading population from file at: {:?}", path.as_ref());
    let file = File::open(path.as_ref())
        .unwrap_or_else(|_| panic!("Could not open File at {:?}", path.as_ref()));
    let mut reader = BufReader::new(file);

    if let Some(header_delim) = generated::read_delimiter(&mut reader) {
        let mut buffer = vec![0; header_delim];
        reader
            .read_exact(&mut buffer)
            .expect("Failed to read delimited buffer.");
        let header = Header::decode(buffer.as_slice()).expect("oh nono");
        info!("Header Info: {header:?}");
    }

    let mut persons = IntMap::default();

    for person in MessageIter::<Person, BufReader<File>>::new(reader) {
        let id = Id::get_from_ext(&person.id);
        let internal_person = InternalPerson::from(person);

        if filter(&internal_person) {
            persons.insert(id, internal_person);
        }
    }

    info!("Finished loading population");

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
            attributes: Default::default(),
            subpopulation: Some(value.subpopulation().external().to_string()),
        }
    }
}

impl Plan {
    fn from(value: &InternalPlan) -> Self {
        Self {
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
            link_id: value.link_id.external().to_string(),
            coordinate: value.coord.as_ref().map(|c| Coordinate {
                x: c.x,
                y: c.y,
                z: c.z,
            }),
            start_time_ns: value.start_time.map(SimTime::as_nanos),
            end_time_ns: value.end_time.map(SimTime::as_nanos),
            max_dur_ns: value.max_dur.map(duration_to_u64_nanos),
            attributes: value.attributes.as_cloned_map(),
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
    use crate::generated::population::{Leg, Person, Plan, PtRouteDescription};
    use crate::simulation::id::Id;
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::Network;
    use crate::simulation::scenario::population::{
        InternalActivity, InternalGenericRoute, InternalLeg, InternalPerson, InternalPlan,
        InternalPtRouteDescription, InternalRoute, Population,
    };
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::time::SimTime;
    use macros::deterministic_id_test;
    use std::path::PathBuf;
    use std::time::Duration;

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
        let round_trip = InternalActivity::from(wire);

        assert_eq!(10.0, round_trip.coord.as_ref().unwrap().x);
        assert_eq!(20.0, round_trip.coord.as_ref().unwrap().y);
        assert_eq!(0., round_trip.coord.as_ref().unwrap().z);
        assert_eq!(Some(SimTime::from_nanos(1_500_000)), round_trip.start_time);
        assert_eq!(Some(SimTime::from_nanos(2_250_000)), round_trip.end_time);
        assert_eq!(Some(Duration::from_nanos(3_500_000)), round_trip.max_dur);
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
            PathBuf::from("./assets/equil/equil-plans.xml.gz"),
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
            PathBuf::from("./assets/equil/equil-plans.xml.gz"),
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
}
