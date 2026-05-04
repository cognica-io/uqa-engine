//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Universal posting list abstraction and value types for UQA.
//!
//! See `docs/plans/0001-uqa-python-to-rust-port.md` Section 2.1 for the
//! algebraic invariants this crate must preserve.

pub mod posting_list;
pub mod types;

pub use posting_list::{GeneralizedPostingList, PostingList};
pub use types::{
    DocId, FieldName, GeneralizedPayload, GeneralizedPostingEntry, IndexStats, Payload,
    PostingEntry, Value,
};
