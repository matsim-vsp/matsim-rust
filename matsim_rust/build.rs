extern crate prost_build;
extern crate protoc_bin_vendored;

use prost_build::Config;
use std::path::Path;

fn main() {
    let proto_files = [
        "src/simulation/io/proto/types/general.proto",
        "src/simulation/io/proto/types/events.proto",
        "src/simulation/io/proto/types/facilities.proto",
        "src/simulation/io/proto/types/ids.proto",
        "src/simulation/io/proto/types/network.proto",
        "src/simulation/io/proto/types/population.proto",
        "src/simulation/io/proto/types/transit.proto",
        "src/simulation/io/proto/types/vehicles.proto",
        "src/external_services/routing/routing.proto",
    ];

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
    let protoc_include = protoc_bin_vendored::include_path()
        .expect("The bundled protoc compiler's standard .proto include directory is unavailable");

    tonic_build::configure()
        .build_client(true)
        .compile_protos_with_config(config, &proto_files, &[Path::new("src/"), &protoc_include])
        .unwrap();
}
