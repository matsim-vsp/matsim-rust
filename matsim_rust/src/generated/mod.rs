use crate::generated::general::AttributeValue;
use crate::generated::general::attribute_value::Type;
use crate::simulation::io::xml::attributes::IOAttribute;
use prost::Message;
use serde::ser::Error;
use serde::{Serialize, Serializer};
use std::fs;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, Write};
use std::marker::PhantomData;
use std::path::Path;
use tracing::info;

// Include the `messages` module, which is generated from messages.proto
pub mod events {
    include!(concat!(env!("OUT_DIR"), "/events.rs"));
}
pub mod facilities {
    include!(concat!(env!("OUT_DIR"), "/facilities.rs"));
}
pub mod ids {
    include!(concat!(env!("OUT_DIR"), "/ids.rs"));
}

pub mod network {
    include!(concat!(env!("OUT_DIR"), "/network.rs"));
}
pub mod population {
    include!(concat!(env!("OUT_DIR"), "/population.rs"));
}
pub mod transit {
    include!(concat!(env!("OUT_DIR"), "/transit.rs"));
}
pub mod vehicles {
    include!(concat!(env!("OUT_DIR"), "/vehicles.rs"));
}

pub mod general {
    include!(concat!(env!("OUT_DIR"), "/general.rs"));
}

pub mod routing {
    include!(concat!(env!("OUT_DIR"), "/routing.rs"));
}

pub fn read_from_file<T: Message + Default>(path: &Path) -> T {
    info!("Loading message from file at: {path:?}");
    let mut reader = File::open(path).unwrap_or_else(|_| panic!("Could not open File at {path:?}"));

    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .unwrap_or_else(|_| panic!("Could not read File at {path:?}"));
    let wire_type = T::decode(bytes.as_slice()).expect("Failed to decode file contents");

    info!("Finished loading message from file at: {path:?}");
    wire_type
}

pub fn write_to_file<T: Message>(message: T, path: &Path) {
    info!("Starting to write message to file: {path:?}");
    let bytes = message.encode_to_vec();

    // Create the file and all necessary directories
    // this doesn't cover some edge cases, but this will do for now
    //let path = Path::new(file_path);
    let prefix = path.parent().unwrap();
    fs::create_dir_all(prefix).unwrap();
    let mut file =
        File::create(path).unwrap_or_else(|_| panic!("Failed to create file at: {path:?}"));
    file.write_all(&bytes)
        .unwrap_or_else(|_| panic!("Failed to write bytes to file at: {path:?}"));
    info!("Finished writing message to file: {path:?}");
}

pub fn read_delimiter<R>(reader: &mut BufReader<R>) -> Option<usize>
where
    R: Read + Seek,
{
    // read the delimiter of the message. Prost says delimiter is between 1 and 10 bytes
    // so, read the first 10 bytes of the buffer
    let mut delim_buffer: [u8; 10] = [0; 10];
    // this could crash
    match reader.read_exact(&mut delim_buffer) {
        Ok(_) => {} // go on.
        Err(e) => match e.kind() {
            ErrorKind::UnexpectedEof => return None,
            _ => {
                panic!("Error while reading file: {}", e);
            }
        },
    };

    let delimiter =
        prost::decode_length_delimiter(delim_buffer.as_slice()).expect("error reading delimiter");

    // since the delimiter is a varint figure out how many bytes the delimiter was actually taking
    // up in the buffer. Set the buffers position to the first byte after the delimiter, which
    // should be the start of the TimeStep message
    let delim_encoded_len = prost::encoding::encoded_len_varint(delimiter as u64) as i64;
    let offset = delim_encoded_len - (delim_buffer.len() as i64);
    reader
        .seek_relative(offset)
        .expect("Seeking relative failed");

    Some(delimiter)
}

pub struct MessageIter<T, R>
where
    T: Message + Default,
    R: Read + Seek,
{
    type_marker: PhantomData<T>,
    internal_reader: BufReader<R>,
}

impl<T, R> Iterator for MessageIter<T, R>
where
    T: Message + Default,
    R: Read + Seek,
{
    type Item = T;

    fn next(&mut self) -> Option<Self::Item> {
        if let Some(delimiter) = read_delimiter(&mut self.internal_reader) {
            let mut bytes: Vec<u8> = vec![0; delimiter];
            self.internal_reader
                .read_exact(&mut bytes)
                .expect("Failed to read exact from buffer");
            let message = T::decode(bytes.as_slice()).expect("Failed to decode message");
            Some(message)
        } else {
            None
        }
    }
}

