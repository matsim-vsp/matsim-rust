use crate::generated::general::AttributeValue;
use crate::generated::general::attribute_value::Type;
use crate::simulation::id::Id;
use crate::simulation::id::serializable_type::StableTypeId;
use crate::simulation::scenario::Coordinate;
use crate::simulation::scenario::network::Link;
use io::xml::attributes::IOAttributes;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::fmt::Debug;
use tracing::warn;

pub mod agents;
pub mod analysis;
#[allow(deprecated)]
pub mod config;
pub mod controller;
pub mod data_structures;
pub mod engines;
pub mod events;
pub mod framework_events;
pub mod id;
pub mod io;
pub mod logging;
pub mod messaging;
pub mod network;
pub mod population;
pub mod profiling;
pub mod pt;
pub mod random;
pub mod replanning;
pub mod scenario;
pub mod scoring;
#[allow(clippy::module_inception)]
pub mod simulation;
pub mod time;
pub mod time_queue;
pub mod vehicles;

pub trait Identifiable<I: StableTypeId> {
    fn id(&self) -> &Id<I>;
}

pub trait CoordinateLocation {
    fn coordinate(&self) -> &Coordinate;
}

pub trait LinkLocation {
    fn link_id(&self) -> &Id<Link>;
}

pub trait Attributable {
    fn attributes(&self) -> &InternalAttributes;
    fn attributes_mut(&mut self) -> &mut InternalAttributes;
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, Clone)]
pub struct InternalAttributes {
    // we are using serde_json::Value to allow for flexible attribute types and serializability
    attributes: HashMap<String, Value>,
}

impl InternalAttributes {
    pub fn insert<T: Serialize>(&mut self, key: impl Into<String>, value: T) {
        self.attributes.insert(key.into(), json!(value));
    }

    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        self.attributes
            .get(key)
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }

    /// Reads a boolean attribute, telling a missing value apart from a malformed one.
    ///
    /// [`Self::get`] cannot: it answers `None` for both, so a policy that must not read a
    /// broken value as "no" has no way to notice it. A string counts as a boolean when
    /// `str::parse::<bool>` accepts it, which is the same rule [`From<IOAttributes>`] applies
    /// to `class="java.lang.Boolean"`; accepting it therefore keeps XML and protobuf inputs
    /// agreeing on one logical value. A malformed `java.lang.Boolean` never gets this far:
    /// the XML conversion already rejects it.
    pub fn get_bool(&self, key: &str) -> Result<Option<bool>, String> {
        match self.attributes.get(key) {
            None => Ok(None),
            Some(Value::Bool(value)) => Ok(Some(*value)),
            Some(Value::String(value)) => value
                .parse::<bool>()
                .map(Some)
                .map_err(|_| format!("`{value}` is not a boolean")),
            Some(value) => Err(format!("{value} is not a boolean")),
        }
    }

    pub fn iter(&self) -> std::collections::hash_map::Iter<'_, String, Value> {
        self.attributes.iter()
    }

    pub fn as_cloned_map(&self) -> HashMap<String, AttributeValue> {
        let mut attributes = HashMap::new();
        for (key, value) in self.iter() {
            let insert = match value {
                Value::Bool(b) => AttributeValue::new_bool(*b),
                Value::Number(n) => {
                    if n.is_i64() {
                        AttributeValue::new_int(n.as_i64().unwrap())
                    } else if n.is_f64() {
                        AttributeValue::new_double(n.as_f64().unwrap())
                    } else {
                        warn!("Unsupported number type for key '{}': {:?}", key, n);
                        continue;
                    }
                }
                Value::String(s) => AttributeValue::new_string(s.clone()),
                _ => {
                    warn!("Unsupported attribute type for key '{}': {:?}", key, value);
                    continue;
                }
            };
            attributes.insert(key.clone(), insert);
        }
        attributes
    }

    pub fn add(&mut self, key: impl Into<String>, value: impl Serialize) {
        self.attributes
            .insert(key.into(), serde_json::to_value(value).unwrap());
    }
}

