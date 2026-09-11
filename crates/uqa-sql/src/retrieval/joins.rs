//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation-producing operator join validation and logical operands.

use super::{
    const_f64, const_optional_string, const_string, lower_where_bound, BindingResult,
    RetrievalArguments, RetrievalConstants, RetrievalExpr, SQLError, ScalarExpr,
};

fn lower_join_operand(
    source: &dyn RetrievalArguments,
    expression: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
    function_name: &str,
) -> BindingResult<RetrievalExpr> {
    lower_where_bound(source, expression, constants)?.ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{function_name} operand cannot be represented by the operator IR"
        ))
    })
}

fn const_join_threshold(
    expression: &ScalarExpr,
    constants: &RetrievalConstants<'_>,
    function_name: &str,
    minimum: f64,
    maximum: f64,
) -> BindingResult<f64> {
    let threshold = const_f64(expression, constants).ok_or_else(|| {
        SQLError::TypeMismatch(format!(
            "{function_name}.threshold must be a constant number"
        ))
    })?;
    if !threshold.is_finite() || !(minimum..=maximum).contains(&threshold) {
        return Err(SQLError::TypeMismatch(format!(
            "{function_name}.threshold must be finite and in [{minimum}, {maximum}], got {threshold}"
        )));
    }
    Ok(threshold)
}

pub fn lower_operator_join_table_function(
    source: &dyn RetrievalArguments,
    name: &str,
    relations: Option<&crate::ast::OperatorJoinRelations>,
    args: &[ScalarExpr],
    constants: &RetrievalConstants<'_>,
) -> BindingResult<(crate::ast::OperatorJoinRelations, RetrievalExpr)> {
    let expected = match name {
        "text_similarity_join" | "vector_similarity_join" => 5,
        "graph_join" => 6,
        "hybrid_join" | "cross_paradigm_join" => 4,
        _ => {
            return Err(SQLError::Unsupported(format!(
                "operator join table function `{name}`"
            )))
        }
    };
    let relations = relations.ok_or_else(|| {
        SQLError::TypeMismatch(format!("{name} requires left and right table identifiers"))
    })?;
    let actual = args.len() + 2;
    if actual != expected {
        return Err(SQLError::BadArity {
            name: name.to_string(),
            expected: expected.to_string(),
            actual,
        });
    }
    let left = lower_join_operand(source, &args[0], constants, name)?;
    let right = lower_join_operand(source, &args[1], constants, name)?;
    let tree = match name {
        "text_similarity_join" => RetrievalExpr::TextSimilarityJoin {
            left: Box::new(left),
            right: Box::new(right),
            threshold: const_join_threshold(&args[2], constants, "text_similarity_join", 0.0, 1.0)?,
        },
        "vector_similarity_join" => RetrievalExpr::VectorSimilarityJoin {
            left: Box::new(left),
            right: Box::new(right),
            threshold: const_join_threshold(
                &args[2],
                constants,
                "vector_similarity_join",
                -1.0,
                1.0,
            )?,
        },
        "graph_join" => RetrievalExpr::GraphJoin {
            left: Box::new(left),
            right: Box::new(right),
            label: const_optional_string(&args[2], constants)
                .ok_or_else(|| {
                    SQLError::TypeMismatch(
                        "graph_join.label must be a constant string or NULL".into(),
                    )
                })?
                .into_option(),
            graph: const_string(&args[3], constants).ok_or_else(|| {
                SQLError::TypeMismatch("graph_join.graph must be a constant string".into())
            })?,
        },
        "hybrid_join" => RetrievalExpr::HybridJoin {
            left: Box::new(left),
            right: Box::new(right),
        },
        "cross_paradigm_join" => RetrievalExpr::CrossParadigmJoin {
            left: Box::new(left),
            right: Box::new(right),
        },
        _ => unreachable!("operator join name validated above"),
    };
    Ok((relations.clone(), tree))
}
