//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Versioned graph posting-list payload codec carried through dynamic values.

use super::{BTreeMap, EdgeId, Value, VertexId};

/// Field used by the graph posting-list Phi encoding.
///
/// This is public only so `uqa-graph` and the core posting merge logic can
/// share one versioned codec. Applications should treat it as an opaque
/// implementation detail.
#[doc(hidden)]
pub const GRAPH_PHI_FIELD: &str = "_uqa_graph_phi";
#[doc(hidden)]
pub const GRAPH_PHI_VERTICES_FIELD: &str = "_graph_vertices";
#[doc(hidden)]
pub const GRAPH_PHI_EDGES_FIELD: &str = "_graph_edges";

const GRAPH_PHI_MAGIC: &str = "uqa.graph.phi";
const GRAPH_PHI_VERSION: i64 = 1;
const GRAPH_PHI_MAGIC_KEY: &str = "magic";
const GRAPH_PHI_VERSION_KEY: &str = "version";
const GRAPH_PHI_BASE_SCORE_KEY: &str = "base_score";
const GRAPH_PHI_GRAPH_PRESENT_KEY: &str = "graph_present";
const GRAPH_PHI_VERTICES_KEY: &str = "vertices";
const GRAPH_PHI_EDGES_KEY: &str = "edges";
const GRAPH_PHI_GRAPH_NAME_KEY: &str = "graph_name";
const GRAPH_PHI_OVERRIDE_PRESENT_KEY: &str = "override_present";
const GRAPH_PHI_OVERRIDE_SCORE_KEY: &str = "override_score";
const GRAPH_PHI_ORIGINAL_PRESENT_KEY: &str = "original_present";
const GRAPH_PHI_ORIGINAL_VALUE_KEY: &str = "original_value";
const GRAPH_PHI_ORIGINAL_VERTICES_PRESENT_KEY: &str = "original_vertices_present";
const GRAPH_PHI_ORIGINAL_VERTICES_VALUE_KEY: &str = "original_vertices_value";
const GRAPH_PHI_ORIGINAL_EDGES_PRESENT_KEY: &str = "original_edges_present";
const GRAPH_PHI_ORIGINAL_EDGES_VALUE_KEY: &str = "original_edges_value";
const GRAPH_PHI_FIELD_COUNT: usize = 15;

/// Graph-specific part of a versioned Phi envelope.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphPhiPayload {
    pub vertices: Vec<VertexId>,
    pub edges: Vec<EdgeId>,
    pub graph_name: String,
}

impl GraphPhiPayload {
    #[doc(hidden)]
    pub fn encoded_vertices(&self) -> Value {
        encode_u64_list(&self.vertices)
    }

    #[doc(hidden)]
    pub fn encoded_edges(&self) -> Value {
        encode_u64_list(&self.edges)
    }
}

/// Lossless metadata carried through ordinary posting payload merges.
///
/// Scores use their IEEE-754 bit representation in [`Value`] so `-0.0` and
/// NaN payloads survive a non-colliding round trip exactly. The original value
/// at [`GRAPH_PHI_FIELD`] is nested in the envelope, making the reserved field
/// collision-safe for values produced by the encoder.
#[doc(hidden)]
#[derive(Debug, Clone)]
pub struct GraphPhiEnvelope {
    pub base_score: f64,
    pub graph_payload: Option<GraphPhiPayload>,
    pub score_override: Option<f64>,
    pub original_reserved: Option<Value>,
    pub original_vertices: Option<Value>,
    pub original_edges: Option<Value>,
}

