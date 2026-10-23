//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Input-value traversal for output liveness, distinct from row-count demand.

pub(super) use uqa_sql::semantics::visit_input_values as visit;