impl From<IOAttributes> for InternalAttributes {
    fn from(attrs: IOAttributes) -> Self {
        let mut res = InternalAttributes::default();
        for attr in attrs.attributes {
            match attr.class.as_str() {
                "java.lang.Integer" => res.insert(attr.name, attr.value.parse::<i32>().unwrap()),
                "java.lang.Long" => res.insert(attr.name, attr.value.parse::<i64>().unwrap()),
                "java.lang.Double" => res.insert(attr.name, attr.value.parse::<f64>().unwrap()),
                "java.lang.String" => res.insert(attr.name, attr.value),
                "java.lang.Boolean" => res.insert(attr.name, attr.value.parse::<bool>().unwrap()),
                _ => {} //warn!("Unknown attribute class {:?}. Skipping...", attr.class),
            };
        }
        res
    }
}

impl<T: Serialize> From<HashMap<String, T>> for InternalAttributes {
    fn from(value: HashMap<String, T>) -> Self {
        let mut res = InternalAttributes::default();
        for (key, value) in value {
            res.insert(key, value);
        }
        res
    }
}

impl From<&HashMap<String, AttributeValue>> for InternalAttributes {
    fn from(value: &HashMap<String, AttributeValue>) -> Self {
        let mut res = InternalAttributes::default();
        for (key, value) in value {
            match value.r#type.as_ref().unwrap() {
                Type::IntValue(i) => {
                    res.insert(key, i);
                }
                Type::StringValue(s) => {
                    res.insert(key, s);
                }
                Type::DoubleValue(d) => {
                    res.insert(key, d);
                }
                Type::BoolValue(b) => {
                    res.insert(key, b);
                }
            };
        }
        res
    }
}

#[cfg(test)]
mod tests {
    use super::{AttributeValue, InternalAttributes};
    use crate::simulation::io::xml::attributes::{IOAttribute, IOAttributes};
    use std::collections::HashMap;

    /// XML and protobuf serialize the same attribute value differently, so the boolean reader
    /// has to agree on both: one as a typed boolean, one as the text MATSim writes.
    #[test]
    fn a_boolean_attribute_reads_the_same_from_xml_and_protobuf() {
        let from_xml = |value: &str, class: &str| {
            InternalAttributes::from(IOAttributes {
                attributes: vec![IOAttribute::new_with_class(
                    "ownsCar".to_string(),
                    class.to_string(),
                    value.to_string(),
                )],
            })
        };
        let from_protobuf = |value: AttributeValue| {
            let attributes = HashMap::from([("ownsCar".to_string(), value)]);
            InternalAttributes::from(&attributes)
        };

        for (xml, protobuf) in [
            (
                from_xml("true", "java.lang.Boolean"),
                from_protobuf(AttributeValue::new_bool(true)),
            ),
            (
                from_xml("false", "java.lang.Boolean"),
                from_protobuf(AttributeValue::new_bool(false)),
            ),
            // A value declared as text in one format and as a boolean in the other is still the
            // same value, so the gate must not depend on how the input spelled it.
            (
                from_xml("true", "java.lang.String"),
                from_protobuf(AttributeValue::new_string("true".to_string())),
            ),
        ] {
            assert_eq!(xml.get_bool("ownsCar"), protobuf.get_bool("ownsCar"));
        }
    }

    /// Missing is not malformed: an absent attribute denies a policy but is not an input error.
    #[test]
    fn a_missing_boolean_is_absent_rather_than_malformed() {
        let attributes = InternalAttributes::default();
        assert_eq!(attributes.get_bool("ownsCar"), Ok(None));
    }

    #[test]
    fn a_malformed_boolean_is_reported() {
        for attributes in [
            {
                let mut attributes = InternalAttributes::default();
                attributes.insert("ownsCar", "yes");
                attributes
            },
            InternalAttributes::from(HashMap::from([(
                "ownsCar".to_string(),
                AttributeValue::new_int(1),
            )])),
        ] {
            assert!(attributes.get_bool("ownsCar").is_err(), "{attributes:?}");
        }
    }
}
