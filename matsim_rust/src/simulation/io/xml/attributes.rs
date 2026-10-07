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
