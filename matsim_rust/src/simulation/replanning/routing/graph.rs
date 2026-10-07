use crate::simulation::id::Id;
use crate::simulation::scenario::network::{Link, Network, Node};
use nohash_hasher::IntMap;
use std::fmt;
use std::fmt::{Debug, Display};
use std::ops::Range;
use std::sync::Arc;

/// A (directed) graph whose nodes are Ids of network nodes, and edges are Ids of network links.
/// The graph is used for routing. The actual network nodes and links can be accessed by their id.
pub trait Graph: Debug + Send + Sync {
    /// get network node from node id
    fn node(&self, id: Id<Node>) -> Result<&Node, GraphError>;
    /// get network link from link id
    fn edge(&self, id: Id<Link>) -> Result<&Link, GraphError>;
    /// get slice of the outgoing edges, as link ids, of a given node, given as node id
    fn outgoing_edges(&self, node: Id<Node>) -> Result<&[Id<Link>], GraphError>;
    /// get slice of the incoming edges, as link ids, of a given node, given as node id
    fn incoming_edges(&self, node: Id<Node>) -> Result<&[Id<Link>], GraphError>;
    fn num_nodes(&self) -> usize;
    /// get the end node (head) of a given link, given as link id
    /// Returns an error if the link does not exist in the graph
    fn get_end_node(&self, link_id: Id<Link>) -> Result<Id<Node>, GraphError>;
    /// get the start node of a given link, given as link id
    /// Returns an error if the link does not exist in the graph
    fn get_start_node(&self, link_id: Id<Link>) -> Result<Id<Node>, GraphError>;
}

/// An index of a node in an `IndexableGraph`
pub type NodeIndex = usize;

/// An index of a link in an `IndexableGraph`
pub type LinkIndex = usize;

/// An index of a link in the backward graph of a `ForwardBackwardGraph`.
/// Only used internally, when indexing the backward graph to find the start node of some edge.
/// This separate type alias is used to emphasize that indices of links are not the same in the
/// forward and backward graphs.
pub(crate) type BackwardLinkIndex = usize;

/// A (directed) graph where nodes and links can be accessed by both their id and their index.
/// Mirrors many trait methods from its supertrait "Graph", but using indices instead of ids.
/// This is used to keep the routing algorithms efficient, while still being able to use the ids in
/// the rest of the code.
pub trait IndexableGraph: Graph {
    /// get a map from node ids to node indices
    fn get_node_idxs_from_ids(&self) -> &IntMap<Id<Node>, NodeIndex>;
    fn get_node_idx_from_id(&self, id: Id<Node>) -> NodeIndex;
    fn get_link_idx_from_id(&self, id: Id<Link>) -> Result<LinkIndex, GraphError>;
    fn get_node_id_from_idx(&self, idx: NodeIndex) -> Result<Id<Node>, GraphError>;
    fn get_link_id_from_idx(&self, idx: LinkIndex) -> Result<Id<Link>, GraphError>;
    fn get_node_from_idx(&self, idx: NodeIndex) -> Result<&Node, GraphError>;
    fn get_link_from_idx(&self, idx: LinkIndex) -> Result<&Link, GraphError>;
    fn outgoing_edges_as_idx(&self, node: NodeIndex) -> Vec<LinkIndex>;
    fn incoming_edges_as_idx(&self, node: NodeIndex) -> Vec<LinkIndex>;
    fn get_end_node_as_idx(&self, edge: LinkIndex) -> Result<NodeIndex, GraphError>;
    fn get_start_node_as_idx(&self, edge: LinkIndex) -> Result<NodeIndex, GraphError>;
}

