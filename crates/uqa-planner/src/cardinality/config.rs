//! Estimator construction and statistics attachment.

use super::{Arc, BTreeMap, CardinalityEstimator, ColumnStats, GraphStats, GraphStoreSampler};

impl CardinalityEstimator {
    pub fn new() -> Self {
        Self {
            default_selectivity: 0.1,
            like_selectivity: 0.05,
            range_selectivity: 0.3,
            column_stats: BTreeMap::new(),
            graph_stats: None,
            graph_store: None,
        }
    }

    pub fn with_column_stats(mut self, stats: BTreeMap<String, ColumnStats>) -> Self {
        self.column_stats = stats;
        self
    }

    pub fn with_graph_stats(mut self, stats: GraphStats) -> Self {
        self.graph_stats = Some(stats);
        self
    }

    pub fn with_graph_store(mut self, store: Arc<dyn GraphStoreSampler>) -> Self {
        self.graph_store = Some(store);
        self
    }
}
