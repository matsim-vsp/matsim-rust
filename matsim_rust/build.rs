extern crate prost_build;
extern crate protoc_bin_vendored;

use prost_build::Config;

/// Compiles ids.proto of this crate. The shared message types (population, network, events, ...)
/// and the client of the routing service API come generated from matsim-schemas.
fn main() {
    let proto_files = ["src/simulation/io/proto/types/ids.proto"];

    // tell cargo to rerun this build script if any of the proto files change
    for proto in &proto_files {
        println!("cargo:rerun-if-changed={}", proto);
    }

    // Compiling the protobuf files
    let mut config = Config::new();
    // We use a prebuilt protoc compiler. This line makes it available to prost-build
    // without requiring a system installation or compiling protobuf's C++ sources.
    config.protoc_executable(
        protoc_bin_vendored::protoc_bin_path()
            .expect("No bundled protoc compiler is available for this build platform"),
    );

    config.compile_protos(&proto_files, &["src/"]).unwrap();
}
