//! Transit vehicle simulation: scheduled vehicles driven through the network, and passengers
//! boarding and alighting at stops. Enabled by `transit.simulate_vehicles`; otherwise PT legs
//! are teleported.

pub(crate) mod doors;
pub mod driver;
pub mod runs;
pub(crate) mod stops;
