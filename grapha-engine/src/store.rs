pub mod json;
pub mod sqlite;

use crate::delta::{EntitySyncStats, GraphDelta, SyncMode};
use grapha_core::graph::Graph;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreWriteStats {
    pub mode: SyncMode,
    pub nodes: EntitySyncStats,
    pub edges: EntitySyncStats,
}

impl StoreWriteStats {
    pub fn from_graphs(previous: Option<&Graph>, graph: &Graph, mode: SyncMode) -> Self {
        let (nodes, edges) = match previous {
            Some(previous_graph) => {
                let delta = GraphDelta::between(previous_graph, graph);
                (delta.node_stats(), delta.edge_stats())
            }
            None => (
                EntitySyncStats::from_total(graph.nodes.len()),
                EntitySyncStats::from_total(graph.edges.len()),
            ),
        };
        Self { mode, nodes, edges }
    }

    pub fn summary(self) -> String {
        format!(
            "{} nodes +{} ~{} -{}, edges +{} ~{} -{}",
            self.mode.label(),
            self.nodes.added,
            self.nodes.updated,
            self.nodes.deleted,
            self.edges.added,
            self.edges.updated,
            self.edges.deleted
        )
    }
}

/// Abstraction over graph storage backends.
pub trait Store {
    fn save(&self, graph: &Graph) -> anyhow::Result<()>;

    fn save_incremental(
        &self,
        previous: Option<&Graph>,
        graph: &Graph,
    ) -> anyhow::Result<StoreWriteStats> {
        let stats = StoreWriteStats::from_graphs(previous, graph, SyncMode::FullRebuild);
        self.save(graph)?;
        Ok(stats)
    }

    fn load(&self) -> anyhow::Result<Graph>;
}
