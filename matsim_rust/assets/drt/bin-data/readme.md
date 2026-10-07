To create the `binpb` files, use this command:

```shell
cargo run --bin convert_to_binary -- --network assets/drt/grid_network.xml --population assets/drt/multi_mode_one_shared_taxi_population.xml --vehicles assets/drt/vehicles.xml --output-dir assets/drt/bin-data --run-id drt
```