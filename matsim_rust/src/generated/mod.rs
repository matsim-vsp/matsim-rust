use prost::Message;
use std::fs;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, Write};
use std::marker::PhantomData;
use std::path::Path;
use tracing::info;

// The message types shared across MATSim projects come from matsim-schemas.
pub use matsim_schemas::{events, facilities, general, network, population, transit, vehicles};

// Generated from the .proto files of this crate, see build.rs.
pub mod ids {
    include!(concat!(env!("OUT_DIR"), "/ids.rs"));
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
