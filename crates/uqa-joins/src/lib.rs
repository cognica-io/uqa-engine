//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Join algorithms across relational, text, vector, and graph paradigms.

pub mod cross_paradigm;

pub use cross_paradigm::{
    CrossParadigmJoin, GraphJoin, HybridJoin, TextSimilarityJoin, VectorSimilarityJoin,
};
