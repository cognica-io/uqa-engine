//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::collections::BTreeSet;

use super::{
    query_may_mutate_engine_inner, MutabilityClassification, QueryEffectContext, SQLError,
};

fn lowered_statement_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    statement: crate::ast::Statement,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let plan = crate::plan::UnifiedPlan::lower_with(statement, &|name: &str| {
        context.catalog.has_registered_aggregate_function(name)
    });
    match plan {
        crate::plan::UnifiedPlan::Query(query) => query_may_mutate_engine_inner(
            context,
            &query,
            visiting_views,
            visiting_routines,
            classification,
        ),
        crate::plan::UnifiedPlan::Command(_) => Ok(true),
    }
}

fn plpgsql_expression_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    expression: &crate::ast::Expr,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    lowered_statement_may_mutate_engine(
        context,
        crate::ast::Statement::Values {
            rows: vec![vec![expression.clone()]],
        },
        visiting_views,
        visiting_routines,
        classification,
    )
}

fn plpgsql_expressions_may_mutate_engine<'a>(
    context: &QueryEffectContext<'_>,
    expressions: impl IntoIterator<Item = &'a crate::ast::Expr>,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    for expression in expressions {
        if plpgsql_expression_may_mutate_engine(
            context,
            expression,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plpgsql_return_value_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    value: Option<&crate::plpgsql::PLpgSQLReturnValue>,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    let Some(crate::plpgsql::PLpgSQLReturnValue::Expr(expression)) = value else {
        return Ok(false);
    };
    plpgsql_expression_may_mutate_engine(
        context,
        expression,
        visiting_views,
        visiting_routines,
        classification,
    )
}

