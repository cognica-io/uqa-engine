//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Core value types for UQA: doc ids, payloads, posting entries, and the
//! dynamic [`Value`] used inside payload fields.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use rust_decimal::{Decimal, RoundingStrategy};
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

mod decimal;
mod graph;
mod graph_phi;
mod index_stats;
mod posting;
mod temporal;
mod value;

pub use decimal::DecimalValue;
pub use graph::{Edge, EdgeId, Vertex, VertexId};
pub use graph_phi::{
    GraphPhiEnvelope, GraphPhiPayload, GRAPH_PHI_EDGES_FIELD, GRAPH_PHI_FIELD,
    GRAPH_PHI_VERTICES_FIELD,
};
pub use index_stats::IndexStats;
pub use posting::{GeneralizedPayload, GeneralizedPostingEntry, Payload, PostingEntry};
pub use temporal::TemporalValue;
pub use value::Value;

#[cfg(test)]
mod tests;