impl<T, R> MessageIter<T, R>
where
    T: Message + Default,
    R: Read + Seek,
{
    pub fn new(reader: R) -> Self {
        Self {
            type_marker: Default::default(),
            internal_reader: BufReader::new(reader),
        }
    }
}

impl AttributeValue {
    pub(crate) fn new_int(value: i64) -> Self {
        AttributeValue {
            r#type: Some(Type::IntValue(value)),
        }
    }

    pub(crate) fn new_string(value: String) -> Self {
        AttributeValue {
            r#type: Some(Type::StringValue(value)),
        }
    }

    pub(crate) fn new_double(value: f64) -> Self {
        AttributeValue {
            r#type: Some(Type::DoubleValue(value)),
        }
    }

    pub(crate) fn new_bool(value: bool) -> Self {
        AttributeValue {
            r#type: Some(Type::BoolValue(value)),
        }
    }

    pub fn as_int(&self) -> i64 {
        match self.r#type.as_ref().unwrap() {
            Type::IntValue(value) => *value,
            _ => panic!("Expected int, got {:?}", self),
        }
    }

    pub fn as_int_opt(&self) -> Option<i64> {
        match self.r#type.as_ref().unwrap() {
            Type::IntValue(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_string(&self) -> String {
        match self.r#type.as_ref().unwrap() {
            Type::StringValue(value) => value.clone(),
            _ => panic!("Expected string, got {:?}", self),
        }
    }

    pub fn as_string_opt(&self) -> Option<String> {
        match self.r#type.as_ref().unwrap() {
            Type::StringValue(value) => Some(value.clone()),
            _ => None,
        }
    }

    pub fn as_double(&self) -> f64 {
        match self.r#type.as_ref().unwrap() {
            Type::DoubleValue(value) => *value,
            _ => panic!("Expected double, got {:?}", self),
        }
    }

    pub fn as_double_opt(&self) -> Option<f64> {
        match self.r#type.as_ref().unwrap() {
            Type::DoubleValue(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> bool {
        match self.r#type.as_ref().unwrap() {
            Type::BoolValue(value) => *value,
            _ => panic!("Expected bool, got {:?}", self),
        }
    }

    pub fn as_bool_opt(&self) -> Option<bool> {
        match self.r#type.as_ref().unwrap() {
            Type::BoolValue(value) => Some(*value),
            _ => None,
        }
    }

    pub fn from_io_attr(attr: IOAttribute) -> AttributeValue {
        match attr.class.as_str() {
            "java.lang.String" => AttributeValue::new_string(attr.value),
            "java.lang.Double" => AttributeValue::new_double(attr.value.parse().unwrap()),
            "java.lang.Integer" => AttributeValue::new_int(attr.value.parse().unwrap()),
            "java.lang.Boolean" => AttributeValue::new_bool(attr.value.parse().unwrap()),
            _ => panic!("Unsupported attribute class: {}", attr.class),
        }
    }
}

impl From<bool> for AttributeValue {
    fn from(value: bool) -> Self {
        AttributeValue::new_bool(value)
    }
}

impl From<String> for AttributeValue {
    fn from(value: String) -> Self {
        AttributeValue::new_string(value)
    }
}

impl From<&str> for AttributeValue {
    fn from(value: &str) -> Self {
        AttributeValue::new_string(value.to_string())
    }
}

impl From<f64> for AttributeValue {
    fn from(value: f64) -> Self {
        AttributeValue::new_double(value)
    }
}

// we can't tag the enum as non-exhaustive because prost generates it. This is why the warning is manually disabled.
#[allow(unreachable_patterns)]
impl Serialize for AttributeValue {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self.r#type.as_ref().unwrap() {
            Type::IntValue(i) => serializer.serialize_i64(*i),
            Type::StringValue(s) => serializer.serialize_str(s),
            Type::DoubleValue(d) => serializer.serialize_f64(*d),
            Type::BoolValue(b) => serializer.serialize_bool(*b),
            _ => Err(S::Error::custom("Unsupported type")),
        }
    }
}
