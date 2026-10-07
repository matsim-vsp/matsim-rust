use crate::simulation::InternalAttributes;
use crate::simulation::config;
use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network};
use nohash_hasher::IntMap;

use super::STORAGE_CAPACITY_USED_IN_QSIM;

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StorageCapacityDefinition {
    qsim_override: Option<f64>,
    original: f64,
}

impl StorageCapacityDefinition {
    pub(crate) fn build(
        length: f64,
        perm_lanes: f64,
        capacity_h: f64,
        sample_size: f64,
        effective_cell_size: f64,
        free_speed: f64,
    ) -> Self {
        let flow_cap_s = capacity_h * sample_size / 3600.;
        let cap = length * perm_lanes * sample_size / effective_cell_size;

        // set storage capacity to at least flow capacity. Otherwise, the flow capacity would never be fully used during `move_node`.
        let max_storage_cap = flow_cap_s.max(cap);

        let freespeed_travel_time = length / free_speed;
        let temp_storage_cap = freespeed_travel_time * flow_cap_s;

        // set storage capacity to at least the freespeed travel time * flow capacity. Otherwise, the flow capacity would never be fully used,
        // because vehicles would need at least need freespeed travel time to reach the end of the link.
        let adjusted = (max_storage_cap < temp_storage_cap).then_some(temp_storage_cap);

        Self {
            qsim_override: adjusted,
            original: max_storage_cap,
        }
    }

    pub(crate) fn qsim_override(&self) -> Option<f64> {
        self.qsim_override
    }

    #[allow(dead_code)]
    pub(crate) fn original(&self) -> f64 {
        self.original
    }

    pub(crate) fn max(&self) -> f64 {
        self.qsim_override.unwrap_or(self.original)
    }
}

#[derive(Debug, Clone)]
pub struct LinkStorageCapacities {
    capacities: IntMap<Id<Link>, StorageCapacityDefinition>,
}

impl LinkStorageCapacities {
    pub fn from_network(network: &Network, config: &config::QSim) -> Self {
        let effective_cell_size = network.effective_cell_size();
        let capacities = network
            .links()
            .into_iter()
            .map(|link| {
                let definition = StorageCapacityDefinition::build(
                    link.length,
                    link.permlanes,
                    link.capacity,
                    config.sample_size,
                    effective_cell_size,
                    link.freespeed,
                );
                (link.id.clone(), definition)
            })
            .collect();

        Self { capacities }
    }

    pub(crate) fn get(&self, link_id: &Id<Link>) -> &StorageCapacityDefinition {
        self.capacities
            .get(link_id)
            .unwrap_or_else(|| panic!("No storage-capacity definition found for link {link_id}"))
    }

    pub(crate) fn attribute_overrides(&self) -> IntMap<Id<Link>, InternalAttributes> {
        self.capacities
            .iter()
            .filter_map(|(link_id, definition)| {
                definition.qsim_override().map(|adjusted| {
                    let mut attributes = InternalAttributes::default();
                    attributes.insert(STORAGE_CAPACITY_USED_IN_QSIM, adjusted);
                    (link_id.clone(), attributes)
                })
            })
            .collect()
    }
}

/// StorageCap tracks changes in storage capacity for a link.
/// First of all it stores the maximum available storage capacity for a link.
/// Also, consumed and released capacity during a simulation time step is tracked
/// and can be queried separately. Once the time step is finished, the temporary
/// Bookkeeping can be applied to the overall consumed capacity by using the 'apply_updates'
/// method.
///
/// Consumed capacity can be queried immediately via 'currently_used', while released capacity
/// is treated separately. This is because we want vehicles which enter a link consume capacity
/// immediately, but capacity freed by vehicles leaving a link should only take effect in the next
/// simulation time step.
///
/// The consumed and released capacities are also tracked, so that we can figure out which
/// SplitInLinks must send storage capacity updates to upstream partitions. This logic
/// can be found in SimNetwork::move_links.
#[derive(Debug, Clone)]
pub struct StorageCap {
    max: f64,
    used: f64,
}

