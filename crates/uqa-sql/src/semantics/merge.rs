//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! MERGE clause scopes, target privileges, and positional RETURNING schemas.
use super::returning::expanded_returning_projections;
use crate::{
    plan::{MergePlan, MergeWhenPlan, ProjectionPlan},
    SQLError, SQLParam,
};
use std::collections::BTreeSet;

#[expect(
    clippy::too_many_lines,
    reason = "validates clause-specific schemas and FULL JOIN requirements"
)]
pub fn validate_merge_action_scopes(
    routines: &dyn crate::routines::RoutineResolution,
    stmt: &MergePlan,
    target_schema: &crate::RowSchema,
    source_schema: &crate::RowSchema,
    params: &[SQLParam],
    bindings: &crate::binding::context::BindingContext<'_>,
) -> Result<(), SQLError> {
    let matched_schema = crate::RowSchema::join(target_schema, source_schema, std::iter::empty());
    let expression_type = |expression: &crate::ScalarExpr, schema: &crate::RowSchema| {
        crate::binding::analyze_projection_output_schema(
            routines,
            &[ProjectionPlan {
                expr: expression.clone(),
                alias: None,
            }],
            schema,
            schema,
            &stmt.subqueries,
            params,
            bindings,
        )
        .map(|output| output.column_type(0).cloned())
    };
    let validate_boolean = |expression: &crate::ScalarExpr,
                            schema: &crate::RowSchema,
                            label: &str|
     -> Result<(), SQLError> {
        if expression_type(expression, schema)?
            .is_some_and(|ty| ty != crate::ast::ColumnType::Boolean)
        {
            return Err(SQLError::TypeMismatch(format!(
                "argument of {label} must be type boolean"
            )));
        }
        Ok(())
    };
    validate_boolean(&stmt.join_condition, &matched_schema, "MERGE ON")?;
    let has_source_missing = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::UpdateNotMatchedBySource { .. }
                | MergeWhenPlan::DeleteNotMatchedBySource { .. }
                | MergeWhenPlan::NothingNotMatchedBySource { .. }
        )
    });
    let has_target_missing = stmt.when_clauses.iter().any(|clause| {
        matches!(
            clause,
            MergeWhenPlan::InsertNotMatched { .. } | MergeWhenPlan::NothingNotMatched { .. }
        )
    });
    if has_source_missing
        && has_target_missing
        && !super::join_predicates::join_conjuncts(&stmt.join_condition)
            .into_iter()
            .any(|conjunct| {
                matches!(
                    conjunct,
                    crate::ScalarExpr::Binary {
                        op: crate::ast::BinaryOp::Equal,
                        lhs,
                        rhs,
                    } if super::join_predicates::decide_join_sides(
                        target_schema,
                        source_schema,
                        lhs,
                        rhs,
                    )
                    .is_some()
                )
            })
    {
        return Err(SQLError::Routine {
            sqlstate: "0A000".into(),
            message:
                "FULL JOIN is only supported with merge-joinable or hash-joinable join conditions"
                    .into(),
        });
    }
    for clause in &stmt.when_clauses {
        let (condition, expressions, schema): (
            Option<&crate::ScalarExpr>,
            Vec<&crate::ScalarExpr>,
            &crate::RowSchema,
        ) = match clause {
            MergeWhenPlan::UpdateMatched {
                condition,
                assignments,
            } => (
                condition.as_ref(),
                assignments
                    .iter()
                    .map(|assignment| &assignment.value)
                    .collect(),
                &matched_schema,
            ),
            MergeWhenPlan::DeleteMatched { condition }
            | MergeWhenPlan::NothingMatched { condition } => {
                (condition.as_ref(), Vec::new(), &matched_schema)
            }
            MergeWhenPlan::UpdateNotMatchedBySource {
                condition,
                assignments,
            } => (
                condition.as_ref(),
                assignments
                    .iter()
                    .map(|assignment| &assignment.value)
                    .collect(),
                target_schema,
            ),
            MergeWhenPlan::DeleteNotMatchedBySource { condition }
            | MergeWhenPlan::NothingNotMatchedBySource { condition } => {
                (condition.as_ref(), Vec::new(), target_schema)
            }
            MergeWhenPlan::InsertNotMatched {
                condition, values, ..
            } => (condition.as_ref(), values.iter().collect(), source_schema),
            MergeWhenPlan::NothingNotMatched { condition } => {
                (condition.as_ref(), Vec::new(), source_schema)
            }
        };
        if let Some(condition) = condition {
            validate_boolean(condition, schema, "WHEN")?;
        }
        for expression in expressions {
            expression_type(expression, schema)?;
        }
    }
    Ok(())
}

