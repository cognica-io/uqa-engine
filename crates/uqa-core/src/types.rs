//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core value types for UQA: doc ids, payloads, posting entries, and the
//! dynamic [`Value`] used inside payload fields.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Document identifier.
///
/// `u64` addresses up to ~1.8e19 documents while keeping the on-disk
/// representation compact at 8 bytes per posting entry head.
pub type DocId = u64;

/// Field name within a document.
pub type FieldName = String;

/// One string-key or integer-index step in a hierarchical-document path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum PathSegment {
    Key(String),
    Index(usize),
}

/// A path expression - a sequence of [`PathSegment`]s navigating a
/// hierarchical document.
pub type PathExpr = Vec<PathSegment>;

mod array;
mod decimal;
mod graph;
mod graph_phi;
mod index_stats;
mod jsonb;
mod occurrence;
mod posting;
mod temporal;
mod value;

pub use array::{ArrayTraversalError, ArrayValue, BudgetedArrayElements};
pub use decimal::DecimalValue;
pub use graph::{Edge, EdgeId, Vertex, VertexId};
pub use graph_phi::{
    GraphPhiEnvelope, GraphPhiPayload, GRAPH_PHI_EDGES_FIELD, GRAPH_PHI_FIELD,
    GRAPH_PHI_VERTICES_FIELD,
};
pub use index_stats::IndexStats;
pub use jsonb::{
    jsonb_equality_key, write_jsonb_comparison_key, write_jsonb_equality_key, JsonbKeyError,
};
pub use occurrence::{TokenOccurrence, TokenOccurrenceError, TokenOffsets};
pub use posting::{GeneralizedPayload, GeneralizedPostingEntry, Payload, PostingEntry};
pub use temporal::TemporalValue;
pub use value::{JsonValueDecoder, Value, ValueRetentionError};

#[cfg(test)]
mod tests;
