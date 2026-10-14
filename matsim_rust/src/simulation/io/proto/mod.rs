use std::io::{BufRead, ErrorKind};

pub mod proto_events;
pub mod proto_facilities;
pub mod proto_network;
pub mod proto_population;
pub mod proto_transit;
pub mod proto_vehicles;

/// Appends the next length delimited message to `buffer`. Returns `false` at the end of the input.
pub(crate) fn read_length_delimited(reader: &mut impl BufRead, buffer: &mut Vec<u8>) -> bool {
    let Some(length) = next_delimiter_length(reader) else {
        return false;
    };
    let start = buffer.len();
    buffer.resize(start + length, 0);
    reader
        .read_exact(&mut buffer[start..])
        .expect("Failed to read delimited buffer.");
    true
}

/// Reads the varint length prefix of the next message. Returns `None` at the end of the input.
pub(crate) fn next_delimiter_length(reader: &mut impl BufRead) -> Option<usize> {
    let mut value: u64 = 0;
    // A varint encoding of a u64 has at most 10 bytes with 7 payload bits each.
    for index in 0..10 {
        let mut byte = [0; 1];
        if let Err(e) = reader.read_exact(&mut byte) {
            if e.kind() == ErrorKind::UnexpectedEof && index == 0 {
                return None;
            }
            panic!("Error while reading length delimiter: {e}");
        }
        value |= u64::from(byte[0] & 0x7f) << (7 * index);
        if byte[0] < 0x80 {
            return Some(usize::try_from(value).expect("Length delimiter exceeds usize."));
        }
    }
    panic!("Invalid length delimiter: varint is longer than 10 bytes.");
}
