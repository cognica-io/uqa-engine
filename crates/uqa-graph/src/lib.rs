//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Property graphs and their posting representation.
//!
//! Defines `GraphStore` (trait + in-memory implementation), the
//! `GraphPostingList` extension of [`uqa_core::PostingList`], and the
//! lossless `Phi` codec for carrying graph metadata through ordinary posting
//! storage. Graph-result merges expose their subgraph collision policy
//! separately from ordinary posting payload merges.

pub mod adapters;
pub mod agtype;
mod centrality;
mod cross_paradigm;
pub mod cypher;
mod delta;
mod embedding;
mod incremental_match;
mod index;
mod memory_store;
mod message_passing;
mod operator_impls;
mod operators;
mod pattern;
mod posting_list;
mod rpq;
mod sqlite_store;
mod store;
mod subgraph_index;
mod temporal;
mod types;
mod versioned_store;

pub use adapters::{GraphPostingCodec, PostingToGraphAdapter, TextTfScoreNormalizer};
pub use centrality::{BetweennessCentrality, PageRank, HITS};
pub use cross_paradigm::{
    CrossParadigmError, CrossParadigmResult, Document, SemanticGraphSearch, TextToGraph, ToGraph,
    VectorEnhancedMatch, VertexEmbedding,
};
pub use delta::{DeltaOp, GraphDelta};
pub use embedding::{GraphEmbedding, MAX_GRAPH_EMBEDDING_DIMENSIONS, MAX_GRAPH_EMBEDDING_LAYERS};
pub use incremental_match::{implicated_vertices, IncrementalPatternMatcher};
pub use index::{LabelIndex, PathIndex};
pub use memory_store::{
    graphid_label_id, graphid_sequence, make_graphid, GraphLabelRegistry, MemoryGraphStore,
    EDGE_DEFAULT_LABEL_ID, FIRST_USER_LABEL_ID, GRAPHID_LABEL_SHIFT, VERTEX_DEFAULT_LABEL_ID,
};
pub use message_passing::{AggregationKind, MessagePassing, MAX_MESSAGE_PASSING_LAYERS};
pub use operator_impls::{CypherQueryOperator, WeightedPathQueryOperator};
pub use operators::{
    AggFn, GMatch, RegularPathQuery, Traverse, VertexAggregation, VertexMatch, WeightedPathQuery,
    DEFAULT_GRAPH_SCORE,
};
pub use pattern::{EdgePattern, EdgePredicate, GraphPattern, VertexPattern, VertexPredicate};
pub use posting_list::{
    GraphPayload, GraphPostingList, GraphPostingListError, GraphPostingListResult,
    SubgraphMergePolicy,
};
pub use rpq::{
    build_nfa, epsilon_closure, parse_rpq, simplify, subset_construction, Dfa, DfaState, Nfa,
    NfaTransition, RPQBuildError, RPQParseError, RegularPathExpr, StateId, MAX_DFA_STATES,
    MAX_NFA_STATES, MAX_RPQ_AST_DEPTH,
};
pub use sqlite_store::SQLiteGraphStore;
pub use store::{GraphStore, GraphStoreError, GraphStoreResult};
pub use subgraph_index::SubgraphIndex;
pub use temporal::{TemporalFilter, TemporalPatternMatch, TemporalTraverse};
pub use types::Direction;
pub use versioned_store::VersionedGraphStore;
