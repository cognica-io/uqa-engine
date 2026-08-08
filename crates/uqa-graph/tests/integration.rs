//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Consolidated graph integration tests.

#[path = "adapters.rs"]
mod adapters;
#[path = "algebra.rs"]
mod algebra;
#[path = "centrality.rs"]
mod centrality;
#[path = "cross_paradigm.rs"]
mod cross_paradigm;
#[path = "cypher.rs"]
mod cypher;
#[path = "cypher_fuzz.rs"]
mod cypher_fuzz;
#[path = "cypher_round_trip.rs"]
mod cypher_round_trip;
#[path = "cypher_unicode.rs"]
mod cypher_unicode;
#[path = "cypher_write.rs"]
mod cypher_write;
#[path = "embedding_index.rs"]
mod embedding_index;
#[path = "filter_pushdown.rs"]
mod filter_pushdown;
#[path = "operators.rs"]
mod operators;
#[path = "pattern_negation.rs"]
mod pattern_negation;
#[path = "pattern_rename.rs"]
mod pattern_rename;
#[path = "rpq.rs"]
mod rpq;
#[path = "rpq_algebra.rs"]
mod rpq_algebra;
#[path = "temporal_versioned.rs"]
mod temporal_versioned;
