//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document supports, finite-support relations, posting storage, ranked views,
//! and shared value types for UQA.
//!
//! See `docs/plans/0001-uqa-rs-implementation-plan.md` Section 2.1 for the
//! algebraic invariants this crate must preserve.

pub mod cancel;
pub mod doc_set;
pub mod posting_list;
pub mod predicate;
pub mod ranked_view;
pub mod relation;
pub mod types;

pub use cancel::{CancellationToken, QueryCancelled, SQLSTATE_QUERY_CANCELED};
pub use doc_set::DocSet;
pub use posting_list::{GeneralizedPostingList, PostingList};
pub use predicate::Predicate;
pub use ranked_view::RankedView;
pub use relation::{LogSemiring, Relation, RelationEntry, Semiring};
pub use types::{
    jsonb_equality_key, ArrayValue, DecimalValue, DocId, Edge, EdgeId, FieldName,
    GeneralizedPayload, GeneralizedPostingEntry, IndexStats, PathExpr, PathSegment, Payload,
    PostingEntry, TemporalValue, Value, Vertex, VertexId,
};
