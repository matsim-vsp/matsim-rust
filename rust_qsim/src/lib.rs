extern crate core;

// this module is used to tinker with rust problems in a simple fashion
#[allow(dead_code, clippy::all)]
pub mod experiments;
pub mod simulation;

pub mod external_services;
pub mod generated;
#[cfg(test)]
mod test_utils;
pub mod utilities;
