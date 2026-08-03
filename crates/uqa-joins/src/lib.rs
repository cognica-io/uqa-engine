//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join algorithms across relational, text, vector, and graph paradigms.
//!
//! The crate ships two complementary surfaces:
//!
//! * Row-oriented relational joins ([`row_join`]) over
//!   `Vec<ResultRow>` -- hash inner / left / right / full, semi /
//!   anti, cross, sort-merge, and an index-backed inner join. These
//!   are what the SQL engine in `uqa-engine` calls into for row-tuple
//!   join shapes.
//! * Cross-paradigm joins ([`cross_paradigm`]) that bridge text,
//!   vector, hybrid, and graph posting lists.

#![allow(
    clippy::enum_glob_use,
    clippy::implicit_hasher,
    clippy::module_name_repetitions,
    clippy::needless_pass_by_value,
    clippy::similar_names,
    clippy::option_if_let_else,
    clippy::single_match_else,
    clippy::too_many_lines
)]

pub mod cross_paradigm;
pub mod row_join;

pub use cross_paradigm::{
    CrossParadigmJoin, GraphJoin, HybridJoin, TextSimilarityJoin, VectorSimilarityJoin,
};
pub use row_join::{
    anti_join, cross_join, full_outer_join, hash_inner_join, index_inner_join, left_outer_join,
    nested_loop_join, right_outer_join, semi_join, sort_merge_inner_join, JoinKey, JoinKind,
};
