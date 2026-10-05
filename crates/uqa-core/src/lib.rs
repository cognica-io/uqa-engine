//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Document supports, finite-support relations, posting storage, ranked views,
//! and shared value types for UQA.
//!
//! See `docs/plans/0001-uqa-engine-implementation-plan.md` Section 2.1 for the
//! algebraic invariants this crate must preserve.

pub mod agtype;
pub mod cancel;
pub mod doc_set;
mod float_text;
pub mod json;
pub mod memory;
pub mod notifications;
pub mod ordering;
pub mod posting_list;
pub mod predicate;
pub mod ranked_view;
pub mod relation;
mod relation_identity;
pub mod types;
pub mod vector_execution;

pub use cancel::{
    CancellationDeadline, CancellationReason, CancellationToken, QueryCancelled, ScheduledAction,
    SQLSTATE_QUERY_CANCELED,
};
pub use doc_set::DocSet;
pub use float_text::{format_float_pg, format_float_pg_with_control};
pub use posting_list::{GeneralizedPostingList, PostingList};
pub use predicate::{sql_null_test, Predicate};
pub use ranked_view::RankedView;
pub use relation::{LogSemiring, Relation, RelationEntry, Semiring};
pub use relation_identity::RelationIdentity;
pub use types::{
    jsonb_equality_key, jsonb_equality_key_with_control, write_jsonb_comparison_key,
    write_jsonb_equality_key, ArrayAssignmentError, ArrayTraversalError, ArrayValue,
    BudgetedArrayElements, ControlledArrayElements, DecimalValue, DiskANNIndexStats,
    DiskANNQueryStats, DocId, Edge, EdgeId, EnumLabelKey, EnumLabelKeyError,
    EnumLabelKeyParseError, EnumValue, FieldName, GeneralizedPayload, GeneralizedPostingEntry,
    IndexStats, JsonValueDecoder, JsonbKeyError, LegacyVectorKind, LegacyVectorValue, PathExpr,
    PathSegment, Payload, PostingEntry, TemporalInputError, TemporalValue, TokenOccurrence,
    TokenOccurrenceError, TokenOffsets, Value, ValueRetentionError, VectorGeneration,
    VectorPopulationStats, VectorQueryRoute, VectorReadStats, Vertex, VertexId,
    MAX_ENUM_LABEL_KEY_BYTES,
};

mod scored_entry;
pub use scored_entry::ScoredEntry;

pub mod catalog_acl;
pub mod catalog_identity;
pub mod catalog_index;
pub mod catalog_role;
pub mod catalog_schema;
pub mod catalog_sequence;

pub mod retrieval;
pub mod rpq;