/// A directed graph, stored in Compressed sparse row (CSR) format.
/// That is, contains the two vectors:
/// - `first_out`: a vector such that `first_out[i]` is the index of the first outgoing edge of
///     node i in `head`, and `first_out[i+1]` is the index of the first outgoing edge of node i+1,
///     i.e., the outgoing edges of node i are exactly those in
///     `head[first_out[i]..first_out[i+1]]`.
/// - `head`: a vector such that `head[j]` is the node index of the end node of the edge with
///     index j
/// This structure allows to efficiently look up the outgoing edges of a node. To efficiently look
/// up incoming edges, use `ForwardBackwardGraph`s, that contain two `CsrGraph`s, one for the
/// forward and one for the backward graph, with the latter allowing to access incoming edges cheaply.
///
/// This structure considers nodes and edges as indices, so it does not contain any information on
/// the actual nodes and edges. Other structures, such as `ForwardBackwardGraph`, are responsible
/// for keeping track of what nodes and edges are, while this struct only stores the Graph
/// structure.
#[derive(Clone, Debug, PartialEq)]
#[allow(dead_code)]
pub struct CsrGraph {
    /// a vector such that `first_out[i]` is the index of the first outgoing edge of
    /// node i in `head`, and `first_out[i+1]` is the index of the first outgoing edge of node i+1,
    /// so that the outgoing edges of node i are exactly those in
    /// `head[first_out[i]..first_out[i+1]]`.
    pub(crate) first_out: Vec<LinkIndex>,
    /// a vector such that `head[j]` is the node index of the end node of the edge with index j
    pub(crate) head: Vec<NodeIndex>,
}

impl CsrGraph {
    pub(crate) fn new(first_out: Vec<LinkIndex>, head: Vec<NodeIndex>) -> CsrGraph {
        CsrGraph { first_out, head }
    }
}

/// Error type for graph operations. Has variants for when node ids, node indices or a range of
/// node indices, or link ids, link indices or a range of link indices, are passed to be used in
/// some operation but are not found in the graph on which the operation takes place.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphError {
    LinkIdNotFound(Id<Link>),
    LinkIndexNotFound(LinkIndex),
    LinkRangeNotFound(Range<LinkIndex>),
    NodeIdNotFound(Id<Node>),
    NodeIndexNotFound(NodeIndex),
    NodeRangeNotFound(Range<NodeIndex>),
}

impl Display for GraphError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GraphError::LinkIdNotFound(link_id) => {
                write!(f, "no link with id {} found in the graph", link_id)
            }
            GraphError::NodeIdNotFound(node_id) => {
                write!(f, "no node with id {} found in the graph", node_id)
            }
            GraphError::LinkIndexNotFound(link_index) => {
                write!(f, "no link with index {} found in the graph", link_index)
            }
            GraphError::NodeIndexNotFound(node_index) => {
                write!(f, "no node with index {} found in the graph.", node_index)
            }
            GraphError::LinkRangeNotFound(link_range) => {
                write!(
                    f,
                    "not all links with indices in range {}..{} found in the graph.",
                    link_range.start, link_range.end
                )
            }
            GraphError::NodeRangeNotFound(node_range) => {
                write!(
                    f,
                    "not all nodes with indices in range {}..{} found in the graph.",
                    node_range.start, node_range.end
                )
            }
        }
    }
}

impl std::error::Error for GraphError {}

/// An implementation of `Graph` and `IndexableGraph`, i.e., a directed graph with network nodes
/// as nodes, and network links as edges, where nodes and edges can be accessed by both their id
/// and their index.
///
/// The graph structure is stored in two `CsrGraph`s, one "forward" graph and one "backward", with
/// the backward graph a copy of the forward one, just with flipped directions on the edges. This
/// allows to have cheap access to both outgoing and incoming edges.
///
/// Also contains maps and vectors to map between indices and (node/link) ids. And stores an Arc
/// pointing to the Network it is representing, which can be used to access the actual network
/// nodes and links from their id.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ForwardBackwardRoutingGraph {
    forward_graph: CsrGraph,
    backward_graph: CsrGraph,
    network: Arc<Network>,
    node_index_by_id: IntMap<Id<Node>, NodeIndex>, // maps node ids to node indices
    node_id_by_index: Vec<Id<Node>>,               // maps (Node)Indices to node ids
    forward_link_id_by_index: Vec<Id<Link>>, // maps link indices of the forward graph to link ids
    backward_link_id_by_index: Vec<Id<Link>>, // maps link indices of the backward graph to link ids
    forward_link_index_by_id: IntMap<Id<Link>, LinkIndex>, // maps link ids to link indices in the forward graph
    backward_link_index_by_id: IntMap<Id<Link>, LinkIndex>, // maps link ids to link indices in the backward graph
}

