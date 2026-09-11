//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SQL retrieval argument binding and lowering to runtime-independent expressions.

mod binding;
mod calls;
mod constants;
mod fusion;
mod graph;
mod ir;
mod joins;
mod predicates;
use crate::semantics::graph_functions::{
    default_graph_name as default_operator_graph, GraphNameCatalog,
};
use crate::{ast::BinaryOp, SQLError, SQLParam, ScalarExpr};
pub use binding::{lower_sql_function_bound, lower_where_bound};
pub use ir::{AttentionSpec, MultiStageEntry, RetrievalExpr, TextScoringMode};
pub use joins::lower_operator_join_table_function;
use std::collections::BTreeSet;
use uqa_core::{
    retrieval::{Direction as DeepGraphDirection, ExternalPriorMode, GatingSpec, MultiStageCutoff},
    Predicate, Value,
};
type BindingResult<T> = Result<T, SQLError>;

/// Evaluates a scalar with the caller's parameter values and without runtime function hooks.
pub type ConstantEvaluator<'a> = dyn Fn(&ScalarExpr, &[SQLParam]) -> Result<Value, SQLError> + 'a;
pub struct RetrievalConstants<'a> {
    pub params: &'a [SQLParam],
    pub evaluate: &'a ConstantEvaluator<'a>,
}
impl RetrievalConstants<'_> {
    fn without_parameters(&self) -> RetrievalConstants<'_> {
        RetrievalConstants {
            params: &[],
            evaluate: self.evaluate,
        }
    }
}
/// Runtime scalar evaluation and graph catalog access; SQL owns recursion and validation order.
pub trait RetrievalArguments: GraphNameCatalog {
    fn evaluate_argument(
        &self,
        expression: &ScalarExpr,
        params: &[SQLParam],
    ) -> Result<Value, SQLError>;
}

use calls::{
    bind_operator_argument, checked_retrieval_call_tree_present, lower_bayesian_match_with_prior,
    lower_calibrated_vector_match, lower_multi_field_match, lower_operator_arg, lower_signal_arg,
    lower_staged_retrieval, try_lower_fts_match, try_lower_knn_match, try_lower_text_match,
    validate_checked_retrieval_call_tree, validate_operator_function_arity,
    validate_probability_signal_contract,
};
use constants::{
    const_bool, const_f64, const_f64_vector, const_gating, const_optional_string, const_string,
    const_temporal_bound, const_usize, const_value, const_vector, named_arg_expr,
};
use fusion::{
    lower_bayesian_evidence_fusion, lower_learned_fusion, lower_positive_evidence_pool,
    try_lower_attention_fusion,
};
use graph::lower_graph_function;
use predicates::{column_name, lower_comparison, lower_document_boolean, lower_function};

enum OptionalStringConstant {
    Null,
    Value(String),
}
impl OptionalStringConstant {
    fn into_option(self) -> Option<String> {
        match self {
            Self::Null => None,
            Self::Value(value) => Some(value),
        }
    }
}

/// Lower representable SQL predicates using the supplied constant evaluator. Unsupported scalar shapes remain relational predicates.
pub fn lower_where(expr: &ScalarExpr, constants: &RetrievalConstants<'_>) -> Option<RetrievalExpr> {
    match expr {
        ScalarExpr::And(parts) => {
            let mut out: Vec<RetrievalExpr> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, constants)?);
            }
            Some(lower_document_boolean(out, false))
        }
        ScalarExpr::Or(parts) => {
            let mut out: Vec<RetrievalExpr> = Vec::with_capacity(parts.len());
            for p in parts {
                out.push(lower_where(p, constants)?);
            }
            Some(lower_document_boolean(out, true))
        }
        // Complement is only sound when the inner predicate cannot be
        // NULL for any row (search functions, IS NULL tests). Column
        // comparisons under NOT fall through to the wildcard `None`
        // and keep three-valued semantics through the row-evaluator
        // relational evaluation: `NOT (col = 5)` must not match rows whose `col`
        // is NULL.
        ScalarExpr::Not(inner) if crate::semantics::expr_is_null_free(inner) => Some(
            RetrievalExpr::Complement(Box::new(lower_where(inner, constants)?)),
        ),
        ScalarExpr::Func { name, args, .. } => lower_function(name, args, constants),
        ScalarExpr::Binary { op, lhs, rhs } => lower_comparison(*op, lhs, rhs, constants),
        ScalarExpr::IsNull { expr, negated } => {
            let field = column_name(expr)?;
            let predicate = if *negated {
                Predicate::IsNotNull
            } else {
                Predicate::IsNull
            };
            Some(RetrievalExpr::Filter {
                field,
                predicate,
                source: None,
            })
        }
        ScalarExpr::Between { expr, low, high } => {
            let field = column_name(expr)?;
            let lo = const_value(low, constants)?;
            let hi = const_value(high, constants)?;
            Some(RetrievalExpr::Filter {
                field,
                predicate: Predicate::Between { low: lo, high: hi },
                source: None,
            })
        }
        ScalarExpr::InList {
            expr,
            list,
            negated,
        } => {
            let field = column_name(expr)?;
            let mut set: BTreeSet<Value> = BTreeSet::new();
            let mut has_null = false;
            for v in list {
                let value = const_value(v, constants)?;
                if matches!(value, Value::Null) {
                    has_null = true;
                    continue;
                }
                set.insert(value);
            }
            if *negated {
                // `col NOT IN (...)`: a NULL in the list means no row
                // can ever satisfy it; otherwise complement the match
                // set but keep NULL rows excluded (three-valued NOT).
                if has_null {
                    return Some(RetrievalExpr::Empty);
                }
                let filter = RetrievalExpr::Filter {
                    field: field.clone(),
                    predicate: Predicate::InSet(set),
                    source: None,
                };
                let not_null = RetrievalExpr::Filter {
                    field,
                    predicate: Predicate::IsNotNull,
                    source: None,
                };
                return Some(RetrievalExpr::Intersect(vec![
                    RetrievalExpr::Complement(Box::new(filter)),
                    not_null,
                ]));
            }
            Some(RetrievalExpr::Filter {
                field,
                predicate: Predicate::InSet(set),
                source: None,
            })
        }
        _ => None,
    }
}
