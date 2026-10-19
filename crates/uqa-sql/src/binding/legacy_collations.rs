//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Calls stored before each call recorded its input collation.
//!
//! A definition stored by an earlier release records no selection on its calls, which would then run under the database default collation. Loading derives the selection `PostgreSQL` records when it creates the definition for each view, CHECK constraint, default, rule and trigger condition these predicates find. Once derived, a call keeps its selection, so a later load finds nothing to derive.

use crate::{
    ast::{Expr, FunctionBinding, FunctionDispatch},
    plan::QueryPlan,
    ScalarExpr,
};

/// Whether a stored call reads its input collation but records no selection.
#[must_use]
pub fn call_lacks_input_collation(name: &str, binding: Option<&FunctionBinding>) -> bool {
    match binding {
        Some(binding) => binding.input_collation.is_none() && reads_input_collation(binding),
        // `lower` and `upper` of a range stay unbound by design, so only a name without such an overload identifies an unbound call stored by an earlier release.
        None => matches!(
            local_name(name),
            "starts_with"
                | "initcap"
                | "casefold"
                | "ilike"
                | "similar_to"
                | "regex_match_op"
                | "regex_imatch_op"
                | "regexp_count"
                | "regexp_instr"
                | "regexp_like"
                | "regexp_match"
                | "regexp_matches"
                | "regexp_replace"
                | "regexp_split_to_array"
                | "regexp_split_to_table"
                | "regexp_substr"
        ),
    }
}

/// Whether a stored expression has a call that [`call_lacks_input_collation`] finds.
#[must_use]
pub fn expression_lacks_input_collation(expression: &Expr) -> bool {
    expression.any_node(&|node| {
        matches!(node, Expr::Func { name, binding, .. } if call_lacks_input_collation(name, binding.as_ref()))
    })
}

/// Whether a stored query has a call that [`call_lacks_input_collation`] finds, including the calls of its subqueries.
#[must_use]
pub fn query_lacks_input_collation(query: &QueryPlan) -> bool {
    let mut lacking = false;
    query.visit_scalar_expressions(&mut |root| {
        root.visit(&mut |node| {
            if let ScalarExpr::Func { name, binding, .. } = node {
                lacking |= call_lacks_input_collation(name, binding.as_ref());
            }
        });
    });
    lacking
}

/// Case mapping, ILIKE, the regular-expression operators and functions, and the pattern forms of `substring`, whose results depend on the input collation's provider. LIKE compares characters exactly under every built-in collation.
fn reads_input_collation(binding: &FunctionBinding) -> bool {
    match binding.dispatch {
        Some(FunctionDispatch::Pattern { operator, .. }) => {
            operator.case_insensitive() || !operator.is_like()
        }
        Some(FunctionDispatch::RegexSubstring | FunctionDispatch::SimilarSubstring) => true,
        Some(_) => false,
        None => {
            binding.builtin
                && matches!(
                    local_name(&binding.name),
                    "starts_with"
                        | "lower"
                        | "upper"
                        | "initcap"
                        | "casefold"
                        | "regexp_count"
                        | "regexp_instr"
                        | "regexp_like"
                        | "regexp_match"
                        | "regexp_matches"
                        | "regexp_replace"
                        | "regexp_split_to_array"
                        | "regexp_split_to_table"
                        | "regexp_substr"
                )
        }
    }
}

fn local_name(name: &str) -> &str {
    name.strip_prefix("pg_catalog.").unwrap_or(name)
}

#[cfg(test)]
mod tests;