impl ForwardBackwardRoutingGraph {
    pub fn new(
        forward_graph: CsrGraph,
        backward_graph: CsrGraph,
        network: Arc<Network>,
        node_index_by_id: IntMap<Id<Node>, NodeIndex>, // maps node ids to indices (in first_out, x, y etc.)
        node_id_by_index: Vec<Id<Node>>, // maps (Node)Indices (in first_out, x, y etc.) to node ids
        forward_link_id_by_index: Vec<Id<Link>>, // maps link indices of the forward graph to link ids
        backward_link_id_by_index: Vec<Id<Link>>, // maps link indices of the backward graph to link ids
        forward_link_index_by_id: IntMap<Id<Link>, LinkIndex>, // maps link ids to link indices in the forward graph
        backward_link_index_by_id: IntMap<Id<Link>, LinkIndex>, // maps link ids to link indices in the backward graph
    ) -> Self {
        let graph = Self {
            forward_graph,
            backward_graph,
            network,
            node_index_by_id,
            node_id_by_index,
            forward_link_id_by_index,
            backward_link_id_by_index,
            forward_link_index_by_id,
            backward_link_index_by_id,
        };
        graph.validate_else_panic();
        graph
    }

    /// maps node ids to actual network nodes, by looking them up in the network
    fn nodes_by_node_ids(&self) -> &IntMap<Id<Node>, Node> {
        self.network.nodes_with_ids()
    }
    /// maps link ids to actual network links, by looking them up in the network
    fn links_by_link_ids(&self) -> &IntMap<Id<Link>, Link> {
        self.network.links_with_ids()
    }

    fn validate_else_panic(&self) {
        // ensure forward and backward graphs have same amount of edges
        assert_eq!(
            self.forward_graph.head.len(),
            self.backward_graph.head.len()
        );
        // ensure forward and backward graphs have same amount of nodes
        assert_eq!(
            self.forward_graph.first_out.len(),
            self.backward_graph.first_out.len()
        );
    }

    // All these getters are for internal use, they are only pub(crate) for testing reasons

    pub(crate) fn forward_first_out(&self) -> &Vec<LinkIndex> {
        &self.forward_graph.first_out
    }

    pub(crate) fn backward_first_out(&self) -> &Vec<BackwardLinkIndex> {
        &self.backward_graph.first_out
    }

    pub(crate) fn forward_head(&self) -> &Vec<NodeIndex> {
        &self.forward_graph.head
    }

    pub(crate) fn backward_head(&self) -> &Vec<NodeIndex> {
        &self.backward_graph.head
    }

    pub(crate) fn forward_link_ids(&self) -> &Vec<Id<Link>> {
        &self.forward_link_id_by_index
    }

    pub(crate) fn backward_link_ids(&self) -> &Vec<Id<Link>> {
        &self.backward_link_id_by_index
    }

    pub(crate) fn forward_link_id_pos(&self) -> &IntMap<Id<Link>, LinkIndex> {
        &self.forward_link_index_by_id
    }

    pub(crate) fn backward_link_id_pos(&self) -> &IntMap<Id<Link>, BackwardLinkIndex> {
        &self.backward_link_index_by_id
    }

    pub(crate) fn get_node_id(&self, idx: NodeIndex) -> Result<Id<Node>, GraphError> {
        self.node_id_by_index
            .get(idx)
            .ok_or_else(|| GraphError::NodeIndexNotFound(idx))
            .cloned()
    }

    pub(crate) fn get_link_id(&self, idx: LinkIndex) -> Result<Id<Link>, GraphError> {
        self.forward_link_id_by_index
            .get(idx)
            .ok_or_else(|| GraphError::LinkIndexNotFound(idx))
            .cloned()
    }

