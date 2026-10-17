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
use std::collections::{BTreeMap, HashMap};
use std::fmt::Debug;
use tracing::warn;

pub mod agents;
pub mod analysis;
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
    // we are using serde_json::Value to allow for flexible attribute types and serializability.
    // Stored in a BTreeMap to guarantee deterministic, ascending-by-attribute-name iteration order.
    attributes: BTreeMap<String, Value>,
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

    pub fn iter(&self) -> std::collections::btree_map::Iter<'_, String, Value> {
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

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.attributes.remove(key)
    }

    pub fn len(&self) -> usize {
        self.attributes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.attributes.is_empty()
    }

    pub fn contains_key(&self, key: &str) -> bool {
        self.attributes.contains_key(key)
    }
}

impl<'a> IntoIterator for &'a InternalAttributes {
    type Item = (&'a String, &'a Value);
    type IntoIter = std::collections::btree_map::Iter<'a, String, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.attributes.iter()
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

impl<T: Serialize> From<BTreeMap<String, T>> for InternalAttributes {
    fn from(value: BTreeMap<String, T>) -> Self {
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

impl From<&BTreeMap<String, AttributeValue>> for InternalAttributes {
    fn from(value: &BTreeMap<String, AttributeValue>) -> Self {
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
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap};

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

    #[test]
    fn ordering_different_insertion_orders_produce_identical_serialization() {
        let mut attrs1 = InternalAttributes::default();
        attrs1.insert("zebra", "last");
        attrs1.insert("apple", 1);
        attrs1.insert("mango", 2.5);
        attrs1.insert("banana", true);

        let mut attrs2 = InternalAttributes::default();
        attrs2.insert("banana", true);
        attrs2.insert("mango", 2.5);
        attrs2.insert("zebra", "last");
        attrs2.insert("apple", 1);

        let mut attrs3 = InternalAttributes::default();
        attrs3.insert("apple", 1);
        attrs3.insert("banana", true);
        attrs3.insert("mango", 2.5);
        attrs3.insert("zebra", "last");

        // Serde JSON serialization matches byte for byte regardless of insertion order
        let json1 = serde_json::to_string(&attrs1).unwrap();
        let json2 = serde_json::to_string(&attrs2).unwrap();
        let json3 = serde_json::to_string(&attrs3).unwrap();
        assert_eq!(json1, json2);
        assert_eq!(json2, json3);

        // Iteration yields keys in ascending alphabetical order
        let keys: Vec<&String> = attrs1.iter().map(|(k, _)| k).collect();
        assert_eq!(keys, vec!["apple", "banana", "mango", "zebra"]);

        // IOAttributes conversion preserves ascending order and identical content
        let io1 = IOAttributes::from(&attrs1);
        let io2 = IOAttributes::from(&attrs2);
        let io3 = IOAttributes::from(&attrs3);
        assert_eq!(io1, io2);
        assert_eq!(io2, io3);
        let io_names: Vec<&str> = io1.attributes.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(io_names, vec!["apple", "banana", "mango", "zebra"]);
    }

    #[test]
    fn determinism_serializing_same_entries_yields_identical_bytes() {
        let entries = [
            ("score", json!(10.5)),
            ("mode", json!("car")),
            ("active", json!(true)),
            ("capacity", json!(4)),
        ];

        let mut a = InternalAttributes::default();
        for (k, v) in &entries {
            a.insert(*k, v);
        }

        let mut b = InternalAttributes::default();
        for (k, v) in entries.iter().rev() {
            b.insert(*k, v);
        }

        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
        assert_eq!(IOAttributes::from(&a), IOAttributes::from(&b));
    }

    #[test]
    fn independence_adding_new_attribute_preserves_relative_order() {
        let mut attrs = InternalAttributes::default();
        attrs.insert("charlie", 3);
        attrs.insert("echo", 5);

        let initial_keys: Vec<&String> = attrs.iter().map(|(k, _)| k).collect();
        assert_eq!(initial_keys, vec!["charlie", "echo"]);

        attrs.insert("alpha", 1);
        attrs.insert("bravo", 2);
        attrs.insert("delta", 4);
        attrs.insert("foxtrot", 6);

        let final_keys: Vec<&String> = attrs.iter().map(|(k, _)| k).collect();
        assert_eq!(
            final_keys,
            vec!["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"]
        );

        // Subsequence invariant: charlie still precedes echo
        let pos_charlie = final_keys.iter().position(|&k| k == "charlie").unwrap();
        let pos_echo = final_keys.iter().position(|&k| k == "echo").unwrap();
        assert!(pos_charlie < pos_echo);
    }

    #[test]
    fn preservation_lookup_insertion_defaults_clone_and_equality() {
        let default_attrs = InternalAttributes::default();
        assert!(default_attrs.is_empty());
        assert_eq!(default_attrs.len(), 0);

        let mut attrs = InternalAttributes::default();
        attrs.insert("count", 42i64);
        attrs.insert("name", "test");
        attrs.insert("weight", 12.34f64);
        attrs.insert("flag", true);

        // Lookup by key
        assert_eq!(attrs.get::<i64>("count"), Some(42));
        assert_eq!(attrs.get::<String>("name"), Some("test".to_string()));
        assert_eq!(attrs.get::<f64>("weight"), Some(12.34));
        assert_eq!(attrs.get::<bool>("flag"), Some(true));
        assert_eq!(attrs.get_bool("flag"), Ok(Some(true)));
        assert_eq!(attrs.get::<i64>("nonexistent"), None);
        assert!(attrs.contains_key("count"));
        assert!(!attrs.contains_key("missing"));

        // Clone
        let mut cloned = attrs.clone();
        assert_eq!(attrs, cloned);

        // Content-based equality regardless of insertion order
        let mut different_order = InternalAttributes::default();
        different_order.insert("flag", true);
        different_order.insert("weight", 12.34f64);
        different_order.insert("name", "test");
        different_order.insert("count", 42i64);
        assert_eq!(attrs, different_order);

        // Removal
        assert_eq!(cloned.remove("name"), Some(serde_json::json!("test")));
        assert_eq!(cloned.get::<String>("name"), None);
        assert_ne!(attrs, cloned);
        assert_eq!(cloned.len(), 3);

        // BTreeMap From conversions
        let mut btree = BTreeMap::new();
        btree.insert("k".to_string(), "v".to_string());
        let from_btree = InternalAttributes::from(btree);
        assert_eq!(from_btree.get::<String>("k"), Some("v".to_string()));
    }

    #[test]
    fn non_plan_consumer_ordering_link_overrides_and_facilities() {
        // Link attribute overrides
        let mut link_attrs = InternalAttributes::default();
        link_attrs.insert("capacity_factor", 1.5);
        link_attrs.insert("allowed_modes", "car,bus");
        link_attrs.insert("toll", 100);

        let io_link_attrs = IOAttributes::from(&link_attrs);
        let names: Vec<&str> = io_link_attrs
            .attributes
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, vec!["allowed_modes", "capacity_factor", "toll"]);

        // Facilities
        let mut fac_attrs = InternalAttributes::default();
        fac_attrs.insert("opening_hours", "08:00-20:00");
        fac_attrs.insert("capacity", 50);
        fac_attrs.insert("accessible", true);

        let io_fac_attrs = IOAttributes::from(&fac_attrs);
        let fac_names: Vec<&str> = io_fac_attrs
            .attributes
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(fac_names, vec!["accessible", "capacity", "opening_hours"]);
    }
}
