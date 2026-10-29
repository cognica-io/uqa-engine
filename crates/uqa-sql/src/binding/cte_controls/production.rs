//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! SEARCH and CYCLE declarations publish admitted result or hidden recursive metadata through one validation algorithm.

use crate::{
    plan::CtePlan, routines::RoutineResolution, ColumnType, RowSchema, SQLError, SQLParam,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap};

struct Generated<'a> {
    name: &'a str,
    ty: Option<Produced<ColumnType>>,
}

pub fn extend_cte_generated_schema_with_control(
    routines: &dyn RoutineResolution,
    cte: &CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    if cte.search.is_none() && cte.cycle.is_none() {
        control.check()?;
        return Ok(schema);
    }
    let fields = generated(routines, cte, &schema, params, true, control)?;
    RowSchema::with_output_metadata_with_control(
        None,
        schema
            .columns()
            .iter()
            .enumerate()
            .map(|(position, name)| {
                (
                    name.as_str(),
                    schema.column_type(position),
                    schema.column_collation(position),
                    schema.record_metadata(position).cloned(),
                )
            })
            .chain(
                fields
                    .iter()
                    .flatten()
                    .map(|field| (field.name, field.ty.as_deref(), None, None)),
            ),
        false,
        control,
    )
}

pub fn extend_recursive_cte_binding_schema_with_control(
    routines: &dyn RoutineResolution,
    cte: &CtePlan,
    schema: RowSchema,
    params: &[SQLParam],
    control: &ProductionControl<'_>,
) -> Result<RowSchema, SQLError> {
    let fields = generated(routines, cte, &schema, params, false, control)?;
    RowSchema::with_conflicting_virtual_columns_with_control(
        &schema,
        fields
            .iter()
            .flatten()
            .map(|field| (field.name, field.ty.as_deref())),
        control,
    )
}

fn generated<'a>(
    routines: &dyn RoutineResolution,
    cte: &'a CtePlan,
    schema: &RowSchema,
    params: &[SQLParam],
    reject_conflicts: bool,
    control: &ProductionControl<'_>,
) -> Result<[Option<Generated<'a>>; 3], SQLError> {
    control.check()?;
    let mut fields = [None, None, None];
    if let Some(search) = &cte.search {
        require_columns(&search.columns, "search", schema, control)?;
        reject_conflict(
            &search.sequence_column,
            "search sequence",
            schema,
            reject_conflicts,
        )?;
        fields[0] = Some(Generated {
            name: &search.sequence_column,
            ty: Some(if search.breadth_first {
                control.finish(ColumnType::Record, control.empty_reservation())?
            } else {
                record_array(control)?
            }),
        });
    }
    if let Some(cycle) = &cte.cycle {
        require_columns(&cycle.columns, "cycle", schema, control)?;
        if cycle.mark_column == cycle.path_column {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: "cycle mark column name and cycle path column name are the same".into(),
            });
        }
        reject_conflict(&cycle.mark_column, "cycle mark", schema, reject_conflicts)?;
        reject_conflict(&cycle.path_column, "cycle path", schema, reject_conflicts)?;
        let empty = RowSchema::default();
        let left = crate::type_resolution::common_context_expression_type_with_control(
            &cycle.mark_value,
            &empty,
            params,
            Some(routines),
            control,
        )?;
        let right = crate::type_resolution::common_context_expression_type_with_control(
            &cycle.mark_default,
            &empty,
            params,
            Some(routines),
            control,
        )?;
        let ty = match (left, right) {
            (Some(left), Some(right)) => Some(crate::type_resolution::common_type_in_with_control(
                crate::type_resolution::CommonTypeContext::Cycle,
                &left,
                &right,
                control,
            )?),
            (left @ Some(_), None) | (None, left @ Some(_)) => left,
            (None, None) => None,
        };
        if let Some(ty) = &ty {
            crate::type_resolution::equality_operand_type_with_control(ty, ty, control)?;
        }
        for name in &cycle.columns {
            control.check()?;
            if let Some(ty) = schema
                .columns()
                .iter()
                .position(|column| column == name)
                .and_then(|position| schema.column_type(position))
            {
                crate::type_resolution::equality_operand_type_with_control(ty, ty, control)?;
            }
        }
        fields[1] = Some(Generated {
            name: &cycle.mark_column,
            ty,
        });
        fields[2] = Some(Generated {
            name: &cycle.path_column,
            ty: Some(record_array(control)?),
        });
    }
    Ok(fields)
}

fn record_array(control: &ProductionControl<'_>) -> Result<Produced<ColumnType>, SQLError> {
    let memory = control.reserve(size_of::<ColumnType>())?;
    Ok(control.finish(ColumnType::Array(Box::new(ColumnType::Record)), memory)?)
}

fn require_columns(
    requested: &[String],
    kind: &str,
    schema: &RowSchema,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let mut seen = ProductionMap::new(*control);
    for column in requested {
        if seen.insert_copy(column.as_str(), ())?.is_some() {
            return Err(SQLError::Routine {
                sqlstate: "42701".into(),
                message: format!("{kind} column \"{column}\" specified more than once"),
            });
        }
        if !schema.columns().iter().any(|name| name == column) {
            return Err(SQLError::Routine {
                sqlstate: "42601".into(),
                message: format!("{kind} column \"{column}\" not in WITH query column list"),
            });
        }
    }
    Ok(())
}

fn reject_conflict(
    name: &str,
    kind: &str,
    schema: &RowSchema,
    reject: bool,
) -> Result<(), SQLError> {
    if reject && schema.columns().iter().any(|column| column == name) {
        return Err(SQLError::Routine {
            sqlstate: "42601".into(),
            message: format!(
                "{kind} column name \"{name}\" already used in WITH query column list"
            ),
        });
    }
    Ok(())
}
