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
pub mod build_info;
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
pub mod queue;
pub mod random;
pub mod replanning;
pub mod scenario;
pub mod scoring;
#[allow(clippy::module_inception)]
pub mod simulation;
pub mod time;
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

    /// Borrows one entry of a string-map attribute without cloning the map.
    pub fn string_map_entry(&self, key: &str, entry: &str) -> Option<&str> {
        self.attributes.get(key)?.as_object()?.get(entry)?.as_str()
    }

    pub(crate) fn insert_string_map_entry(
        &mut self,
        key: &str,
        entry: impl Into<String>,
        value: impl Into<String>,
    ) {
        let map = self
            .attributes
            .entry(key.to_string())
            .or_insert_with(|| json!({}));
        map.as_object_mut()
            .unwrap_or_else(|| panic!("Attribute {key} must be a string map"))
            .insert(entry.into(), Value::String(value.into()));
    }

    /// Merges assignments in place, creating a missing map even when `entries` is empty.
    pub(crate) fn merge_string_map_entries(
        &mut self,
        key: &str,
        entries: impl IntoIterator<Item = (String, String)>,
    ) {
        let map = self
            .attributes
            .entry(key.to_string())
            .or_insert_with(|| json!({}));
        assert!(map.is_object(), "Attribute {key} must be a string map");
        for (entry, value) in entries {
            self.insert_string_map_entry(key, entry, value);
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
                Value::Object(map) if map.values().all(Value::is_string) => {
                    let entries = map
                        .iter()
                        .map(|(entry, value)| {
                            (
                                entry.clone(),
                                value.as_str().expect("validated string").to_string(),
                            )
                        })
                        .collect();
                    AttributeValue::new_string_map(entries)
                }
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
                "org.matsim.vehicles.PersonVehicles" | "org.matsim.vehicles.PersonVehicleTypes" => {
                    let map: std::collections::BTreeMap<String, String> =
                        serde_json::from_str(&attr.value).unwrap_or_else(|err| {
                            panic!(
                                "Invalid string map in attribute {} ({}): {err}",
                                attr.name, attr.class
                            )
                        });
                    res.insert(attr.name, map);
                }
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
                Type::StringMapValue(map) => {
                    res.insert(key, &map.entries);
                }
            };
        }
        res
    }
}