impl GraphPhiEnvelope {
    #[doc(hidden)]
    pub fn encode(self) -> Value {
        let (graph_present, vertices, edges, graph_name) = self.graph_payload.map_or_else(
            || (false, Vec::new(), Vec::new(), String::new()),
            |graph| (true, graph.vertices, graph.edges, graph.graph_name),
        );
        let (override_present, override_score) = self.score_override.map_or_else(
            || (false, Value::Null),
            |score| (true, encode_f64_bits(score)),
        );
        let (original_present, original_value) = self
            .original_reserved
            .map_or_else(|| (false, Value::Null), |value| (true, value));
        let (original_vertices_present, original_vertices_value) = self
            .original_vertices
            .map_or_else(|| (false, Value::Null), |value| (true, value));
        let (original_edges_present, original_edges_value) = self
            .original_edges
            .map_or_else(|| (false, Value::Null), |value| (true, value));

        Value::Map(BTreeMap::from([
            (
                GRAPH_PHI_MAGIC_KEY.to_string(),
                Value::Str(GRAPH_PHI_MAGIC.to_string()),
            ),
            (
                GRAPH_PHI_VERSION_KEY.to_string(),
                Value::Int(GRAPH_PHI_VERSION),
            ),
            (
                GRAPH_PHI_BASE_SCORE_KEY.to_string(),
                encode_f64_bits(self.base_score),
            ),
            (
                GRAPH_PHI_GRAPH_PRESENT_KEY.to_string(),
                Value::Bool(graph_present),
            ),
            (
                GRAPH_PHI_VERTICES_KEY.to_string(),
                encode_u64_list(&vertices),
            ),
            (GRAPH_PHI_EDGES_KEY.to_string(), encode_u64_list(&edges)),
            (GRAPH_PHI_GRAPH_NAME_KEY.to_string(), Value::Str(graph_name)),
            (
                GRAPH_PHI_OVERRIDE_PRESENT_KEY.to_string(),
                Value::Bool(override_present),
            ),
            (GRAPH_PHI_OVERRIDE_SCORE_KEY.to_string(), override_score),
            (
                GRAPH_PHI_ORIGINAL_PRESENT_KEY.to_string(),
                Value::Bool(original_present),
            ),
            (GRAPH_PHI_ORIGINAL_VALUE_KEY.to_string(), original_value),
            (
                GRAPH_PHI_ORIGINAL_VERTICES_PRESENT_KEY.to_string(),
                Value::Bool(original_vertices_present),
            ),
            (
                GRAPH_PHI_ORIGINAL_VERTICES_VALUE_KEY.to_string(),
                original_vertices_value,
            ),
            (
                GRAPH_PHI_ORIGINAL_EDGES_PRESENT_KEY.to_string(),
                Value::Bool(original_edges_present),
            ),
            (
                GRAPH_PHI_ORIGINAL_EDGES_VALUE_KEY.to_string(),
                original_edges_value,
            ),
        ]))
    }