    /// Get index of a link in the backward graph, given the index of the link in the forward graph.
    /// The indices differ since they depend on the number of outgoing and incoming edges of all
    /// nodes with lower index than the start node of the link, which differ between forward and
    /// backward graph.
    pub(crate) fn get_backward_link_index(
        &self,
        forward_link_idx: LinkIndex,
    ) -> Result<BackwardLinkIndex, GraphError> {
        // convert forward link index to link id
        let link_id = self.get_link_id(forward_link_idx)?;
        // convert back to backward link index
        self.backward_link_id_pos()
            .get(&link_id)
            .ok_or_else(|| GraphError::LinkIdNotFound(link_id))
            .cloned()
    }
}

impl Graph for ForwardBackwardRoutingGraph {
    fn node(&self, id: Id<Node>) -> Result<&Node, GraphError> {
        self.nodes_by_node_ids()
            .get(&id)
            .ok_or_else(|| GraphError::NodeIdNotFound(id))
    }

    fn edge(&self, id: Id<Link>) -> Result<&Link, GraphError> {
        self.links_by_link_ids()
            .get(&id)
            .ok_or_else(|| GraphError::LinkIdNotFound(id))
    }

    fn outgoing_edges(&self, node: Id<Node>) -> Result<&[Id<Link>], GraphError> {
        let node_idx = self
            .node_index_by_id
            .get(&node)
            .ok_or_else(|| GraphError::NodeIdNotFound(node.clone()))?;

        let link_indices =
            self.forward_first_out()[*node_idx]..self.forward_first_out()[node_idx + 1];

        self.forward_link_ids()
            .get(link_indices.clone())
            .ok_or_else(|| GraphError::LinkRangeNotFound(link_indices))
    }

    fn incoming_edges(&self, node: Id<Node>) -> Result<&[Id<Link>], GraphError> {
        let node_idx = self
            .node_index_by_id
            .get(&node)
            .ok_or_else(|| GraphError::NodeIdNotFound(node.clone()))?;

        let link_indices =
            self.backward_first_out()[*node_idx]..self.backward_first_out()[node_idx + 1];

        self.backward_link_ids()
            .get(link_indices.clone())
            .ok_or_else(|| GraphError::LinkRangeNotFound(link_indices))
    }

    fn num_nodes(&self) -> usize {
        self.forward_first_out().len() - 1
    }

    fn get_end_node(&self, link_id: Id<Link>) -> Result<Id<Node>, GraphError> {
        // find index of link (in self.forward_head())
        let link_id_index = self.get_link_idx_from_id(link_id)?;
        // get node index of end node
        let node_idx = self.forward_head().get(link_id_index).unwrap().clone();
        // convert node index to node id and return
        Ok(self.node_id_by_index[node_idx].clone())
    }

    fn get_start_node(&self, link_id: Id<Link>) -> Result<Id<Node>, GraphError> {
        // get link index of link in the forward graph
        let link_index = self.get_link_idx_from_id(link_id)?;

        // convert it to a backward link index for the backward graph, so we can look up the start
        // node of the edge via self.backward_head()
        let backward_link_index = self.get_backward_link_index(link_index)?;
        let start_node_idx = self
            .backward_head()
            .get(backward_link_index)
            .ok_or_else(|| GraphError::LinkIndexNotFound(backward_link_index))
            .copied()?;

        // finally convert the found node index into node id
        self.get_node_id(start_node_idx)
    }
}

impl IndexableGraph for ForwardBackwardRoutingGraph {
    fn get_node_idxs_from_ids(&self) -> &IntMap<Id<Node>, NodeIndex> {
        &self.node_index_by_id
    }

    fn get_node_idx_from_id(&self, node_id: Id<Node>) -> NodeIndex {
        self.node_index_by_id[&node_id]
    }

