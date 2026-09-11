//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

pub(crate) use uqa_sql::binding::view_dependencies::{
    bind_cte_plan_relations, bind_query_plan_relations, bind_query_plan_sequence_references,
    canonical_virtual_relation_reference, query_plan_has_legacy_routine_identity,
    query_plan_references_function, query_plan_references_relation, query_plan_references_sequence,
    rewrite_query_plan_routine_identity,
};

#[cfg(test)]
pub(crate) use uqa_sql::binding::view_dependencies::sequence_function_reference_mut;
