use crate::simulation::InternalAttributes;
use serde::{Deserialize, Serialize};
use tracing::warn;

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone)]
pub struct IOAttribute {
    #[serde(rename = "@name")]
    pub name: String,
    #[serde(rename = "@class")]
    pub class: String,
    #[serde(rename = "$value", default)]
    pub value: String,
}

impl IOAttribute {
    pub fn new(name: String, value: String) -> Self {
        IOAttribute {
            name,
            class: "".to_string(),
            value,
        }
    }

    pub fn new_with_class(name: String, class: String, value: String) -> Self {
        IOAttribute { name, class, value }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Clone, Default)]
pub struct IOAttributes {
    #[serde(rename = "attribute", default)]
    pub attributes: Vec<IOAttribute>,
}

impl IOAttributes {
    #[allow(clippy::needless_lifetimes)] // lifetimes are in fact needed here i think
    pub fn find_or_else<'a, F>(&'a self, name: &str, f: F) -> &'a str
    where
        F: FnOnce() -> &'a str,
    {
        let opt_attr = self.attributes.iter().find(|&attr| attr.name.eq(name));
        if let Some(&attr) = opt_attr.as_ref() {
            attr.value.as_str()
        } else {
            f()
        }
    }

    pub fn find_or_else_opt<'a, F>(attrs_opt: &'a Option<IOAttributes>, name: &str, f: F) -> &'a str
    where
        F: FnOnce() -> &'a str,
    {
        if let Some(attrs) = attrs_opt.as_ref() {
            attrs.find_or_else(name, f)
        } else {
            f()
        }
    }

    pub fn find(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|&attr| attr.name.eq(name))
            .map(|attr| attr.value.as_str())
    }

    pub(crate) fn from_internal_none_if_empty(attrs: &InternalAttributes) -> Option<IOAttributes> {
        if attrs.attributes.is_empty() {
            None
        } else {
            Some(IOAttributes::from(attrs))
        }
    }
}

impl From<&InternalAttributes> for IOAttributes {
    fn from(attrs: &InternalAttributes) -> Self {
        let mut res = IOAttributes::default();
        for (key, value) in attrs.iter() {
            match value {
                serde_json::value::Value::Number(num) => {
                    if num.is_i64() {
                        res.attributes.push(IOAttribute::new_with_class(
                            key.clone(),
                            "java.lang.Long".to_string(),
                            num.as_i64().unwrap().to_string(),
                        ))
                    } else if num.is_f64() {
                        res.attributes.push(IOAttribute::new_with_class(
                            key.clone(),
                            "java.lang.Double".to_string(),
                            num.to_string(),
                        ))
                    } else {
                        panic!(
                            "Unsupported number type for attribute with name '{}': {:?}",
                            key, num
                        );
                    }
                }
                serde_json::value::Value::String(s) => {
                    res.attributes.push(IOAttribute::new_with_class(
                        key.clone(),
                        "java.lang.String".to_string(),
                        s.clone(),
                    ))
                }
                serde_json::value::Value::Bool(b) => {
                    res.attributes.push(IOAttribute::new_with_class(
                        key.clone(),
                        "java.lang.Boolean".to_string(),
                        b.to_string(),
                    ))
                }

                serde_json::value::Value::Object(map)
                    if key == "vehicles" || key == "vehicleTypes" =>
                {
                    let class = if key == "vehicles" {
                        "org.matsim.vehicles.PersonVehicles"
                    } else {
                        "org.matsim.vehicles.PersonVehicleTypes"
                    };
                    assert!(
                        map.values().all(|value| value.is_string()),
                        "Attribute {key} must contain only string values"
                    );
                    res.attributes.push(IOAttribute::new_with_class(
                        key.clone(),
                        class.to_string(),
                        serde_json::to_string(map).unwrap(),
                    ));
                }

                _ => {
                    warn!(
                        "Unknown attribute class of attribute with name '{}' and value {:?}. \
                    Skipping...",
                        key, value
                    );
                }
            }
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::{IOAttribute, IOAttributes};
    use crate::generated::general::AttributeValue;
    use crate::simulation::InternalAttributes;
    use prost::Message;

    #[test]
    fn person_vehicle_maps_survive_xml_and_protobuf() {
        let xml = r#"<attributes>
            <attribute name="vehicles" class="org.matsim.vehicles.PersonVehicles">{"truck":"fleet-17","car":"p1_car"}</attribute>
            <attribute name="vehicleTypes" class="org.matsim.vehicles.PersonVehicleTypes">{"truck":"light8t"}</attribute>
            <attribute name="label" class="java.lang.String">unchanged</attribute>
        </attributes>"#;
        let input: IOAttributes = quick_xml::de::from_str(xml).unwrap();
        let attributes = InternalAttributes::from(input.clone());
        assert_eq!(
            Some("fleet-17"),
            attributes.string_map_entry("vehicles", "truck")
        );
        assert_eq!(
            Some("light8t"),
            attributes.string_map_entry("vehicleTypes", "truck")
        );
        let wire = attributes
            .as_cloned_map()
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    AttributeValue::decode(value.encode_to_vec().as_slice()).unwrap(),
                )
            })
            .collect();
        let restored = InternalAttributes::from(&wire);
        assert_eq!(attributes, restored);
        let output = IOAttributes::from(&restored);
        let written = quick_xml::se::to_string(&output).unwrap();
        let parsed: IOAttributes = quick_xml::de::from_str(&written).unwrap();
        assert_eq!(attributes, InternalAttributes::from(parsed));
        assert_eq!(
            "org.matsim.vehicles.PersonVehicles",
            output
                .attributes
                .iter()
                .find(|a| a.name == "vehicles")
                .unwrap()
                .class
        );
        // A scalar encoded with the old wire format is still understood.
        let scalar = AttributeValue::decode([0x12, 0x03, b'o', b'l', b'd'].as_slice()).unwrap();
        assert_eq!("old", scalar.as_string());
    }

    #[test]
    fn empty_vehicle_map_remains_present() {
        let input = IOAttributes {
            attributes: vec![IOAttribute::new_with_class(
                "vehicles".into(),
                "org.matsim.vehicles.PersonVehicles".into(),
                "{}".into(),
            )],
        };
        let attributes = InternalAttributes::from(input);
        assert_eq!(
            Some(std::collections::BTreeMap::<String, String>::new()),
            attributes.get("vehicles")
        );
        assert_eq!(
            "{}",
            IOAttributes::from(&attributes).find("vehicles").unwrap()
        );
        assert_eq!(
            attributes,
            InternalAttributes::from(&attributes.as_cloned_map())
        );
    }

    #[test]
    #[should_panic(expected = "Invalid string map in attribute vehicleTypes")]
    fn rejects_non_string_vehicle_type() {
        let _ = InternalAttributes::from(IOAttributes {
            attributes: vec![IOAttribute::new_with_class(
                "vehicleTypes".into(),
                "org.matsim.vehicles.PersonVehicleTypes".into(),
                r#"{"truck":17}"#.into(),
            )],
        });
    }

    #[test]
    #[should_panic(expected = "Invalid string map in attribute vehicles")]
    fn rejects_malformed_vehicle_json() {
        let _ = InternalAttributes::from(IOAttributes {
            attributes: vec![IOAttribute::new_with_class(
                "vehicles".into(),
                "org.matsim.vehicles.PersonVehicles".into(),
                "{".into(),
            )],
        });
    }
}
