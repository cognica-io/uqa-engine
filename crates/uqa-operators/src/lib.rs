//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Algebraic operators over posting lists.
//!
//! Operators form a monoid under composition (Theorem 3.2.3, Paper 1):
//! every concrete operator's `execute` returns a [`PostingList`], and
//! [`ComposedOperator`] is associative with the empty operator as
//! identity.

pub mod base;
pub mod boolean;
pub mod primitive;

pub use base::{ComposedOperator, ExecutionContext, Operator};
pub use boolean::{ComplementOperator, IntersectOperator, UnionOperator};
pub use primitive::{FacetOperator, FilterOperator, ScoreOperator, TermOperator};
