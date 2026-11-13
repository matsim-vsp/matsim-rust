extern crate prost_build;
extern crate protoc_bin_vendored;

use prost_build::Config;
use std::path::PathBuf;

/// Compiles the .proto files of this crate. The shared message types (population, network, events,
/// ...) come from matsim-schemas: routing.proto imports them from there, and the generated code
/// refers to the Rust types of matsim-schemas instead of generating them again.
///
/// routing.proto lies in `proto/` at the repository root because the Java router (`java/`) is
/// generated from the same file.
fn main() {
    let proto_files = [
        PathBuf::from("src/simulation/io/proto/types/ids.proto"),
        PathBuf::from("../proto/routing.proto"),
    ];

    // tell cargo to rerun this build script if any of the proto files change
    for proto in &proto_files {
        println!("cargo:rerun-if-changed={}", proto.display());
    }

    // Compiling the protobuf files
    let mut config = Config::new();
    // We use a prebuilt protoc compiler. This line makes it available to prost-build
    // without requiring a system installation or compiling protobuf's C++ sources.
    config.protoc_executable(
        protoc_bin_vendored::protoc_bin_path()
            .expect("No bundled protoc compiler is available for this build platform"),
    );
    matsim_schemas::configure_extern_path(&mut config);

    let protoc_include = protoc_bin_vendored::include_path()
        .expect("The bundled protoc compiler's standard .proto include directory is unavailable");
    let include_dirs = [
        PathBuf::from("src/"),
        PathBuf::from("../proto/"),
        matsim_schemas::proto_dir(),
        protoc_include,
    ];

    tonic_prost_build::configure()
        .build_client(true)
        .compile_with_config(config, &proto_files, &include_dirs)
        .unwrap();
}