pub fn expanded_merge_returning_projections(
    catalog: &dyn super::returning::ReturningCatalog,
    target_table: &str,
    target_qualifier: &str,
    aliases: &crate::ast::ReturningAliases,
    source_schema: &crate::RowSchema,
    source_relation: crate::ast::InternalRelationId,
    returning: &[ProjectionPlan],
) -> Result<Vec<ProjectionPlan>, SQLError> {
    let target_star = ProjectionPlan {
        expr: crate::ScalarExpr::QualifiedStar(target_qualifier.into()),
        alias: None,
    };
    let target_projections = expanded_returning_projections(
        catalog,
        target_table,
        target_qualifier,
        aliases,
        std::slice::from_ref(&target_star),
    )?;
    let mut projections = Vec::new();
    for projection in returning {
        match &projection.expr {
            crate::ScalarExpr::Star => {
                projections.extend(
                    source_schema
                        .columns()
                        .iter()
                        .enumerate()
                        .filter(|(position, _)| {
                            super::projection::visible_projection_source_position(
                                source_schema,
                                *position,
                            )
                        })
                        .map(|(position, column)| ProjectionPlan {
                            expr: crate::ScalarExpr::InternalColumn(
                                source_relation.column(position),
                            ),
                            alias: Some(
                                source_schema
                                    .public_name(position)
                                    .unwrap_or(column)
                                    .to_string(),
                            ),
                        }),
                );
                projections.extend(target_projections.iter().cloned());
            }
            crate::ScalarExpr::QualifiedStar(qualifier)
                if qualifier == target_qualifier
                    || qualifier == &aliases.old
                    || qualifier == &aliases.new =>
            {
                projections.extend(expanded_returning_projections(
                    catalog,
                    target_table,
                    target_qualifier,
                    aliases,
                    std::slice::from_ref(projection),
                )?);
            }
            _ => projections.push(projection.clone()),
        }
    }
    Ok(projections)
}

pub fn merge_returning_source_schema(
    source_schema: &crate::RowSchema,
    source_relation: crate::ast::InternalRelationId,
) -> crate::RowSchema {
    let aliases = source_schema
        .columns()
        .iter()
        .enumerate()
        .map(|(position, _)| {
            (
                source_relation.column(position),
                source_schema
                    .physical_slot(position)
                    .expect("source column has a physical slot"),
                source_schema.column_type(position).cloned(),
            )
        })
        .collect::<Vec<_>>();
    crate::RowSchema::with_physical_internal_aliases(source_schema, &aliases)
}

