//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! View rewrite-rule input columns and types for INSERT.

use super::{BTreeSet, ColumnType, Engine, InsertPlan, SQLError};

pub(super) fn view_rule_insert_column_type(
    engine: &Engine,
    stmt: &InsertPlan,
    input_position: usize,
) -> Result<Option<ColumnType>, SQLError> {
    for plan in &stmt.view_rule_insert_plans {
        let Some(column) = plan.supplied_columns.get(input_position) else {
            continue;
        };
        let definition = engine
            .view_definition(&plan.relation)?
            .ok_or_else(|| SQLError::UnknownTable(plan.relation.clone()))?;
        let schema = engine.stored_view_schema(&definition)?;
        let Some(position) =
            schema
                .columns()
                .iter()
                .enumerate()
                .find_map(|(position, internal)| {
                    let public = schema.public_name(position).unwrap_or(internal);
                    public.eq_ignore_ascii_case(column).then_some(position)
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

pub(super) fn required_view_rule_insert_input_positions(
    engine: &Engine,
    stmt: &InsertPlan,
) -> Result<Option<BTreeSet<usize>>, SQLError> {
    let mut required = BTreeSet::new();
    for plan in &stmt.view_rule_insert_plans {
        let Some(columns) = crate::sql::rules::relation_rule_row_columns(
            engine,
            &plan.relation,
            uqa_sql::ast::RuleEvent::Insert,
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
        if crate::sql::rules::relation_suppresses_original_query(
            engine,
            &plan.relation,
            uqa_sql::ast::RuleEvent::Insert,
        )? {
            break;
        }
    }
    Ok(Some(required))
}