impl StorageCap {
    pub(crate) fn from_definition(definition: &StorageCapacityDefinition) -> Self {
        Self {
            max: definition.max(),
            used: 0.0,
        }
    }

    #[cfg(test)]
    pub fn build(
        length: f64,
        perm_lanes: f64,
        capacity_h: f64,
        sample_size: f64,
        effective_cell_size: f64,
        free_speed: f64,
    ) -> Self {
        let definition = StorageCapacityDefinition::build(
            length,
            perm_lanes,
            capacity_h,
            sample_size,
            effective_cell_size,
            free_speed,
        );
        Self::from_definition(&definition)
    }

    pub fn used(&self) -> f64 {
        self.used
    }

    #[cfg(test)]
    pub(crate) fn max(&self) -> f64 {
        self.max
    }

    /// Consumes storage capacity on a link
    ///
    /// This method should be called when a vehicle enters a link.
    ///
    /// # Parameters
    /// * 'value' storage capacity to be consumed
    pub fn consume(&mut self, value: f64) {
        self.used += value;
    }

    /// Releases storage capacity on a link
    ///
    /// This method should be called when a vehicle leaves a link
    pub fn release(&mut self, value: f64) {
        self.used -= value;
    }

    /// Tests whether there is storage capacity available on the link.
    pub fn is_available(&self) -> bool {
        let available_cap = self.max - self.used;
        available_cap > 0.0
    }
}

#[cfg(test)]
mod test {
    use crate::simulation::config;
    use crate::simulation::id::Id;
    use crate::simulation::network::STORAGE_CAPACITY_USED_IN_QSIM;
    use crate::simulation::network::storage_cap::{
        LinkStorageCapacities, StorageCap, StorageCapacityDefinition,
    };
    use crate::simulation::scenario::Coordinate;
    use crate::simulation::scenario::network::{Link, Network, Node};
    use macros::deterministic_id_test;

    #[test]
    fn storage_capacity_definition_without_adjustment() {
        let cap = StorageCap::build(100., 3., 1., 0.2, 7.5, 10.);
        assert_eq!(8., cap.max);
    }

    #[test]
    fn storage_capacity_definition_with_freespeed_adjustment() {
        let definition = StorageCapacityDefinition::build(100., 3., 360000., 0.2, 7.5, 10.);
        assert_eq!(Some(200.), definition.qsim_override());
        assert_eq!(20., definition.original());
    }

    #[test]
    fn storage_capacity_definition_does_not_adjust_equal_capacity() {
        let definition = StorageCapacityDefinition::build(100., 3., 360000., 0.2, 7.5, 100.);
        assert_eq!(None, definition.qsim_override());
        assert_eq!(20., definition.original());
    }

    #[deterministic_id_test]
    fn storage_capacity_catalog_maps_adjusted_capacity_to_link_attribute() {
        let mut network = Network::new();
        let from = Node::new(Id::create("from"), Coordinate::default(), 0, 1);
        let to = Node::new(Id::create("to"), Coordinate::default(), 0, 1);
        let link_id = Id::create("link");
        let mut link = Link::new_with_default(link_id.clone(), &from, &to);
        link.length = 100.;
        link.capacity = 360000.;
        link.freespeed = 10.;
        link.permlanes = 3.;
        network.add_node(from);
        network.add_node(to);
        network.add_link(link);

        let mut qsim = config::QSim::default();
        qsim.sample_size = 0.2;
        let capacities = LinkStorageCapacities::from_network(&network, &qsim);

        assert_eq!(Some(200.), capacities.get(&link_id).qsim_override());
        assert_eq!(
            Some(200.),
            capacities.attribute_overrides()[&link_id].get::<f64>(STORAGE_CAPACITY_USED_IN_QSIM)
        );
        assert_eq!(
            None,
            network
                .get_link(&link_id)
                .attributes
                .get::<f64>(STORAGE_CAPACITY_USED_IN_QSIM)
        );
    }
}