pub fn ensure_merge_mutation_privileges(
    catalog: &dyn super::mutation_privileges::MutationPrivilegeCatalog,
    stmt: &MergePlan,
) -> Result<(), SQLError> {
    let mut column_privileges = BTreeSet::new();
    let mut requires_delete = false;
    let mut requires_any_insert = false;
    let table_columns = catalog.bound_table_column_names(&stmt.target)?;
    let privilege_subject = stmt
        .target_privilege_subject
        .clone()
        .unwrap_or_else(|| catalog.current_user_name());
    for clause in &stmt.when_clauses {
        match clause {
            MergeWhenPlan::InsertNotMatched {
                columns, values, ..
            } => {
                if columns.is_empty() && values.is_empty() {
                    requires_any_insert = true;
                } else {
                    let columns = if columns.is_empty() {
                        table_columns
                            .iter()
                            .take(values.len())
                            .cloned()
                            .collect::<Vec<_>>()
                    } else {
                        columns.clone()
                    };
                    column_privileges.extend(columns.into_iter().map(|column| {
                        (
                            crate::catalog::security::table::TableAclPrivilege::Insert,
                            column,
                        )
                    }));
                }
            }
            MergeWhenPlan::UpdateMatched { assignments, .. }
            | MergeWhenPlan::UpdateNotMatchedBySource { assignments, .. } => {
                column_privileges.extend(assignments.iter().map(|assignment| {
                    (
                        crate::catalog::security::table::TableAclPrivilege::Update,
                        assignment.column.clone(),
                    )
                }));
            }
            MergeWhenPlan::DeleteMatched { .. }
            | MergeWhenPlan::DeleteNotMatchedBySource { .. } => requires_delete = true,
            _ => {}
        }
    }
    if requires_delete {
        catalog.ensure_table_privilege_for(
            &stmt.target,
            &privilege_subject,
            crate::catalog::security::table::TableAclPrivilege::Delete,
        )?;
    }
    if requires_any_insert {
        catalog.ensure_any_column_privilege_for(
            &stmt.target,
            &privilege_subject,
            crate::catalog::security::table::TableAclPrivilege::Insert,
        )?;
    }
    for (privilege, column) in column_privileges {
        catalog.ensure_column_privilege_for(
            &stmt.target,
            &column,
            &privilege_subject,
            privilege,
        )?;
    }
    Ok(())
}

pub fn merge_returning_schema(
    routines: &dyn crate::routines::RoutineResolution,
    catalog: &dyn super::returning::ReturningCatalog,
    stmt: &MergePlan,
    params: &[SQLParam],
    source_schema: &crate::RowSchema,
    ctes: &crate::binding::context::BindingContext<'_>,
) -> Result<Option<crate::RowSchema>, SQLError> {
    if stmt.returning.is_empty() {
        return Ok(None);
    }
    let source_relation = crate::ast::InternalRelationId::allocate();
    let projections = expanded_merge_returning_projections(
        catalog,
        &stmt.target,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        source_schema,
        source_relation,
        &stmt.returning,
    )?;
    let returning_source_schema = merge_returning_source_schema(source_schema, source_relation);
    let star_schema = super::returning::returning_target_schema(catalog, &stmt.target)?;
    let expression_schema = super::returning_expression_schema(
        &star_schema,
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        Some(&returning_source_schema),
    );
    crate::binding::analyze_projection_output_schema(
        routines,
        &projections,
        &expression_schema,
        &star_schema,
        &stmt.subqueries,
        params,
        ctes,
    )
    .map(Some)
}

pub fn merge_command_returning_schema(
    routines: &dyn crate::routines::RoutineResolution,
    catalog: &dyn super::returning::ReturningCatalog,
    rows: &dyn super::mutation_rows::MutationRowCatalog,
    stmt: &MergePlan,
    params: &[SQLParam],
    bindings: &crate::binding::context::BindingContext<'_>,
) -> Result<Option<crate::RowSchema>, SQLError> {
    if stmt.returning.is_empty() {
        return Ok(None);
    }
    let source =
        crate::binding::analyze_source_plan_schema(routines, &stmt.source, params, bindings, None)?;
    super::returning::validate_returning_alias_relations(
        &stmt.target_qualifier,
        &stmt.returning_aliases,
        Some(&source),
    )?;
    let target =
        super::mutation_rows::null_target_schema(rows, &stmt.target, &stmt.target_qualifier)?;
    validate_merge_action_scopes(routines, stmt, &target, &source, params, bindings)?;
    merge_returning_schema(routines, catalog, stmt, params, &source, bindings)
}
