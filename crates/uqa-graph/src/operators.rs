//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Traversal, matching, path, and aggregation operators over graph stores.
//!
//! Every operator returns a [`GraphPostingList`] so graph results compose with
//! document support and payload merge operations.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use uqa_core::{DocId, Edge, EdgeId, Payload, PostingEntry, PostingList, Value, VertexId};
use uqa_operators::PathWeightPredicate;

use crate::pattern::{EdgePattern, GraphPattern, VertexPredicate};
use crate::posting_list::{GraphPayload, GraphPostingList};
use crate::rpq::{build_nfa, simplify, subset_construction, Dfa, DfaState, RegularPathExpr};
use crate::store::{GraphStore, GraphStoreError, GraphStoreResult};

mod aggregation;
mod gmatch;
mod numeric;
mod regular_path;
mod result;
mod traverse;
mod vertex_match;
mod weighted_path;

pub use aggregation::{AggFn, VertexAggregation};
pub use gmatch::GMatch;
pub use regular_path::RegularPathQuery;
pub use traverse::Traverse;
pub use vertex_match::VertexMatch;
pub use weighted_path::WeightedPathQuery;

use numeric::value_as_f64;
use result::{graph_id_value, synthetic_doc_id};

/// Default score lifted into traversal and match payloads.
pub const DEFAULT_GRAPH_SCORE: f64 = 0.9;
