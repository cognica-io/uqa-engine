//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property graphs and the algebra over them.
//!
//! Defines `GraphStore` (trait + in-memory implementation), the
//! `GraphPostingList` extension of [`uqa_core::PostingList`], and the
//! `Phi` homomorphism (Theorem 1.1.6, Paper 2) that lets graph
//! operations compose with the standard posting-list algebra without
//! losing structure.

mod centrality;
mod cross_paradigm;
pub mod cypher;
mod delta;
mod embedding;
mod incremental_match;
mod index;
mod memory_store;
mod message_passing;
mod operators;
mod pattern;
mod posting_list;
mod rpq;
mod sqlite_store;
mod store;
mod temporal;
mod types;
mod versioned_store;

pub use centrality::{BetweennessCentrality, Hits, PageRank};
pub use cross_paradigm::{
    Document, SemanticGraphSearch, TextToGraph, ToGraph, VectorEnhancedMatch, VertexEmbedding,
};
pub use delta::{DeltaOp, GraphDelta};
pub use embedding::GraphEmbedding;
pub use incremental_match::{implicated_vertices, IncrementalPatternMatcher};
pub use index::{LabelIndex, PathIndex};
pub use memory_store::MemoryGraphStore;
pub use message_passing::{AggregationKind, MessagePassing};
pub use operators::{
    AggFn, GMatch, RegularPathQuery, Traverse, VertexAggregation, VertexMatch, DEFAULT_GRAPH_SCORE,
};
pub use pattern::{EdgePattern, EdgePredicate, GraphPattern, VertexPattern, VertexPredicate};
pub use posting_list::{GraphPayload, GraphPostingList};
pub use rpq::{
    build_nfa, epsilon_closure, parse_rpq, simplify, subset_construction, Dfa, DfaState, Nfa,
    NfaTransition, RPQParseError, RegularPathExpr, StateId,
};
pub use sqlite_store::SQLiteGraphStore;
pub use store::GraphStore;
pub use temporal::{TemporalFilter, TemporalTraverse};
pub use types::Direction;
pub use versioned_store::VersionedGraphStore;
