//! Numeric aggregation over graph-result vertex payloads.

use super::{
    value_as_f64, BTreeMap, BTreeSet, GraphPayload, GraphPostingList, GraphStore, GraphStoreError,
    GraphStoreResult, Payload, PostingEntry, PostingList, Value, VertexId,
};

/// Aggregation function over a numeric vertex property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggFn {
    Sum,
    Avg,
    Min,
    Max,
    Count,
}

/// Aggregate a numeric property over the vertex set rolled up in a
/// source operator's `GraphPayload`s. Mirrors Definition 2.2.3.
pub struct VertexAggregation<'a> {
    pub source: GraphPostingList,
    pub property_name: String,
    pub agg_fn: AggFn,
    pub graph: &'a str,
}

impl<'a> VertexAggregation<'a> {
    pub fn new(
        source: GraphPostingList,
        property: impl Into<String>,
        agg_fn: AggFn,
        graph: &'a str,
    ) -> Self {
        Self {
            source,
            property_name: property.into(),
            agg_fn,
            graph,
        }
    }

    pub fn execute<G: GraphStore>(&self, store: &G) -> GraphStoreResult<GraphPostingList> {
        let mut vertex_ids: BTreeSet<VertexId> = BTreeSet::new();
        for entry in self.source.inner().entries() {
            if let Some(gp) = self.source.get_graph_payload(entry.doc_id) {
                vertex_ids.extend(gp.subgraph_vertices.iter().copied());
            }
        }
        let mut numeric: Vec<f64> = Vec::new();
        for vid in &vertex_ids {
            let vtx = store.get_vertex(*vid).ok_or_else(|| {
                GraphStoreError::CorruptGraph(format!("missing aggregate vertex {vid}"))
            })?;
            if let Some(value) = vtx.properties.get(&self.property_name) {
                if let Some(number) = value_as_f64(value)? {
                    numeric.push(number);
                } else {
                    return Err(GraphStoreError::InvalidMutation(format!(
                        "vertex {vid} property {:?} is not numeric",
                        self.property_name
                    )));
                }
            }
        }
        let result = aggregate(self.agg_fn, &numeric)?;

        let mut fields: BTreeMap<String, Value> = BTreeMap::new();
        fields.insert(
            "_vertex_agg_property".to_string(),
            Value::Str(self.property_name.clone()),
        );
        fields.insert(
            "_vertex_agg_fn".to_string(),
            Value::Str(format!("{:?}", self.agg_fn).to_lowercase()),
        );
        fields.insert("_vertex_agg_result".to_string(), Value::Float(result));
        fields.insert(
            "_vertex_agg_count".to_string(),
            Value::Int(i64::try_from(numeric.len()).map_err(|_| {
                GraphStoreError::CorruptGraph(
                    "vertex aggregate count exceeds agtype integer range".into(),
                )
            })?),
        );

        let entry = PostingEntry::new(
            0,
            Payload {
                positions: Vec::new(),
                score: result,
                fields,
            },
        );
        let mut graph_payloads = BTreeMap::new();
        graph_payloads.insert(
            0,
            GraphPayload {
                subgraph_vertices: vertex_ids.into_iter().collect(),
                subgraph_edges: Vec::new(),
                graph_name: self.graph.to_string(),
                score_override: Some(result),
            },
        );
        GraphPostingList::try_from_parts(
            PostingList::from_sorted_unchecked(vec![entry]),
            graph_payloads,
        )
        .map_err(Into::into)
    }
}

fn aggregate(agg_fn: AggFn, values: &[f64]) -> GraphStoreResult<f64> {
    if values.is_empty() {
        return Ok(0.0);
    }
    let count = if u64::try_from(values.len()).is_ok_and(|count| count <= 9_007_199_254_740_992) {
        values.len() as f64
    } else {
        return Err(GraphStoreError::CorruptGraph(
            "vertex aggregate count exceeds the exact f64 integer range".into(),
        ));
    };
    let result = match agg_fn {
        AggFn::Sum => values.iter().sum(),
        AggFn::Avg => values.iter().sum::<f64>() / count,
        AggFn::Min => values.iter().copied().fold(f64::INFINITY, f64::min),
        AggFn::Max => values.iter().copied().fold(f64::NEG_INFINITY, f64::max),
        AggFn::Count => count,
    };
    if result.is_finite() {
        Ok(result)
    } else {
        Err(GraphStoreError::InvalidMutation(format!(
            "vertex aggregate result is not finite: {result}"
        )))
    }
}
