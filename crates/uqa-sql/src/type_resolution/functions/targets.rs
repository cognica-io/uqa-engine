//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Argument target vectors borrow source declarations and admit each copied payload.

use super::{compatible_array_argument, compatible_array_result_type, fixed_builtin};
use crate::{ColumnType, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(crate) fn builtin_function_argument_targets_with_control(
    name: &str,
    types: &[Option<ColumnType>],
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<Option<ColumnType>>>, SQLError> {
    let lower = crate::semantics::lowercase_function_name_with_control(name, control)?;
    let name = lower.strip_prefix("pg_catalog.").unwrap_or(&lower);
    if let Some(targets) =
        fixed_builtin::preparation::selected_argument_targets_with_control(name, types, control)?
    {
        return Ok(targets);
    }
    let array = if matches!(
        name,
        "array_cat" | "array_append" | "array_prepend" | "array_remove" | "array_replace"
    ) {
        semantic_optional(compatible_array_result_type(name, types, control))?
    } else {
        None
    };
    let concat = if name == "concat_op" && types.len() == 2 {
        semantic_optional(
            super::super::concatenation::select(types[0].as_ref(), types[1].as_ref(), control)
                .map(Some),
        )?
    } else {
        None
    };
    let mut output = ProductionVec::new(*control);
    output.reserve(types.len())?;
    for (position, ty) in types.iter().enumerate() {
        let selected = if let Some(array) = &array {
            let ColumnType::Array(element) = &**array else {
                unreachable!("compatible array result");
            };
            Some(if compatible_array_argument(name, position) {
                &**array
            } else {
                &**element
            })
        } else if matches!(
            name,
            "upper"
                | "lower"
                | "initcap"
                | "trim"
                | "btrim"
                | "ltrim"
                | "rtrim"
                | "analyze_text"
                | "create_analyzer"
                | "drop_analyzer"
                | "set_table_analyzer"
                | "fts_index_stats"
                | "text_match"
        ) {
            Some(&ColumnType::Text)
        } else if name == "array_sort" && matches!(types.len(), 2 | 3) && position > 0 {
            Some(&ColumnType::Boolean)
        } else if let Some(concat) = &concat {
            Some(concat.argument(position))
        } else {
            ty.as_ref()
        };
        output.push_produced(copy_optional(selected, control)?)?;
    }
    Ok(output.finish()?)
}

pub(in crate::type_resolution) fn semantic_optional<T>(
    result: Result<Option<T>, SQLError>,
) -> Result<Option<T>, SQLError> {
    match result {
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => Err(error),
        Err(_) => Ok(None),
        value => value,
    }
}

pub(in crate::type_resolution) fn copy_optional(
    ty: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Option<ColumnType>>, SQLError> {
    let (ty, memory) = match ty {
        Some(ty) => {
            let (ty, memory) = ty.clone_with_control(control)?.into_parts();
            (Some(ty), memory)
        }
        None => (None, control.empty_reservation()),
    };
    Ok(control.finish(ty, memory)?)
}
