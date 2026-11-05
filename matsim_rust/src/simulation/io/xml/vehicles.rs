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
            attributes: IOAttributes::from_internal_none_if_empty(&t.attributes),
            description: None,
            capacity: None,
            length: Some(IODimension { meter: t.length }),
            width: Some(IODimension { meter: t.width }),
            maximum_velocity: if t.max_v == f64::MAX {
                None
            } else {
                Some(IOVelocity {
                    meter_per_second: t.max_v,
                })
            },
            engine_information: None,
            cost_information: None,
            passenger_car_equivalents: Some(IOPassengerCarEquivalents { pce: t.pce }),
            network_mode: Some(IONetworkMode {
                network_mode: t.net_mode.external().to_owned(),
            }),
            flow_efficiency_factor: Some(IOFowEfficiencyFactor { factor: t.fef }),
        })
        .collect();

    // Sorted by id, so that the file does not depend on the order of the map.
    let mut vehicles: Vec<_> = garage
        .vehicles
        .values()
        .map(|vehicle| IOVehicle {
            id: vehicle.id.external().to_owned(),
            vehicle_type: vehicle.vehicle_type.external().to_owned(),
            attributes: IOAttributes::from_internal_none_if_empty(&vehicle.attributes),
        })
        .collect();
    vehicles.sort_by(|a, b| a.id.cmp(&b.id));

    let io_vehicles = IOVehicleDefinitions {
        xmlns: matsim_namespace(),
        xmlns_xsi: xsi_namespace(),
        schema_location: vehicles_schema_location(),
        veh_types,
        vehicles,
    };

    // The format is defined by an XML schema, not by a DTD. The schema is referenced by the
    // namespace attributes of the root element, as MATSim does.
    xml::write_to_file(&io_vehicles, path, "")
}

fn matsim_namespace() -> String {
    String::from("http://www.matsim.org/files/dtd")
}

fn xsi_namespace() -> String {
    String::from("http://www.w3.org/2001/XMLSchema-instance")
}

fn vehicles_schema_location() -> String {
    String::from(
        "http://www.matsim.org/files/dtd http://www.matsim.org/files/dtd/vehicleDefinitions_v2.0.xsd",
    )
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
#[serde(rename = "vehicleDefinitions")]
pub struct IOVehicleDefinitions {
    // The namespace declarations, which the schema requires. They are not read, but always written
    // with the values of vehicleDefinitions_v2.0.xsd.
    #[serde(rename = "@xmlns", skip_deserializing, default = "matsim_namespace")]
    pub xmlns: String,
    #[serde(rename = "@xmlns:xsi", skip_deserializing, default = "xsi_namespace")]
    pub xmlns_xsi: String,
    #[serde(
        rename = "@xsi:schemaLocation",
        skip_deserializing,
        default = "vehicles_schema_location"
    )]
    pub schema_location: String,
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
    // The schema requires the attributes to be the first child.
    #[serde(rename = "attributes", skip_serializing_if = "Option::is_none")]
    pub attributes: Option<IOAttributes>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capacity: Option<IOCapacity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub length: Option<IODimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<IODimension>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum_velocity: Option<IOVelocity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_information: Option<IOEngineInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_information: Option<IOCostInformation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub passenger_car_equivalents: Option<IOPassengerCarEquivalents>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network_mode: Option<IONetworkMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flow_efficiency_factor: Option<IOFowEfficiencyFactor>,
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
        let mut attributes = InternalAttributes::default();
        attributes.insert("doorOperationMode", "serial");

        garage.add_veh_type(InternalVehicleType {
            id: Id::create("some-type"),
            length: 10.,
            width: 20.0,
            max_v: 1000.0,
            pce: 20.0,
            fef: 0.3,
            net_mode: Id::<String>::create("some network type 🚕"),
            attributes,
        });
        garage.add_veh_by_type(&Id::create("some-person"), &Id::get_from_ext("some-type"));

        to_file(&garage, file);
        let loaded_garage = from_file(file);
        assert_eq!(garage.vehicle_types, loaded_garage.vehicle_types);

        // The schema is referenced by the namespace, not by a DTD.
        let content = std::fs::read_to_string(file).unwrap();
        assert!(content.contains("<vehicleDefinitions xmlns=\"http://www.matsim.org/files/dtd\""));
        assert!(!content.contains("<!DOCTYPE"));
    }
}
