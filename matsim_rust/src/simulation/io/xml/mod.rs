use std::fs;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::Path;

use flate2::Compression;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::info;

pub mod attributes;
pub mod events;
pub mod facilities;
pub mod network;
pub mod population;
pub mod transit;
pub mod vehicles;

use crate::simulation::io::is_url;
use flate2::read::GzDecoder;
use zstd::stream::read::Decoder as ZstdDecoder;
use zstd::stream::write::Encoder as ZstdEncoder;

pub fn read_from_file<T>(file_path: impl AsRef<Path>) -> T
where
    T: DeserializeOwned,
{
    use quick_xml::de::Deserializer;

    // Check if it's a URL or local file and if it's gzipped or not

    let compression = match file_path.as_ref().extension() {
        Some(ext) if ext == "gz" => {
            assert!(
                file_path
                    .as_ref()
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .ends_with("xml"),
                "File has .gz extension but the underlying file does not have .xml extension"
            );
            XmlCompression::Gz
        }
        Some(ext) if ext == "zst" => {
            assert!(
                file_path
                    .as_ref()
                    .file_stem()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .ends_with("xml"),
                "File has .zst extension but the underlying file does not have .xml extension"
            );
            XmlCompression::Zst
        }
        Some(_) => XmlCompression::None,
        _ => XmlCompression::None,
    };

    // Build one `BufRead` reader for all cases
    let reader: Box<dyn BufRead> = if is_url(file_path.as_ref()) {
        #[cfg(feature = "http")]
        {
            url_file_reader(file_path.as_ref().to_str().unwrap(), compression)
        }
        #[cfg(not(feature = "http"))]
        {
            panic!("Tried to read from URL, but feature http is not enabled");
        }
    } else {
        local_file_reader(file_path.as_ref(), compression)
    };

    // Parse the XML
    info!("Starting to read from file: {:?}", file_path.as_ref());
    let mut de = Deserializer::from_reader(reader);
    let res = match serde_path_to_error::deserialize(&mut de) {
        Ok(parsed) => parsed,
        Err(err) => {
            panic!("Failed to deserialize XML:\n{err:#?}");
        }
    };
    info!("Finished reading from file: {:?}", file_path.as_ref());
    res
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum XmlCompression {
    None,
    Gz,
    Zst,
}

fn local_file_reader(file_path: impl AsRef<Path>, compression: XmlCompression) -> Box<dyn BufRead> {
    // Local file path
    let file = File::open(file_path.as_ref()).unwrap_or_else(|_| {
        panic!(
            "xml_reader::read: Could not open file at {:?}",
            file_path.as_ref()
        )
    });

    match compression {
        XmlCompression::None => Box::new(BufReader::new(file)),
        XmlCompression::Gz => Box::new(BufReader::new(GzDecoder::new(file))),
        XmlCompression::Zst => Box::new(BufReader::new(
            ZstdDecoder::new(file).expect("Failed to create zstd decoder"),
        )),
    }
}

#[cfg(feature = "http")]
fn url_file_reader(file_path: &str, compression: XmlCompression) -> Box<dyn BufRead> {
    // URL path
    let resp = reqwest::blocking::get(file_path).expect("Could not fetch URL");
    let bytes = resp.bytes().expect("Could not read response body");

    match compression {
        XmlCompression::None => Box::new(BufReader::new(std::io::Cursor::new(bytes))),
        XmlCompression::Gz => Box::new(BufReader::new(GzDecoder::new(std::io::Cursor::new(bytes)))),
        XmlCompression::Zst => Box::new(BufReader::new(
            ZstdDecoder::new(std::io::Cursor::new(bytes)).expect("Failed to create zstd decoder"),
        )),
    }
}

// Adapter from std::fmt::Write to std::io::Write. Needed because of an odd API of quick-xml.
// See https://github.com/tafia/quick-xml/issues/499 for more details.
struct ToFmtWrite<T>(pub T);

impl<T> std::fmt::Write for ToFmtWrite<T>
where
    T: Write,
{
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write_all(s.as_bytes()).map_err(|_| std::fmt::Error)
    }
}

fn write_xml_payload<T: Serialize>(
    serde_message: &T,
    writer: &mut impl Write,
    dtd_spec: &str,
    include_xml_declaration: bool,
) {
    if include_xml_declaration {
        writer
            .write_all(b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n")
            .expect("Failed to write XML declaration");
    }

    writer
        .write_all(dtd_spec.as_bytes())
        .expect("Failed to write header");
    writer.write_all(b"\n\n").expect("Failed to write newlines");

    let mut fmt_writer = ToFmtWrite(writer);
    let mut serializer = quick_xml::se::Serializer::new(&mut fmt_writer);
    serializer.indent(' ', 4);
    serde_message
        .serialize(serializer)
        .expect("Failed to write message to file");
}

pub fn write_to_file<T: Serialize>(serde_message: &T, path: impl AsRef<Path>, dtd_spec: &str) {
    let prefix = path.as_ref().parent().unwrap();
    fs::create_dir_all(prefix).unwrap();
    let file = File::create(path.as_ref()).unwrap();
    let file_writer = BufWriter::new(file);

    info!("Starting to write file to: {:?}", path.as_ref());
    if path.as_ref().extension().unwrap().eq("gz") {
        let mut compressor = flate2::write::GzEncoder::new(file_writer, Compression::fast());
        write_xml_payload(serde_message, &mut compressor, dtd_spec, true);
        compressor.finish().expect("Failed to finish gz stream");
    } else if path.as_ref().extension().unwrap().eq("zst") {
        let mut compressor =
            ZstdEncoder::new(file_writer, 0).expect("Failed to create zstd encoder");
        write_xml_payload(serde_message, &mut compressor, dtd_spec, true);
        compressor.finish().expect("Failed to finish zstd stream");
    } else if path.as_ref().extension().unwrap().eq("xml") {
        let mut file_writer = file_writer;
        write_xml_payload(serde_message, &mut file_writer, dtd_spec, false);
    } else {
        panic!(
            "Tried to write {:?}. File format not supported. Either use `.xml`, `.xml.gz`, `.xml.zst`, or `.binpb` as extension",
            path.as_ref()
        );
    }
    info!("Finished writing file to: {:?}", path.as_ref());
}

#[cfg(test)]
mod tests {
    use crate::simulation::io::xml::read_from_file;

    // only testing the invalid case here, since the other cases
    // are implicitly tested when data containers are loaded e.g. in
    // network and population
    #[test]
    #[should_panic]
    fn unsupported_ending() {
        read_from_file("file-path-with-unsupported.ending")
    }
}