    fn get_link_idx_from_id(&self, link_id: Id<Link>) -> Result<LinkIndex, GraphError> {
        self.forward_link_id_pos()
            .get(&link_id)
            .ok_or_else(|| GraphError::LinkIdNotFound(link_id.clone()))
            .copied()
    }
    fn get_node_id_from_idx(&self, idx: NodeIndex) -> Result<Id<Node>, GraphError> {
        self.get_node_id(idx)
    }
    fn get_link_id_from_idx(&self, idx: LinkIndex) -> Result<Id<Link>, GraphError> {
        self.get_link_id(idx)
    }
    fn get_node_from_idx(&self, idx: NodeIndex) -> Result<&Node, GraphError> {
        // uses method from Graph trait to map node id to node
        self.node(self.get_node_id_from_idx(idx)?)
    }
    fn get_link_from_idx(&self, idx: LinkIndex) -> Result<&Link, GraphError> {
        // uses method from Graph trait to map link id to link
        self.edge(self.get_link_id_from_idx(idx)?)
    }
    fn outgoing_edges_as_idx(&self, node: NodeIndex) -> Vec<LinkIndex> {
        (self.forward_first_out()[node]..self.forward_first_out()[node + 1]).collect()
    }
    fn incoming_edges_as_idx(&self, node: NodeIndex) -> Vec<LinkIndex> {
        // Get the link ids from backward structure
        let backward_link_indices =
            self.backward_first_out()[node]..self.backward_first_out()[node + 1];

        // Map backward link indices to link ids
        let link_ids: Vec<_> = backward_link_indices
            .map(|i| &self.backward_link_ids()[i])
            .collect();

        // Map link ids back to (forward) link indices
        link_ids
            .iter()
            .map(|id| *self.forward_link_id_pos().get(id).unwrap())
            .collect()
    }

    fn get_end_node_as_idx(&self, edge: LinkIndex) -> Result<NodeIndex, GraphError> {
        self.forward_head()
            .get(edge)
            .ok_or_else(|| GraphError::LinkIndexNotFound(edge))
            .copied()
    }
    fn get_start_node_as_idx(&self, edge: LinkIndex) -> Result<NodeIndex, GraphError> {
        // find index of edge in the backward graph (in self.backward_head()
        let backward_link_index = self.get_backward_link_index(edge)?;

        // look up start node (= end node in the backward graph) in backward_head()
        self.backward_head()
            .get(backward_link_index)
            .ok_or_else(|| GraphError::LinkIndexNotFound(backward_link_index))
            .copied()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use crate::simulation::config::{MetisOptions, PartitionMethod};
    use crate::simulation::id::Id;
    use crate::simulation::replanning::routing::graph::{CsrGraph, ForwardBackwardRoutingGraph};
    use crate::simulation::replanning::routing::graph::{Graph, GraphError, IndexableGraph};
    use crate::simulation::replanning::routing::network_converter;
    use crate::simulation::scenario::network::{Network, Node};
    use macros::deterministic_id_test;
    use nohash_hasher::IntMap;
    use std::sync::Arc;

    pub fn get_triangle_test_network() -> Network {
        Network::from_file(
            "./assets/routing_tests/triangle-network.xml",
            1,
            &PartitionMethod::Metis(MetisOptions::default()),
        )
    }

    pub fn net_to_graph(network: &Network) -> ForwardBackwardRoutingGraph {
        network_converter::convert_network_for_mode(Arc::new(network.clone()), None)
    }

    #[deterministic_id_test]
    #[should_panic]
    fn test_graph_not_valid() {
        ForwardBackwardRoutingGraph::new(
            CsrGraph::new(vec![0, 1, 2], vec![0, 1, 2, 3, 4, 5]),
            CsrGraph::new(vec![0, 1, 2], vec![0, 1, 2, 3, 4]),
            Arc::new(Network::default()),
            IntMap::default(), // node_index_by_id
            vec![Id::create("0"), Id::create("1"), Id::create("2")],
            vec![],
            vec![],
            IntMap::default(), // forward_link_index_by_id
            IntMap::default(), // bacward_link_index_by_id
        );
    }

    #[deterministic_id_test]
    fn test_graph_valid() {
        ForwardBackwardRoutingGraph::new(
            CsrGraph::new(vec![0, 1, 2], vec![0, 1, 2, 3, 4, 5]),
            CsrGraph::new(vec![42, 43, 44], vec![8, 10, 12, 13, 14, 15]),
            Arc::new(Network::default()),
            IntMap::default(), // node_index_by_id
            vec![Id::create("0"), Id::create("1"), Id::create("2")],
            vec![],
            vec![],
            IntMap::default(), // forward_link_index_by_id
            IntMap::default(), // bacward_link_index_by_id
        );
    }

    /// test outgoing_edges
    #[deterministic_id_test]
    fn test_outgoing_edges() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let node_id = Id::create("1");
        let outgoing_edges = graph.outgoing_edges(node_id).unwrap();
        // verify that the found edges are the true ones for this specific graph
        assert_eq!(outgoing_edges, [Id::create("1"), Id::create("2")]);
    }