fn plpgsql_statement_list_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    datums: &[crate::plpgsql::PLpgSQLDatum],
    statements: &[crate::plpgsql::PLpgSQLStmt],
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    for statement in statements {
        if plpgsql_statement_may_mutate_engine(
            context,
            datums,
            statement,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plpgsql_block_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    datums: &[crate::plpgsql::PLpgSQLDatum],
    block: &crate::plpgsql::PLpgSQLBlock,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    if classification.procedural_state_requires_transaction && !block.exceptions.is_empty() {
        return Ok(true);
    }
    if plpgsql_statement_list_may_mutate_engine(
        context,
        datums,
        &block.body,
        visiting_views,
        visiting_routines,
        classification,
    )? {
        return Ok(true);
    }
    for arm in &block.exceptions {
        if plpgsql_statement_list_may_mutate_engine(
            context,
            datums,
            &arm.body,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves PL/pgSQL statement coverage"
)]
fn plpgsql_statement_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    datums: &[crate::plpgsql::PLpgSQLDatum],
    statement: &crate::plpgsql::PLpgSQLStmt,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    use crate::plpgsql::PLpgSQLStmt;
    match statement {
        PLpgSQLStmt::Block(block) => plpgsql_block_may_mutate_engine(
            context,
            datums,
            block,
            visiting_views,
            visiting_routines,
            classification,
        ),
        PLpgSQLStmt::Assign { expr, .. } => plpgsql_expression_may_mutate_engine(
            context,
            expr,
            visiting_views,
            visiting_routines,
            classification,
        ),
        PLpgSQLStmt::If {
            cond,
            then_body,
            elsifs,
            else_body,
        } => {
            if plpgsql_expression_may_mutate_engine(
                context,
                cond,
                visiting_views,
                visiting_routines,
                classification,
            )? || plpgsql_statement_list_may_mutate_engine(
                context,
                datums,
                then_body,
                visiting_views,
                visiting_routines,
                classification,
            )? {
                return Ok(true);
            }
            for (condition, body) in elsifs {
                if plpgsql_expression_may_mutate_engine(
                    context,
                    condition,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? || plpgsql_statement_list_may_mutate_engine(
                    context,
                    datums,
                    body,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
            else_body.as_deref().map_or(Ok(false), |body| {
                plpgsql_statement_list_may_mutate_engine(
                    context,
                    datums,
                    body,
                    visiting_views,
                    visiting_routines,
                    classification,
                )
            })
        }
        PLpgSQLStmt::Case {
            t_expr,
            arms,
            else_body,
            ..
        } => {
            if let Some(expression) = t_expr {
                if plpgsql_expression_may_mutate_engine(
                    context,
                    expression,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
            for (condition, body) in arms {
                if plpgsql_expression_may_mutate_engine(
                    context,
                    condition,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? || plpgsql_statement_list_may_mutate_engine(
                    context,
                    datums,
                    body,
                    visiting_views,
                    visiting_routines,
                    classification,
                )? {
                    return Ok(true);
                }
            }
            else_body.as_deref().map_or(Ok(false), |body| {
                plpgsql_statement_list_may_mutate_engine(
                    context,
                    datums,
                    body,
                    visiting_views,
                    visiting_routines,
                    classification,
                )
            })
        }
        PLpgSQLStmt::Loop { body, .. } => plpgsql_statement_list_may_mutate_engine(
            context,
            datums,
            body,
            visiting_views,
            visiting_routines,
            classification,
        ),
        PLpgSQLStmt::While { cond, body, .. } => Ok(plpgsql_expression_may_mutate_engine(
            context,
            cond,
            visiting_views,
            visiting_routines,
            classification,
        )?
            || plpgsql_statement_list_may_mutate_engine(
                context,
                datums,
                body,
                visiting_views,
                visiting_routines,
                classification,
            )?),
        PLpgSQLStmt::ForI {
            lower,
            upper,
            step,
            body,
            ..
        } => Ok(plpgsql_expressions_may_mutate_engine(
            context,
            [Some(lower), Some(upper), step.as_ref()]
                .into_iter()
                .flatten(),
            visiting_views,
            visiting_routines,
            classification,
        )? || plpgsql_statement_list_may_mutate_engine(
            context,
            datums,
            body,
            visiting_views,
            visiting_routines,
            classification,
        )?),
        PLpgSQLStmt::ForQuery { query, body, .. } => Ok(lowered_statement_may_mutate_engine(
            context,
            query.clone(),
            visiting_views,
            visiting_routines,
            classification,
        )?
            || plpgsql_statement_list_may_mutate_engine(
                context,
                datums,
                body,
                visiting_views,
                visiting_routines,
                classification,
            )?),
        PLpgSQLStmt::ForDynamic { .. } => Ok(true),
        PLpgSQLStmt::ForCursor {
            cursor,
            arguments,
            body,
            ..
        } => {
            if classification.procedural_state_requires_transaction
                || plpgsql_expressions_may_mutate_engine(
                    context,
                    arguments.iter().map(|argument| &argument.expr),
                    visiting_views,
                    visiting_routines,
                    classification,
                )?
            {
                return Ok(true);
            }
            let query = datums.get(*cursor).and_then(|datum| match datum {
                crate::plpgsql::PLpgSQLDatum::Var(variable) => {
                    variable.cursor.as_ref().map(|cursor| &cursor.query)
                }
                _ => None,
            });
            Ok(query.map_or(Ok(false), |query| {
                lowered_statement_may_mutate_engine(
                    context,
                    query.clone(),
                    visiting_views,
                    visiting_routines,
                    classification,
                )
            })? || plpgsql_statement_list_may_mutate_engine(
                context,
                datums,
                body,
                visiting_views,
                visiting_routines,
                classification,
            )?)
        }
        PLpgSQLStmt::ForeachArray { expr, body, .. } => Ok(plpgsql_expression_may_mutate_engine(
            context,
            expr,
            visiting_views,
            visiting_routines,
            classification,
        )?
            || plpgsql_statement_list_may_mutate_engine(
                context,
                datums,
                body,
                visiting_views,
                visiting_routines,
                classification,
            )?),
        PLpgSQLStmt::Exit { cond, .. } => cond.as_ref().map_or(Ok(false), |condition| {
            plpgsql_expression_may_mutate_engine(
                context,
                condition,
                visiting_views,
                visiting_routines,
                classification,
            )
        }),
        PLpgSQLStmt::Return { value } | PLpgSQLStmt::ReturnNext { value } => {
            plpgsql_return_value_may_mutate_engine(
                context,
                value.as_ref(),
                visiting_views,
                visiting_routines,
                classification,
            )
        }
        PLpgSQLStmt::ReturnQuery { query }
        | PLpgSQLStmt::ExecSQL { stmt: query, .. }
        | PLpgSQLStmt::Perform { query } => lowered_statement_may_mutate_engine(
            context,
            query.clone(),
            visiting_views,
            visiting_routines,
            classification,
        ),
        PLpgSQLStmt::ReturnQueryExecute { .. } | PLpgSQLStmt::DynExecute { .. } => Ok(true),
        PLpgSQLStmt::Raise { params, .. } => plpgsql_expressions_may_mutate_engine(
            context,
            params,
            visiting_views,
            visiting_routines,
            classification,
        ),
        PLpgSQLStmt::Assert { condition, message } => plpgsql_expressions_may_mutate_engine(
            context,
            [Some(condition), message.as_ref()].into_iter().flatten(),
            visiting_views,
            visiting_routines,
            classification,
        ),
        PLpgSQLStmt::OpenCursor { cursor, open } => {
            if classification.procedural_state_requires_transaction {
                return Ok(true);
            }
            match open {
                crate::plpgsql::PLpgSQLCursorOpen::Bound { arguments } => {
                    if plpgsql_expressions_may_mutate_engine(
                        context,
                        arguments.iter().map(|argument| &argument.expr),
                        visiting_views,
                        visiting_routines,
                        classification,
                    )? {
                        return Ok(true);
                    }
                    let query = datums.get(*cursor).and_then(|datum| match datum {
                        crate::plpgsql::PLpgSQLDatum::Var(variable) => {
                            variable.cursor.as_ref().map(|cursor| &cursor.query)
                        }
                        _ => None,
                    });
                    query.map_or(Ok(false), |query| {
                        lowered_statement_may_mutate_engine(
                            context,
                            query.clone(),
                            visiting_views,
                            visiting_routines,
                            classification,
                        )
                    })
                }
                crate::plpgsql::PLpgSQLCursorOpen::Static { query, .. } => {
                    lowered_statement_may_mutate_engine(
                        context,
                        *query.clone(),
                        visiting_views,
                        visiting_routines,
                        classification,
                    )
                }
                crate::plpgsql::PLpgSQLCursorOpen::Dynamic { .. } => Ok(true),
            }
        }
        PLpgSQLStmt::FetchCursor { count, .. } | PLpgSQLStmt::MoveCursor { count, .. } => {
            if classification.procedural_state_requires_transaction {
                return Ok(true);
            }
            match count {
                crate::plpgsql::PLpgSQLCursorCount::Constant(_) => Ok(false),
                crate::plpgsql::PLpgSQLCursorCount::Expression(expression) => {
                    plpgsql_expression_may_mutate_engine(
                        context,
                        expression,
                        visiting_views,
                        visiting_routines,
                        classification,
                    )
                }
            }
        }
        PLpgSQLStmt::CloseCursor { .. } => Ok(classification.procedural_state_requires_transaction),
        PLpgSQLStmt::Commit { .. } | PLpgSQLStmt::Rollback { .. } => Ok(classification
            .include_session_mutations
            || classification.procedural_state_requires_transaction),
        PLpgSQLStmt::GetDiagnostics { .. } => Ok(false),
    }
}

pub(super) fn plpgsql_function_may_mutate_engine(
    context: &QueryEffectContext<'_>,
    function: &crate::plpgsql::PLpgSQLFunction,
    visiting_views: &mut BTreeSet<String>,
    visiting_routines: &mut BTreeSet<String>,
    classification: MutabilityClassification,
) -> Result<bool, SQLError> {
    for datum in &function.datums {
        let crate::plpgsql::PLpgSQLDatum::Var(variable) = datum else {
            continue;
        };
        if super::domains::named_type_coercion_may_mutate(
            context,
            &variable.type_name,
            visiting_views,
            visiting_routines,
            classification,
        )? {
            return Ok(true);
        }
        if let Some(expression) = &variable.default {
            if plpgsql_expression_may_mutate_engine(
                context,
                expression,
                visiting_views,
                visiting_routines,
                classification,
            )? {
                return Ok(true);
            }
        }
    }
    plpgsql_block_may_mutate_engine(
        context,
        &function.datums,
        &function.action,
        visiting_views,
        visiting_routines,
        classification,
    )
}
