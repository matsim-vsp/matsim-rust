use crate::generated;
use crate::generated::vehicles::{Vehicle, VehicleType, VehiclesContainer};
use crate::simulation::scenario::vehicles::Garage;
use crate::simulation::scenario::vehicles::{InternalVehicle, InternalVehicleType};
use std::path::Path;
use tracing::info;

pub(crate) fn write_to_proto(garage: &Garage, path: &Path) {
    info!("Converting Garage into wire type");
    let vehicle_types = garage
        .vehicle_types
        .values()
        .map(VehicleType::from)
        .collect();
    let vehicles = garage.vehicles.values().map(Vehicle::from).collect();

    let wire_format = VehiclesContainer {
        vehicle_types,
        vehicles,
    };
    info!("Finished converting Garage into wire type");
    generated::write_to_file(wire_format, path);
}

pub(crate) fn load_from_proto(path: &Path) -> Garage {
    let wire_garage: VehiclesContainer = generated::read_from_file(path);
    let vehicles = wire_garage
        .vehicles
        .into_iter()
        .map(InternalVehicle::from)
        .map(|v| (v.id.clone(), v))
        .collect();
    let vehicle_types = wire_garage
        .vehicle_types
        .into_iter()
        .map(InternalVehicleType::from)
        .map(|v_type| (v_type.id.clone(), v_type))
        .collect();
    Garage {
        vehicles,
        vehicle_types,
    }
}

impl Vehicle {
    pub fn from(vehicle: &InternalVehicle) -> Self {
        Self {
            id: vehicle.id().internal(),
            r#type: vehicle.vehicle_type.internal(),
            max_v: vehicle.max_v,
            pce: vehicle.pce,
            attributes: vehicle.attributes.as_cloned_map(),
        }
    }
}

impl VehicleType {
    pub fn from(vehicle: &InternalVehicleType) -> Self {
        Self {
            id: vehicle.id.internal(),
            length: vehicle.length,
            width: vehicle.width,
            max_v: vehicle.max_v,
            pce: vehicle.pce,
            fef: vehicle.fef,
            net_mode: vehicle.net_mode.internal(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::vehicles::{InternalVehicleType, from_file, to_file};
    use macros::deterministic_id_test;
    use std::path::PathBuf;

    #[deterministic_id_test]
    fn test_to_from_file_proto() {
        let file = &PathBuf::from(
            "./test_output/simulation/vehicles/io/test_to_from_file_xml/vehicles.binpb",
        );
        let mut garage = Garage::new();

        garage.add_veh_type(InternalVehicleType {
            id: Id::create("some-type"),
            length: 10.,
            width: 20.0,
            max_v: 1000.0,
            pce: 20.0,
            fef: 0.3,
            net_mode: Id::<String>::create("some network type 🚕"),
            attributes: InternalAttributes::default(),
        });
        garage.add_veh_by_type(&Id::create("some-person"), &Id::get_from_ext("some-type"));

        to_file(&garage, file);
        let loaded_garage = from_file(file);

        assert_eq!(garage.vehicle_types, loaded_garage.vehicle_types);
        assert_eq!(garage.vehicles, loaded_garage.vehicles);
    }

    #[deterministic_id_test]
    fn test_to_from_file_xml_zstd() {
        let file = &PathBuf::from(
            "./test_output/simulation/vehicles/io/test_to_from_file_xml_zstd/vehicles.xml.zst",
        );
        let mut garage = Garage::new();

        garage.add_veh_type(InternalVehicleType {
            id: Id::create("some-type"),
            length: 10.,
            width: 20.0,
            max_v: 1000.0,
            pce: 20.0,
            fef: 0.3,
            net_mode: Id::<String>::create("some-network-type"),
            attributes: InternalAttributes::default(),
        });
        garage.add_veh_by_type(&Id::create("some-person"), &Id::get_from_ext("some-type"));

        to_file(&garage, file);
        let loaded_garage = from_file(file);

        assert_eq!(garage.vehicle_types, loaded_garage.vehicle_types);
    }
}
