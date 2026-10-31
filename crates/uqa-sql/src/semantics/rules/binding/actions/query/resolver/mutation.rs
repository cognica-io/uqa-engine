//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! OLD/NEW mutation templates retain scalar namespaces and raise assignment diagnostics only when demanded.

use super::{bind_column_collation, insert_expression, OwnedRuleQueryResolver, RuleColumnTemplate};
use crate::{
    ast::{DeferredSQLError, Expr, InternalRelationId},
    plan::{copy::SyntaxProduction, MutableExpressionPlan, OwnedScalarExpr, ScalarQueryArena},
    semantics::{
        rules::{analysis::RuleAnalysisContext, binding::RuleMutation},
        view_rewrite::DeferredOuterRewrite,
    },
    SQLError, ScalarExpr,
};
use uqa_core::{
    memory::{OwnedMap, ProductionControl},
    Value,
};

impl OwnedRuleQueryResolver {
    pub(in crate::semantics::rules::binding::actions::query) fn from_mutation_with_control(
        context: RuleAnalysisContext<'_>,
        statement: RuleMutation<'_>,
        definitions: &[crate::ast::ColumnDef],
        relation: Option<InternalRelationId>,
        rewrite: Option<&DeferredOuterRewrite>,
        control: &ProductionControl<'_>,
        mut bind_input: impl FnMut(MutableExpressionPlan) -> Result<MutableExpressionPlan, SQLError>,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let output = SyntaxProduction::new(*control);
        let mut columns = output.vector(definitions.len())?;
        let mut expressions = OwnedMap::new();
        for (position, column) in definitions.iter().enumerate() {
            output.check()?;
            let old = match relation {
                Some(relation) => ScalarExpr::InternalColumn(relation.column(position))
                    .copy_for_rewrite(control)?,
                None => OwnedScalarExpr::column(
                    control.copy_text(&column.name)?,
                    Some(control.copy_text(statement.target_qualifier())?),
                    control,
                )?,
            };
            let old_reference = InternalRelationId::allocate().column(0);
            insert_expression(
                &mut expressions,
                old_reference,
                bind_input(MutableExpressionPlan::from_parts(
                    old.copy_for_rewrite(control)?,
                    ScalarQueryArena::copy_from(&[], control)?,
                    control,
                )?)?
                .finish(control)?,
                control,
            )?;
            let new = match statement {
                RuleMutation::Update(update) => {
                    super::super::assignments::update_column_expression_with_control(
                        context, update, column, old, control,
                    )
                }
                RuleMutation::Delete(_) => MutableExpressionPlan::from_parts(
                    OwnedScalarExpr::typed_literal(
                        control.copy_value(&Value::Null)?,
                        column.ty.catalog_name_with_control(control)?,
                        Some(column.ty.clone_with_control(control)?),
                        column.collation_oid,
                        None,
                        control,
                    )?,
                    ScalarQueryArena::copy_from(&[], control)?,
                    control,
                ),
            };
            let new = match new {
                Ok(expression) => {
                    let expression = bind_column_collation(expression, column, control)?;
                    let expression = if let Some(rewrite) = rewrite {
                        rewrite.bind_expression_owned(expression, control)?
                    } else {
                        expression
                    };
                    let reference = InternalRelationId::allocate().column(0);
                    insert_expression(
                        &mut expressions,
                        reference,
                        bind_input(expression)?.finish(control)?,
                        control,
                    )?;
                    Ok(Expr::InternalColumn(reference))
                }
                Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => {
                    return Err(error)
                }
                Err(error) => {
                    Err(output.take(DeferredSQLError::capture_with_control(&error, control)?))
                }
            };
            columns.push(RuleColumnTemplate {
                name: output.text(&column.name)?,
                old: Expr::InternalColumn(old_reference),
                new,
            })?;
        }
        let columns = columns.finish()?;
        Ok(Self {
            table: control.copy_text(statement.table())?,
            columns: output.finish(columns)?,
            expressions,
        })
    }
}