    /// test incoming_edges
    #[deterministic_id_test]
    fn test_incoming_edges() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let node_id = Id::create("3");
        let incoming_edges = graph.incoming_edges(node_id).unwrap();
        // verify that the found edges are the true ones for this specific graph
        assert_eq!(incoming_edges, [Id::create("2"), Id::create("4")]);
    }

    /// Test if outgoing_edges_as_idx yields results consistent with outgoing_edges
    #[deterministic_id_test]
    fn test_outgoing_edges_as_idx() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let node_id = Id::<Node>::create("1");
        let node_idx = graph.get_node_idx_from_id(node_id.clone());

        let outgoing_indices = graph.outgoing_edges_as_idx(node_idx);
        let outgoing_ids = graph.outgoing_edges(node_id).unwrap();

        // Lengths should be equal
        assert_eq!(
            outgoing_indices.len(),
            outgoing_ids.len(),
            "outgoing_edges_as_idx and outgoing_edges should have same length"
        );
        for (i, index) in outgoing_indices.iter().enumerate() {
            // Check whether the link id associated with each LinkIndex returned from
            // outgoing_edges_as_idx() matches the link ids returned from outgoing_edges()
            assert_eq!(graph.get_link_id(*index).unwrap(), outgoing_ids[i]);
        }
    }

    /// Test if incoming_edges_as_idx gives results consistent with incoming_edges
    #[deterministic_id_test]
    fn test_incoming_edges_as_idx() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let node_id = Id::<Node>::create("2");
        let node_idx = graph.get_node_idx_from_id(node_id.clone());

        let incoming_indices = graph.incoming_edges_as_idx(node_idx);
        let incoming_ids = graph.incoming_edges(node_id).unwrap();

        // Lengths should be equal
        assert_eq!(
            incoming_indices.len(),
            incoming_ids.len(),
            "incoming_edges_as_idx and incoming_edges should have same length"
        );
        for (i, index) in incoming_indices.iter().enumerate() {
            // Check whether the link id associated with each LinkIndex returned from
            // incoming_edges_as_idx() matches the link ids returned from incoming_edges()
            assert_eq!(graph.get_link_id(*index).unwrap(), incoming_ids[i]);
        }
    }

    /// Test get_end_node with valid link
    #[deterministic_id_test]
    fn test_get_end_node_valid_link() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        // Test with various valid links that exist in the graph
        let outgoing_links = graph.outgoing_edges(Id::<Node>::create("1")).unwrap();
        assert!(
            !outgoing_links.is_empty(),
            "Node 1 should have outgoing links"
        );
        let true_end_nodes = vec![Id::<Node>::create("2"), Id::<Node>::create("3")];

        for (link_id, true_end_node) in outgoing_links.iter().zip(true_end_nodes) {
            let result = graph.get_end_node(link_id.clone());
            assert!(result.is_ok(), "get_end_node should succeed for valid link");
            let end_node = result.unwrap();
            // End node should match the expected node
            assert_eq!(end_node, true_end_node);
        }
    }

    /// Test get_end_node with invalid link (GraphError)
    #[deterministic_id_test]
    fn test_get_end_node_invalid_link_returns_error() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let invalid_link_id = Id::create("nonexistent_link");
        let result = graph.get_end_node(invalid_link_id.clone());

        assert!(
            result.is_err(),
            "get_end_node should return error for invalid link"
        );
        match result.unwrap_err() {
            GraphError::LinkIdNotFound(_) => {} // Expected
            _ => panic!("Expected LinkNotFound error"),
        }
    }

    /// Test get_start_node with valid link
    #[deterministic_id_test]
    fn test_get_start_node_valid_link() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let outgoing_links = graph.outgoing_edges(Id::<Node>::create("1")).unwrap();
        assert!(!outgoing_links.is_empty());

        for link_id in outgoing_links {
            let result = graph.get_start_node(link_id.clone());
            assert!(
                result.is_ok(),
                "get_start_node should succeed for valid link"
            );
            let start_node = result.unwrap();
            assert_eq!(start_node, Id::<Node>::create("1"));
        }
    }

    /// Test get_start_node with invalid link
    #[deterministic_id_test]
    fn test_get_start_node_invalid_link_returns_error() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let invalid_link_id = Id::create("nonexistent_link");
        let result = graph.get_start_node(invalid_link_id.clone());

        assert!(
            result.is_err(),
            "get_start_node should return error for invalid link"
        );
    }

    /// Test roundtrip: Link ID -> Index -> Link ID should be equal
    #[deterministic_id_test]
    fn test_link_id_to_idx_roundtrip() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let outgoing_links = graph.outgoing_edges(Id::<Node>::create("1")).unwrap();
        for link_id in outgoing_links {
            // Link ID -> Link Index
            let link_idx = graph.get_link_idx_from_id(link_id.clone()).unwrap();
            // Link Index -> Link ID
            let link_id_roundtrip = graph.get_link_id_from_idx(link_idx).unwrap();

            assert_eq!(
                *link_id, link_id_roundtrip,
                "Link ID roundtrip should be idempotent"
            );
        }
    }

    /// Test roundtrip: Node ID -> Index -> Node ID should be equal
    #[deterministic_id_test]
    fn test_node_id_to_idx_roundtrip() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let node_id = Id::<Node>::create("1");
        // Node ID -> Node Index
        let node_idx = graph.get_node_idx_from_id(node_id.clone());
        // Node Index -> Node ID
        let node_id_roundtrip = graph.get_node_id_from_idx(node_idx);

        assert_eq!(
            node_id,
            node_id_roundtrip.unwrap(),
            "Node ID roundtrip should be idempotent"
        );
    }

    #[deterministic_id_test]
    fn test_get_end_node_as_idx_valid_edges() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        let true_end_node_indices = vec![vec![], vec![2, 3], vec![2, 3], vec![1, 2]];

        // For all outgoing edges, get_end_node_as_idx should not panic, since they are all valid (exist in the graph)
        for node_idx in 0..graph.num_nodes() {
            let outgoing_edge_indices = graph.outgoing_edges_as_idx(node_idx);
            for (j, edge_idx) in outgoing_edge_indices.iter().enumerate() {
                let end_node = graph.get_end_node_as_idx(*edge_idx).unwrap(); // Should not panic
                assert_eq!(
                    end_node, true_end_node_indices[node_idx][j],
                    "End node is incorrect, expected {}, got {}",
                    true_end_node_indices[node_idx][j], end_node
                );
            }
        }
    }

    #[deterministic_id_test]
    fn test_get_end_node_as_idx_invalid_edge() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        assert_eq!(
            graph.get_end_node_as_idx(999),
            Err(GraphError::LinkIndexNotFound(999)),
            "get_end_node_as_idx should return LinkIndexNotFound error for invalid edge index"
        );
    }

    #[deterministic_id_test]
    fn test_get_start_node_as_idx_invalid_edge() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        assert_eq!(
            graph.get_start_node_as_idx(999),
            Err(GraphError::LinkIndexNotFound(999)),
            "get_start_node_as_idx should return LinkIndexNotFound error for invalid edge index"
        );
    }

    #[deterministic_id_test]
    fn test_get_start_node_as_idx() {
        let network = get_triangle_test_network();
        let graph = net_to_graph(&network);

        // For all outgoing edges, get_start_node_as_idx should not panic
        for node_idx in 0..graph.num_nodes() {
            let outgoing_edge_indices = graph.outgoing_edges_as_idx(node_idx);
            for edge_idx in outgoing_edge_indices {
                let start_node = graph.get_start_node_as_idx(edge_idx).unwrap(); // Should not panic
                assert_eq!(
                    start_node, node_idx,
                    "Start node should match the source node"
                );
            }
        }
    }
}
