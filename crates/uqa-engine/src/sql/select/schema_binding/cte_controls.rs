//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Static row types and validation for recursive CTE control columns.

use super::{CteScope, Engine, QueryPlan, RowSchema, SQLError, SQLParam, SchemaScope};
use uqa_sql::ast::ColumnType;

pub(super) fn hide_recursive_generated_schema(schema: &RowSchema, visible: usize) -> RowSchema {
    let visible = visible.min(schema.len());
    let base = RowSchema::with_types(
        schema.columns()[..visible].to_vec(),
        schema.column_types()[..visible].to_vec(),
    );
    let generated = schema
        .columns()
        .iter()
        .enumerate()
        .skip(visible)
        .map(|(position, column)| {
            (
                uqa_execution::ColumnIdentity::unqualified(column),
                schema.column_type(position).cloned(),
            )
        })
        .collect::<Vec<_>>();
    RowSchema::with_typed_virtual_identities(&base, &generated)
}

pub(in crate::sql) fn extend_cte_generated_schema(
    engine: &Engine,
    cte: &uqa_planner::CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
) -> Result<RowSchema, SQLError> {
    extend_cte_generated_schema_mode(engine, cte, schema, params, true)
}

fn extend_cte_generated_schema_mode(
    engine: &Engine,
    cte: &uqa_planner::CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
    reject_output_conflicts: bool,
) -> Result<RowSchema, SQLError> {
    if cte.search.is_none() && cte.cycle.is_none() {
        return Ok(schema);
    }
    let mut columns = schema.columns().to_vec();
    let mut types = schema.column_types().to_vec();
    let base_columns = columns.clone();
    let require_columns = |requested: &[String], kind: &str| -> Result<(), SQLError> {
        let mut seen = std::collections::BTreeSet::new();
        for column in requested {
            if !seen.insert(column) {
                return Err(SQLError::Routine {
                    sqlstate: "42701".into(),
                    message: format!("{kind} column \"{column}\" specified more than once"),
                });
            }
            if !base_columns.iter().any(|candidate| candidate == column) {
                return Err(SQLError::Routine {
                    sqlstate: "42601".into(),
                    message: format!("{kind} column \"{column}\" not in WITH query column list"),
                });
            }
        }
        Ok(())
    };
    let reject_conflict = |name: &str, kind: &str| -> Result<(), SQLError> {
        if reject_output_conflicts && base_columns.iter().any(|column| column == name) {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!(
                    "{kind} column name \"{name}\" already used in WITH query column list"
                ),
            });
        }
        Ok(())
    };
    if let Some(search) = &cte.search {
        require_columns(&search.columns, "search")?;
        reject_conflict(&search.sequence_column, "search sequence")?;
        columns.push(search.sequence_column.clone());
        types.push(Some(if search.breadth_first {
            ColumnType::Record
        } else {
            ColumnType::Array(Box::new(ColumnType::Record))
        }));
    }
    if let Some(cycle) = &cte.cycle {
        require_columns(&cycle.columns, "cycle")?;
        if cycle.mark_column == cycle.path_column {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "cycle mark column name and cycle path column name are the same".into(),
            });
        }
        reject_conflict(&cycle.mark_column, "cycle mark")?;
        reject_conflict(&cycle.path_column, "cycle path")?;
        let empty = RowSchema::default();
        let mark_type = match (
            uqa_execution::scalar_type_with_resolver(&cycle.mark_value, &empty, params, engine)?,
            uqa_execution::scalar_type_with_resolver(&cycle.mark_default, &empty, params, engine)?,
        ) {
            (Some(left), Some(right)) => Some(uqa_execution::common_type(&left, &right)?),
            (left @ Some(_), None) | (None, left @ Some(_)) => left,
            (None, None) => None,
        };
        if let Some(mark_type) = mark_type.as_ref() {
            uqa_execution::equality_operand_type(mark_type, mark_type)?;
        }
        for column in &cycle.columns {
            if let Some(column_type) = base_columns
                .iter()
                .position(|candidate| candidate == column)
                .and_then(|position| schema.column_type(position))
            {
                uqa_execution::equality_operand_type(column_type, column_type)?;
            }
        }
        columns.push(cycle.mark_column.clone());
        types.push(mark_type);
        columns.push(cycle.path_column.clone());
        types.push(Some(ColumnType::Array(Box::new(ColumnType::Record))));
    }
    Ok(RowSchema::with_types(columns, types))
}

pub(in crate::sql) fn extend_recursive_cte_binding_schema(
    engine: &Engine,
    cte: &uqa_planner::CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
) -> Result<RowSchema, SQLError> {
    let extended = extend_cte_generated_schema_mode(engine, cte, schema.clone(), params, false)?;
    let generated = extended
        .columns()
        .iter()
        .enumerate()
        .skip(schema.len())
        .map(|(position, column)| {
            (
                uqa_execution::ColumnIdentity::unqualified(column),
                extended.column_type(position).cloned(),
            )
        })
        .collect::<Vec<_>>();
    Ok(RowSchema::with_typed_conflicting_virtual_identities(
        &schema, &generated,
    ))
}

pub(in crate::sql) fn analyze_recursive_control_step(
    engine: &Engine,
    cte: &uqa_planner::CtePlan,
    step: &QueryPlan,
    base_schema: RowSchema,
    params: &[SQLParam],
    ctes: &CteScope,
) -> Result<(), SQLError> {
    let provisional = extend_recursive_cte_binding_schema(engine, cte, base_schema, params)?;
    let mut scope = SchemaScope::for_analysis(ctes);
    scope.ctes.insert(cte.name.clone(), provisional);
    scope.bind_query(engine, step, params, None).map(|_| ())
}