    /// Decode only the exact current schema. A lookalike or future version is
    /// an ordinary application field, not a partially decoded envelope.
    #[doc(hidden)]
    pub fn decode(value: Option<&Value>) -> Option<Self> {
        let Value::Map(fields) = value? else {
            return None;
        };
        let valid_magic = matches!(
            fields.get(GRAPH_PHI_MAGIC_KEY),
            Some(Value::Str(magic)) if magic == GRAPH_PHI_MAGIC
        );
        if fields.len() != GRAPH_PHI_FIELD_COUNT
            || !valid_magic
            || fields.get(GRAPH_PHI_VERSION_KEY) != Some(&Value::Int(GRAPH_PHI_VERSION))
        {
            return None;
        }

        let base_score = decode_f64_bits(fields.get(GRAPH_PHI_BASE_SCORE_KEY)?)?;
        let Value::Bool(graph_present) = fields.get(GRAPH_PHI_GRAPH_PRESENT_KEY)? else {
            return None;
        };
        let vertices = decode_u64_list(fields.get(GRAPH_PHI_VERTICES_KEY)?)?;
        let edges = decode_u64_list(fields.get(GRAPH_PHI_EDGES_KEY)?)?;
        let Value::Str(graph_name) = fields.get(GRAPH_PHI_GRAPH_NAME_KEY)? else {
            return None;
        };
        let Value::Bool(override_present) = fields.get(GRAPH_PHI_OVERRIDE_PRESENT_KEY)? else {
            return None;
        };
        let override_value = fields.get(GRAPH_PHI_OVERRIDE_SCORE_KEY)?;
        let score_override = match (*override_present, override_value) {
            (true, value) => Some(decode_f64_bits(value)?),
            (false, Value::Null) => None,
            (false, _) => return None,
        };
        let Value::Bool(original_present) = fields.get(GRAPH_PHI_ORIGINAL_PRESENT_KEY)? else {
            return None;
        };
        let original_value = fields.get(GRAPH_PHI_ORIGINAL_VALUE_KEY)?;
        let original_reserved = match (*original_present, original_value) {
            (true, value) => Some(value.clone()),
            (false, Value::Null) => None,
            (false, _) => return None,
        };
        let original_vertices = decode_optional_shadow(
            fields.get(GRAPH_PHI_ORIGINAL_VERTICES_PRESENT_KEY)?,
            fields.get(GRAPH_PHI_ORIGINAL_VERTICES_VALUE_KEY)?,
        )
        .ok()?;
        let original_edges = decode_optional_shadow(
            fields.get(GRAPH_PHI_ORIGINAL_EDGES_PRESENT_KEY)?,
            fields.get(GRAPH_PHI_ORIGINAL_EDGES_VALUE_KEY)?,
        )
        .ok()?;

        let graph_payload = if *graph_present {
            Some(GraphPhiPayload {
                vertices,
                edges,
                graph_name: graph_name.clone(),
            })
        } else {
            if !vertices.is_empty()
                || !edges.is_empty()
                || !graph_name.is_empty()
                || score_override.is_some()
            {
                return None;
            }
            None
        };

        Some(Self {
            base_score,
            graph_payload,
            score_override,
            original_reserved,
            original_vertices,
            original_edges,
        })
    }

    /// Whether a value claims the Phi namespace, even if its schema or
    /// version is unsupported. Such values must not fall through to the
    /// ambiguous legacy two-field decoder.
    #[doc(hidden)]
    pub fn is_recognized(value: Option<&Value>) -> bool {
        matches!(
            value,
            Some(Value::Map(fields))
                if matches!(
                    fields.get(GRAPH_PHI_MAGIC_KEY),
                    Some(Value::Str(magic)) if magic == GRAPH_PHI_MAGIC
                )
        )
    }
}

#[derive(Debug)]
struct InvalidGraphPhiEnvelope;

fn decode_optional_shadow(
    present: &Value,
    value: &Value,
) -> std::result::Result<Option<Value>, InvalidGraphPhiEnvelope> {
    match (present, value) {
        (Value::Bool(true), value) => Ok(Some(value.clone())),
        (Value::Bool(false), Value::Null) => Ok(None),
        _ => Err(InvalidGraphPhiEnvelope),
    }
}

fn encode_f64_bits(value: f64) -> Value {
    Value::Bytes(value.to_bits().to_be_bytes().to_vec())
}

fn decode_f64_bits(value: &Value) -> Option<f64> {
    let Value::Bytes(bytes) = value else {
        return None;
    };
    let encoded: [u8; size_of::<u64>()] = bytes.as_slice().try_into().ok()?;
    Some(f64::from_bits(u64::from_be_bytes(encoded)))
}

fn encode_u64_list(values: &[u64]) -> Value {
    Value::List(
        values
            .iter()
            .map(|value| Value::Bytes(value.to_be_bytes().to_vec()))
            .collect(),
    )
}

fn decode_u64_list(value: &Value) -> Option<Vec<u64>> {
    let Value::List(values) = value else {
        return None;
    };
    values
        .iter()
        .map(|value| {
            let Value::Bytes(bytes) = value else {
                return None;
            };
            let encoded: [u8; size_of::<u64>()] = bytes.as_slice().try_into().ok()?;
            Some(u64::from_be_bytes(encoded))
        })
        .collect()
}
