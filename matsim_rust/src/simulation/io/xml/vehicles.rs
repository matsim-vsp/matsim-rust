use std::path::Path;

use crate::simulation::io::xml;
use crate::simulation::io::xml::attributes::IOAttributes;
use crate::simulation::scenario::vehicles::Garage;
use serde::{Deserialize, Serialize};
use tracing::info;

pub(crate) fn load_from_xml(path: &Path) -> Garage {
    let io_vehicles = IOVehicleDefinitions::from_file(path.to_str().unwrap());
    Garage::from(io_vehicles)
}

pub(crate) fn write_to_xml(garage: &Garage, path: &Path) {
    info!("Converting Garage into xml type");

    let veh_types = garage
        .vehicle_types
        .values()
        .map(|t| IOVehicleType {
            id: t.id.external().to_owned(),
            description: None,
            capacity: None,
            length: Some(IODimension { meter: t.length }),
            width: Some(IODimension { meter: t.width }),
            maximum_velocity: Some(IOVelocity {
                meter_per_second: t.max_v,
            }),
            engine_information: None,
            cost_information: None,
            passenger_car_equivalents: Some(IOPassengerCarEquivalents { pce: t.pce }),
            network_mode: Some(IONetworkMode {
                network_mode: t.net_mode.external().to_owned(),
            }),
            flow_efficiency_factor: Some(IOFowEfficiencyFactor { factor: t.fef }),
            attributes: None,
        })
        .collect();

    let io_vehicles = IOVehicleDefinitions {
        veh_types,
        vehicles: vec![],
    };

    xml::write_to_file(
        &io_vehicles,
        path,
        "<!DOCTYPE network SYSTEM \"http://www.matsim.org/files/dtd http://www.matsim.org/files/dtd/vehicleDefinitions_v2.0.xsd\">",
    )
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(rename = "vehicleDefinitions")]
pub struct IOVehicleDefinitions {
    #[serde(rename = "vehicleType")]
    pub veh_types: Vec<IOVehicleType>,
    #[serde(rename = "vehicle", default)]
    pub vehicles: Vec<IOVehicle>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOVehicle {
    #[serde(rename = "@id")]
    pub id: String,
    #[serde(rename = "@type")]
    pub vehicle_type: String,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct IOVehicleType {
    #[serde(rename = "@id")]
    pub id: String,
    pub description: Option<String>,
    pub capacity: Option<IOCapacity>,
    pub length: Option<IODimension>,
    pub width: Option<IODimension>,
    pub maximum_velocity: Option<IOVelocity>,
    pub engine_information: Option<IOEngineInformation>,
    pub cost_information: Option<IOCostInformation>,
    pub passenger_car_equivalents: Option<IOPassengerCarEquivalents>,
    pub network_mode: Option<IONetworkMode>,
    pub flow_efficiency_factor: Option<IOFowEfficiencyFactor>,
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOCapacity {
    // leave emtpy for now
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IODimension {
    #[serde(rename = "@meter")]
    pub(crate) meter: f64,
}

impl Default for IODimension {
    fn default() -> Self {
        Self { meter: 1. }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub struct IOVelocity {
    #[serde(rename = "@meterPerSecond")]
    pub(crate) meter_per_second: f64,
}

impl Default for IOVelocity {
    fn default() -> Self {
        Self {
            meter_per_second: f64::MAX,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Copy)]
pub struct IOPassengerCarEquivalents {
    #[serde(rename = "@pce")]
    pub(crate) pce: f64,
}

impl Default for IOPassengerCarEquivalents {
    fn default() -> Self {
        Self { pce: 1. }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(rename_all = "camelCase")]
pub struct IONetworkMode {
    #[serde(rename = "@networkMode")]
    pub(crate) network_mode: String,
}

impl Default for IONetworkMode {
    fn default() -> Self {
        Self {
            network_mode: String::from("car"),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOFowEfficiencyFactor {
    #[serde(rename = "@factor")]
    pub(crate) factor: f64,
}

impl Default for IOFowEfficiencyFactor {
    fn default() -> Self {
        Self { factor: 1. }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOEngineInformation {
    // leave empty for now.
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOCostInformation {
    // leave empty for now.
}

impl IOVehicleDefinitions {
    pub fn from_file(file: &str) -> Self {
        xml::read_from_file(file)
    }
}

#[cfg(test)]
mod test {
    use std::path::PathBuf;

    use crate::simulation::InternalAttributes;
    use crate::simulation::id::Id;
    use crate::simulation::io::xml::vehicles::IOVehicleDefinitions;
    use crate::simulation::scenario::vehicles::Garage;
    use crate::simulation::scenario::vehicles::{InternalVehicleType, from_file, to_file};
    use macros::deterministic_id_test;
    use quick_xml::de::from_str;

    #[test]
    fn from_string_empty_type() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                            <vehicleDefinitions xmlns=\"http://www.matsim.org/files/dtd\">\
                                <vehicleType id=\"some-vehicle-id\">\
                                </vehicleType>\
                            </vehicleDefinitions>\
                        ";
        let veh_def: IOVehicleDefinitions = from_str(xml).unwrap();

        assert_eq!(1, veh_def.veh_types.len());

        let veh_type = veh_def.veh_types.first().unwrap();
        assert_eq!("some-vehicle-id", veh_type.id.as_str());
    }

    #[test]
    fn from_string_full_type() {
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                            <vehicleDefinitions xmlns=\"http://www.matsim.org/files/dtd\">\
                                <vehicleType id=\"some-vehicle-id\">\
                                    <description>some-description</description>\
                                    <length meter=\"9.5\"/>\
                                    <width meter=\"9.5\"/>\
                                    <maximumVelocity meterPerSecond=\"9.5\"/>\
                                    <passengerCarEquivalents pce=\"9.5\"/>\
                                    <networkMode networkMode=\"some-network-mode\"/>\
                                    <flowEfficiencyFactor factor=\"9.5\"/>\
                                </vehicleType>\
                                <vehicle id=\"drt\" type=\"some-vehicle-id\">
                                    <attributes>
                                        <attribute name=\"dvrpMode\" class=\"java.lang.String\">drt</attribute>
                                        <attribute name=\"startLink\" class=\"java.lang.String\">42</attribute>
                                        <attribute name=\"serviceBeginTime\" class=\"java.lang.Double\">0</attribute>
                                        <attribute name=\"serviceEndTime\" class=\"java.lang.Double\">84000</attribute>
                                    </attributes>
                                </vehicle>
                            </vehicleDefinitions>\
                        ";

        let veh_def: IOVehicleDefinitions = from_str(xml).unwrap();

        assert_eq!(1, veh_def.veh_types.len());

        let veh_type = veh_def.veh_types.first().unwrap();
        assert_eq!("some-vehicle-id", veh_type.id.as_str());
        assert_eq!(
            "some-description",
            veh_type.description.as_ref().unwrap().as_str()
        );
        assert_eq!(
            "some-network-mode",
            veh_type
                .network_mode
                .as_ref()
                .unwrap()
                .network_mode
                .as_str()
        );
        assert_eq!(9.5, veh_type.length.as_ref().unwrap().meter);
        assert_eq!(9.5, veh_type.width.as_ref().unwrap().meter);
        assert_eq!(
            9.5,
            veh_type.maximum_velocity.as_ref().unwrap().meter_per_second
        );
        assert_eq!(
            9.5,
            veh_type.passenger_car_equivalents.as_ref().unwrap().pce
        );
        assert_eq!(
            9.5,
            veh_type.flow_efficiency_factor.as_ref().unwrap().factor
        );

        let vehicle = veh_def.vehicles.first().unwrap();
        assert_eq!("drt", vehicle.id.as_str());
        assert_eq!("some-vehicle-id", vehicle.vehicle_type.as_str());
        let attrs = vehicle.attributes.as_ref().unwrap();
        assert_eq!(4, attrs.attributes.len());
        assert_eq!("drt", attrs.find_or_else("dvrpMode", || ""));
        assert_eq!("42", attrs.find_or_else("startLink", || ""));
        assert_eq!("0", attrs.find_or_else("serviceBeginTime", || ""));
        assert_eq!("84000", attrs.find_or_else("serviceEndTime", || ""));
    }

    #[deterministic_id_test]
    fn test_to_from_file_xml() {
        let file = &PathBuf::from(
            "./test_output/simulation/vehicles/io/test_to_from_file_xml/vehicles.xml",
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
    }
}
