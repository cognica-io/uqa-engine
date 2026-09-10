//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Required event-row columns and declared types for rewritten INSERT inputs.
use crate::{
    plan::InsertPlan,
    semantics::{
        rules::analysis::RuleAnalysisContext,
        view_rewrite::context::{stored_view_schema, ViewRewriteContext},
    },
    ColumnType, SQLError,
};
use std::collections::BTreeSet;
pub fn view_rule_insert_column_type(
    context: ViewRewriteContext<'_>,
    stmt: &InsertPlan,
    input_position: usize,
) -> Result<Option<ColumnType>, SQLError> {
    for plan in &stmt.view_rule_insert_plans {
        let Some(column) = plan.supplied_columns.get(input_position) else {
            continue;
        };
        let definition = context
            .catalog
            .view_definition(&plan.relation)?
            .ok_or_else(|| SQLError::UnknownTable(plan.relation.clone()))?;
        let schema = stored_view_schema(context, &definition)?;
        let Some(position) =
            schema
                .columns()
                .iter()
                .enumerate()
                .find_map(|(position, internal)| {
                    let public = schema.public_name(position).unwrap_or(internal);
                    (public == column).then_some(position)
                })
        else {
            return Err(SQLError::UnknownColumn(format!(
                "{}.{}",
                plan.relation, column
            )));
        };
        return Ok(schema.column_type(position).cloned());
    }
    Ok(None)
}

pub fn required_view_rule_insert_input_positions(
    context: RuleAnalysisContext<'_>,
    stmt: &InsertPlan,
) -> Result<Option<BTreeSet<usize>>, SQLError> {
    let mut required = BTreeSet::new();
    for plan in &stmt.view_rule_insert_plans {
        let Some(columns) = super::analysis::relation_rule_row_columns(
            context,
            &plan.relation,
            crate::ast::RuleEvent::Insert,
        )?
        else {
            return Ok(None);
        };
        required.extend(
            plan.supplied_columns
                .iter()
                .enumerate()
                .filter_map(|(position, column)| columns.contains(column).then_some(position)),
        );
        if super::analysis::relation_suppresses_original_query(
            context,
            &plan.relation,
            crate::ast::RuleEvent::Insert,
        )? {
            break;
        }
    }
    Ok(Some(required))
}
