use crate::generated::vehicles::{Vehicle, VehicleType};
use crate::simulation::InternalAttributes;
use crate::simulation::agents::agent::SimulationAgent;
use crate::simulation::id::Id;
use crate::simulation::io::proto::proto_vehicles::{load_from_proto, write_to_proto};
use crate::simulation::io::xml::vehicles::{
    IOVehicle, IOVehicleDefinitions, IOVehicleType, load_from_xml, write_to_xml,
};
use crate::simulation::scenario::population::InternalPerson;
use crate::simulation::vehicles::SimulationVehicle;
use nohash_hasher::IntMap;
use std::path::Path;
use tracing::info;

pub fn from_file(path: &Path) -> Garage {
    if path.extension().unwrap().eq("binpb") {
        load_from_proto(path)
    } else if path.extension().unwrap().eq("xml")
        || path.extension().unwrap().eq("gz")
        || path.extension().unwrap().eq("zst")
    {
        load_from_xml(path)
    } else {
        panic!(
            "Tried to load {path:?}. File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
        );
    }
}

pub fn to_file(garage: &Garage, path: &Path) {
    if path.extension().unwrap().eq("binpb") {
        write_to_proto(garage, path);
    } else if path.extension().unwrap().eq("xml")
        || path.extension().unwrap().eq("gz")
        || path.extension().unwrap().eq("zst")
    {
        write_to_xml(garage, path);
    } else {
        panic!(
            "file format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension"
        );
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalVehicleType {
    pub id: Id<InternalVehicleType>,
    pub length: f64,
    pub width: f64,
    pub max_v: f64,
    pub pce: f64,
    pub fef: f64,
    pub net_mode: Id<String>,
    pub attributes: InternalAttributes,
}

#[derive(Debug, PartialEq, Clone)]
pub struct InternalVehicle {
    pub id: Id<InternalVehicle>,
    pub max_v: f64,
    pub pce: f64,
    pub vehicle_type: Id<InternalVehicleType>,
    pub attributes: InternalAttributes,
}

impl From<IOVehicleType> for InternalVehicleType {
    fn from(io: IOVehicleType) -> Self {
        InternalVehicleType {
            id: Id::create(&io.id),
            length: io.length.unwrap_or_default().meter,
            width: io.width.unwrap_or_default().meter,
            max_v: io.maximum_velocity.unwrap_or_default().meter_per_second,
            pce: io.passenger_car_equivalents.unwrap_or_default().pce,
            fef: io.flow_efficiency_factor.unwrap_or_default().factor,
            net_mode: Id::create(&io.network_mode.unwrap_or_default().network_mode),
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }
}

impl From<VehicleType> for InternalVehicleType {
    fn from(value: VehicleType) -> Self {
        Self {
            id: Id::get(value.id),
            length: value.length,
            width: value.width,
            max_v: value.max_v,
            pce: value.pce,
            fef: value.fef,
            net_mode: Id::get(value.net_mode),
            attributes: InternalAttributes::default(),
        }
    }
}

impl From<Vehicle> for InternalVehicle {
    fn from(value: Vehicle) -> Self {
        Self {
            id: Id::get(value.id),
            max_v: value.max_v,
            pce: value.pce,
            vehicle_type: Id::get(value.r#type),
            attributes: InternalAttributes::from(&value.attributes),
        }
    }
}

impl InternalVehicle {
    pub fn from_io(io: IOVehicle, io_veh_type: &InternalVehicleType) -> Self {
        InternalVehicle {
            id: Id::create(&io.id),
            max_v: io_veh_type.max_v,
            pce: io_veh_type.pce,
            vehicle_type: Id::create(&io.vehicle_type),
            attributes: io.attributes.map(Into::into).unwrap_or_default(),
        }
    }

    #[cfg(test)]
    pub fn new(id: u64, veh_type: u64, max_v: f64, pce: f64) -> Self {
        InternalVehicle {
            id: Id::create(&id.to_string()),
            max_v,
            pce,
            vehicle_type: Id::create(&veh_type.to_string()),
            attributes: Default::default(),
        }
    }

    pub fn id(&self) -> &Id<InternalVehicle> {
        &self.id
    }
}

#[derive(Debug, Clone)]
pub struct Garage {
    pub vehicles: IntMap<Id<InternalVehicle>, InternalVehicle>,
    pub vehicle_types: IntMap<Id<InternalVehicleType>, InternalVehicleType>,
}

impl Default for Garage {
    fn default() -> Self {
        Garage::new()
    }
}

impl From<IOVehicleDefinitions> for Garage {
    fn from(io_vehicles: IOVehicleDefinitions) -> Self {
        let mut result = Garage::new();
        for io_veh_type in io_vehicles.veh_types {
            add_io_veh_type(&mut result, io_veh_type);
        }
        for io_veh in io_vehicles.vehicles {
            add_io_veh(&mut result, io_veh)
        }
        let keys_ext: Vec<_> = result.vehicle_types.keys().map(|k| k.external()).collect();
        info!(
            "Created Garage from file with vehicle types: {:?}",
            keys_ext
        );
        result
    }
}

fn add_io_veh_type(garage: &mut Garage, io_veh_type: IOVehicleType) {
    let id: Id<InternalVehicleType> = Id::create(&io_veh_type.id);
    let net_mode: Id<String> =
        Id::create(&io_veh_type.network_mode.unwrap_or_default().network_mode);

    let veh_type = InternalVehicleType {
        id,
        length: io_veh_type.length.unwrap_or_default().meter,
        width: io_veh_type.width.unwrap_or_default().meter,
        max_v: io_veh_type
            .maximum_velocity
            .unwrap_or_default()
            .meter_per_second,
        pce: io_veh_type
            .passenger_car_equivalents
            .unwrap_or_default()
            .pce,
        fef: io_veh_type
            .flow_efficiency_factor
            .unwrap_or_default()
            .factor,
        net_mode,
        attributes: io_veh_type.attributes.map(Into::into).unwrap_or_default(),
    };
    garage.add_veh_type(veh_type);
}

fn add_io_veh(garage: &mut Garage, io_veh: IOVehicle) {
    let veh_type = garage.vehicle_types.get(&Id::get_from_ext(io_veh.vehicle_type.as_str()))
        .expect("Vehicle type of vehicle not found. There has to be a vehicle type defined before a vehicle can be added.");
    let vehicle = InternalVehicle::from_io(io_veh, veh_type);

    //add id for drt mode
    if let Some(o) = vehicle.attributes.get::<String>("dvrpMode") {
        Id::<String>::create(o.as_str());
    }

    garage.add_veh(vehicle);
}

impl Garage {
    pub fn new() -> Self {
        Garage {
            vehicles: Default::default(),
            vehicle_types: Default::default(),
        }
    }

    pub fn from_file(file_path: &Path) -> Self {
        from_file(file_path)
    }

    pub fn to_file(&self, file_path: &Path) {
        to_file(self, file_path);
    }

    pub fn add_veh_type(&mut self, veh_type: InternalVehicleType) {
        assert!(
            !self.vehicle_types.contains_key(&veh_type.id),
            "Vehicle type with id {:?} already exists.",
            &veh_type.id
        );

        self.vehicle_types.insert(veh_type.id.clone(), veh_type);
    }

    pub fn add_veh_by_type(
        &mut self,
        person_id: &Id<InternalPerson>,
        type_id: &Id<InternalVehicleType>,
    ) {
        let veh_id_ext = format!("{}_{}", person_id.external(), type_id.external());
        let veh_id = Id::create(&veh_id_ext);

        let veh_type = self.vehicle_types.get(type_id).unwrap();

        let vehicle = InternalVehicle {
            id: veh_id,
            vehicle_type: veh_type.id.clone(),
            attributes: Default::default(),
            max_v: veh_type.max_v,
            pce: veh_type.pce,
        };

        self.add_veh(vehicle);
    }

    pub fn add_veh(&mut self, veh: InternalVehicle) {
        let id = veh.id.clone();
        self.vehicles.insert(id, veh);
    }

    pub fn veh_id(
        &self,
        person_id: &Id<InternalPerson>,
        veh_type_id: &Id<InternalVehicleType>,
    ) -> Id<InternalVehicle> {
        let external = format!("{}_{}", person_id.external(), veh_type_id.external());
        Id::get_from_ext(&external)
    }

    pub fn unpark_veh_with_passengers(
        &self,
        agent: SimulationAgent,
        passengers: Vec<SimulationAgent>,
        id: Id<InternalVehicle>,
    ) -> SimulationVehicle {
        let vehicle = self
            .vehicles
            .get(&id)
            .unwrap_or_else(|| {
                panic!("Can't unpark vehicle with id {id}. It was not parked in this garage.")
            })
            .clone();

        SimulationVehicle::new(vehicle, Some(agent), passengers)

        // The following code would be used if mass conservation is enabled. But, there are some pitfalls.
        // One would need to configure for which vehicle types this is allowed.
        // Currently (apr '25), each and every mode needs to be a vehicle type, in particular walking.
        // But, this makes mass conservation complicated. Imagine a person walking from partition 1 -> 2, driving car from 2 -> 3 and then walk from 3 -> 1.
        // The "walk" vehicle would be parked at partition 2, but partition 3 would need it. Consequently, the simulation crashes right now.

        // let vehicle = self.vehicles.remove(&id).unwrap();
        // let mut vehicle = vehicle;
        // vehicle.driver = Some(person);
        // vehicle.passengers = passengers;
        // vehicle
    }

    pub fn unpark_veh(&self, agent: SimulationAgent, id: Id<InternalVehicle>) -> SimulationVehicle {
        self.unpark_veh_with_passengers(agent, vec![], id)
    }

    pub fn vehicle_type_id(&self, veh: &Id<InternalVehicle>) -> Id<InternalVehicleType> {
        self.vehicles
            .get(veh)
            .map(|v| v.vehicle_type.clone())
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;

    use crate::simulation::agents::agent::SimulationAgent;
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
    use crate::simulation::io::xml::vehicles::{
        IODimension, IOFowEfficiencyFactor, IONetworkMode, IOPassengerCarEquivalents,
        IOVehicleType, IOVelocity,
    };
    use crate::test_utils::create_vehicle_type;

    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::Link;
    use crate::simulation::scenario::population::{InternalActivity, InternalPerson, InternalPlan};
    use crate::simulation::scenario::vehicles::{
        Garage, InternalVehicle, InternalVehicleType, add_io_veh_type,
    };
    use macros::deterministic_id_test;

    #[deterministic_id_test]
    fn add_veh_type() {
        let mut garage = Garage::new();
        let type_id = Id::create("some-type");
        let mode = Id::new_internal(0);
        let veh_type = create_vehicle_type(&type_id, mode);

        garage.add_veh_type(veh_type);

        assert_eq!(1, garage.vehicle_types.len());
    }

    #[deterministic_id_test]
    #[should_panic]
    fn add_veh_type_reject_duplicate() {
        let mut garage = Garage::new();
        let type_id = Id::create("some-type");
        let mode = Id::new_internal(0);
        let veh_type1 = create_vehicle_type(&type_id, mode.clone());
        let veh_type2 = create_vehicle_type(&type_id, mode.clone());

        garage.add_veh_type(veh_type1);
        garage.add_veh_type(veh_type2);
    }

    #[deterministic_id_test]
    fn add_vehicle_without_type() {
        let mut garage = Garage::new();
        garage.add_veh(InternalVehicle {
            id: Id::create("0"),
            max_v: 0.0,
            pce: 0.0,
            vehicle_type: Id::create("0"),
            attributes: Default::default(),
        });
    }

    #[deterministic_id_test]
    fn add_vehicle() {
        // prepare garage with type
        let mut garage = Garage::new();
        let type_id = Id::create("vehicle_type");
        let mode = Id::create("car");
        let veh_type = create_vehicle_type(&type_id, mode);
        let veh_type_id = veh_type.id.clone();
        garage.add_veh_type(veh_type);

        let id = Id::<InternalVehicle>::create("veh");
        garage.add_veh(InternalVehicle {
            id,
            max_v: 0.0,
            pce: 0.0,
            vehicle_type: veh_type_id,
            attributes: Default::default(),
        });

        assert_eq!(1, garage.vehicles.len());
    }

    #[deterministic_id_test]
    fn unpark_from_shared_garage_does_not_mutate_catalog() {
        let mut garage = Garage::new();
        let type_id = Id::create("car");
        garage.add_veh_type(create_vehicle_type(&type_id, Id::create("car")));
        let person_id = Id::create("person");
        garage.add_veh_by_type(&person_id, &type_id);
        let vehicle_id = garage.veh_id(&person_id, &type_id);
        let garage = Arc::new(garage);
        let agent = SimulationAgent::new_plan_based(person("person"));

        let vehicle = garage.unpark_veh(agent, vehicle_id);

        assert_eq!("person_car", vehicle.id().external());
        assert_eq!(1, garage.vehicles.len());
    }

    #[deterministic_id_test]
    fn from_file() {
        let garage = Garage::from_file(&PathBuf::from("./assets/3-links/vehicles.xml"));
        assert_eq!(3, garage.vehicle_types.len());
        assert_eq!(0, garage.vehicles.len());
    }

    #[deterministic_id_test]
    fn add_empty_io_veh_type() {
        let io_veh_type = IOVehicleType {
            id: "some-id".to_string(),
            description: None,
            capacity: None,
            length: None,
            width: None,
            maximum_velocity: None,
            engine_information: None,
            cost_information: None,
            passenger_car_equivalents: None,
            network_mode: None,
            flow_efficiency_factor: None,
            attributes: None,
        };
        let mut garage = Garage::new();

        add_io_veh_type(&mut garage, io_veh_type);

        assert_eq!(1, garage.vehicle_types.len());

        // Check if IDs are created correctly
        Id::<String>::get_from_ext("car");
        Id::<InternalVehicleType>::get_from_ext("some-id");

        let veh_type_opt = garage.vehicle_types.values().next();
        assert!(veh_type_opt.is_some());
    }

    fn person(id: &str) -> InternalPerson {
        let mut plan = InternalPlan::default();
        plan.add_act(InternalActivity::new(
            Some(Coordinate::default()),
            "home",
            Id::<Link>::create("link"),
            None,
            None,
            None,
        ));
        InternalPerson::new(Id::create(id), plan)
    }

    #[deterministic_id_test]
    fn test_add_io_veh_type() {
        let io_veh_type = IOVehicleType {
            id: "some-id".to_string(),
            description: None,
            capacity: None,
            length: Some(IODimension { meter: 10. }),
            width: Some(IODimension { meter: 5. }),
            maximum_velocity: Some(IOVelocity {
                meter_per_second: 100.,
            }),
            engine_information: None,
            cost_information: None,
            passenger_car_equivalents: Some(IOPassengerCarEquivalents { pce: 21.0 }),
            network_mode: Some(IONetworkMode {
                network_mode: "some_mode".to_string(),
            }),
            flow_efficiency_factor: Some(IOFowEfficiencyFactor { factor: 2. }),
            attributes: Some(IOAttributes {
                attributes: vec![IOAttribute::new(
                    String::from("lod"),
                    String::from("teleported"),
                )],
            }),
        };
        let mut garage = Garage::new();
        add_io_veh_type(&mut garage, io_veh_type);

        let expected_id: Id<InternalVehicleType> = Id::get_from_ext("some-id");
        let expected_mode: Id<String> = Id::get_from_ext("some_mode");

        let veh_type_opt = garage.vehicle_types.values().next();
        assert!(veh_type_opt.is_some());
        let veh_type = veh_type_opt.unwrap();
        assert_eq!(veh_type.max_v, 100.);
        assert_eq!(veh_type.width, 5.0);
        assert_eq!(veh_type.length, 10.);
        assert_eq!(veh_type.pce, 21.);
        assert_eq!(veh_type.fef, 2.);
        assert_eq!(veh_type.id.internal(), expected_id.internal());
        assert_eq!(veh_type.net_mode.internal(), expected_mode.internal())
    }
}
