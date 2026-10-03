extern crate prost_build;
extern crate protobuf_src;

use prost_build::Config;

fn main() {
    let proto_files = [
        "src/simulation/io/proto/types/general.proto",
        "src/simulation/io/proto/types/events.proto",
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
    // we use the protobuf-src which provides the protoc compiler. This line makes it available
    // to prost-build
    config.protoc_executable(protobuf_src::protoc());

    tonic_build::configure()
        .build_client(true)
        .compile_protos_with_config(config, &proto_files, &["src/"])
        .expect("Should be able to compile protobuf files.");
}
