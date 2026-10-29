//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Result declarations keep independently admitted fields until return-shape analysis publishes one immutable layout.

use super::{
    AnalyzedResult, ColumnType, CreateFunction, FunctionReturns, RoutineTypeCatalog, SQLError,
    SQLFunctionResultColumn, SQLFunctionResultKind, SQLFunctionResultLayout,
};
use crate::type_resolution::{
    assignment_type_compatible_with_control, canonical_routine_type_name_with_control,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

struct LayoutFields {
    kind: SQLFunctionResultKind,
    declared_type: Produced<ColumnType>,
    source_record: Option<Produced<Vec<Option<ColumnType>>>>,
    columns: Option<Produced<Vec<SQLFunctionResultColumn>>>,
}

impl LayoutFields {
    fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<SQLFunctionResultLayout>, SQLError> {
        let (declared_type, mut memory) = self.declared_type.into_parts();
        let source_record = self.source_record.map(|source| {
            let (source, retained) = source.into_parts();
            memory = control.combine(memory.take(), retained);
            source
        });
        let columns = self.columns.map(|columns| {
            let (columns, retained) = columns.into_parts();
            memory = control.combine(memory.take(), retained);
            columns
        });
        Ok(control.finish(
            SQLFunctionResultLayout {
                kind: self.kind,
                declared_type,
                source_record,
                columns,
            },
            memory,
        )?)
    }

    fn mismatch(
        &self,
        types: &dyn RoutineTypeCatalog,
        detail: String,
        control: &ProductionControl<'_>,
    ) -> Result<SQLError, SQLError> {
        let name = types.format_type_with_control(&self.declared_type, control)?;
        Ok(SQLError::Diagnostic {
            sqlstate: "42P13".into(),
            message: format!(
                "return type mismatch in function declared to return {}",
                name.as_str()
            ),
            detail: Some(detail),
            hint: None,
        })
    }
}

/// Produce the selected declaration before analyzing or evaluating its body.
pub fn declared_sql_function_result_with_control(
    types: &dyn RoutineTypeCatalog,
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLFunctionResultLayout>, SQLError> {
    declared_fields(types, definition, control)?.finish(control)
}

fn declared_fields(
    types: &dyn RoutineTypeCatalog,
    definition: &CreateFunction,
    control: &ProductionControl<'_>,
) -> Result<LayoutFields, SQLError> {
    control.check()?;
    let mut outputs = definition.output_params();
    if definition.is_procedure || outputs.clone().nth(1).is_some() {
        let mut columns = ProductionVec::new(*control);
        columns.reserve(outputs.clone().count())?;
        for parameter in outputs {
            columns.push_produced(column(
                &parameter.name,
                types
                    .resolve_catalog_column_type_name_with_control(&parameter.type_name, control)?,
                control,
            )?)?;
        }
        return Ok(LayoutFields {
            kind: if columns.is_empty() {
                SQLFunctionResultKind::Void
            } else {
                SQLFunctionResultKind::Tuple
            },
            declared_type: control.finish(ColumnType::Record, control.empty_reservation())?,
            source_record: None,
            columns: Some(columns.finish()?),
        });
    }
    let name = match (outputs.next(), &definition.returns) {
        (Some(parameter), _) => parameter.type_name.as_str(),
        (None, FunctionReturns::Scalar { type_name } | FunctionReturns::SetOf { type_name }) => {
            type_name
        }
        (None, FunctionReturns::None | FunctionReturns::Table) => "void",
    };
    let declared_type = match canonical_routine_type_name_with_control(name, control)?.as_str() {
        "void" => control.finish(ColumnType::Void, control.empty_reservation())?,
        "record" => control.finish(ColumnType::Record, control.empty_reservation())?,
        _ => types.resolve_catalog_column_type_name_with_control(name, control)?,
    };
    let (kind, columns) = match &*declared_type {
        ColumnType::Void => (SQLFunctionResultKind::Void, None),
        ColumnType::Record => (SQLFunctionResultKind::Tuple, None),
        ColumnType::Composite(reference) => {
            let descriptor = crate::expr::composites::descriptor_with_control(
                types.composite_types(),
                reference.oid,
                control,
            )?;
            let mut columns = ProductionVec::new(*control);
            columns.reserve(descriptor.attributes.len())?;
            for attribute in &descriptor.attributes {
                columns.push_produced(column(
                    &attribute.name,
                    attribute.ty.clone_with_control(control)?,
                    control,
                )?)?;
            }
            (SQLFunctionResultKind::Tuple, Some(columns.finish()?))
        }
        _ => (SQLFunctionResultKind::Value, None),
    };
    Ok(LayoutFields {
        kind,
        declared_type,
        source_record: None,
        columns,
    })
}

fn column(
    name: &str,
    ty: Produced<ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLFunctionResultColumn>, SQLError> {
    let name = control.copy_text(name)?;
    let (name, names) = name.into_parts();
    let (ty, types) = ty.into_parts();
    Ok(control.finish(
        SQLFunctionResultColumn { name, ty },
        control.combine(names, types),
    )?)
}

/// Analyze the last statement using the same assignment and tuple-shape decisions as ordinary result validation.
pub fn sql_function_result_layout_with_control(
    types: &dyn RoutineTypeCatalog,
    definition: &CreateFunction,
    last: Option<&AnalyzedResult>,
    control: &ProductionControl<'_>,
) -> Result<Produced<SQLFunctionResultLayout>, SQLError> {
    let mut layout = declared_fields(types, definition, control)?;
    if layout.kind == SQLFunctionResultKind::Void {
        return layout.finish(control);
    }
    let Some(columns) = last.and_then(AnalyzedResult::column_types) else {
        return Err(layout.mismatch(
            types,
            "Function's final statement must be SELECT or INSERT/UPDATE/DELETE/MERGE RETURNING."
                .into(),
            control,
        )?);
    };
    if let [actual] = columns {
        layout.source_record = last
            .and_then(|result| result.record_fields(0))
            .map(|fields| super::retention::copy_types(fields, control))
            .transpose()?;
        // Ordinary result validation resolves the named composite even when a source descriptor exists; preserve that catalog lookup and its diagnostics.
        let composite =
            sql_function_composite_columns_with_control(types, actual.as_ref(), control)?;
        if layout.source_record.is_none() {
            layout.source_record = composite;
        }
    }
    if layout.kind == SQLFunctionResultKind::Value {
        let [actual] = columns else {
            return Err(layout.mismatch(
                types,
                "Final statement must return exactly one column.".into(),
                control,
            )?);
        };
        if let Some(actual) = actual {
            if !assignment_type_compatible_with_control(actual, &layout.declared_type, control)? {
                let actual = types.format_type_with_control(actual, control)?;
                return Err(layout.mismatch(
                    types,
                    format!("Actual return type is {}.", actual.as_str()),
                    control,
                )?);
            }
        }
        return layout.finish(control);
    }
    if !definition.is_procedure {
        if let [Some(actual)] = columns {
            if assignment_type_compatible_with_control(actual, &layout.declared_type, control)? {
                layout.kind = SQLFunctionResultKind::Value;
                if matches!(actual, ColumnType::Record)
                    && matches!(*layout.declared_type, ColumnType::Composite(_))
                {
                    if let Some(source) = &layout.source_record {
                        check_composite_assignment(types, source, &layout, control)?;
                    }
                }
                return layout.finish(control);
            }
        }
    }
    if let Some(expected) = &layout.columns {
        for (position, actual) in columns.iter().enumerate() {
            control.check()?;
            let Some(expected) = expected.get(position) else {
                return Err(layout.mismatch(
                    types,
                    "Final statement returns too many columns.".into(),
                    control,
                )?);
            };
            if let Some(actual) = actual {
                if !assignment_type_compatible_with_control(actual, &expected.ty, control)? {
                    let actual = types.format_type_with_control(actual, control)?;
                    let expected = types.format_type_with_control(&expected.ty, control)?;
                    return Err(layout.mismatch(
                        types,
                        format!(
                            "Final statement returns {} instead of {} at column {}.",
                            actual.as_str(),
                            expected.as_str(),
                            position + 1
                        ),
                        control,
                    )?);
                }
            }
        }
        if columns.len() < expected.len() {
            return Err(layout.mismatch(
                types,
                "Final statement returns too few columns.".into(),
                control,
            )?);
        }
    }
    layout.finish(control)
}

fn check_composite_assignment(
    types: &dyn RoutineTypeCatalog,
    source: &[Option<ColumnType>],
    layout: &LayoutFields,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let target = layout
        .columns
        .as_ref()
        .map_or(&[][..], |columns| columns.as_slice());
    let mut detail = match source.len().cmp(&target.len()) {
        std::cmp::Ordering::Less => Some("Input has too few columns.".into()),
        std::cmp::Ordering::Greater => Some("Input has too many columns.".into()),
        std::cmp::Ordering::Equal => None,
    };
    if source.len() == target.len() {
        for (index, (source, target)) in source.iter().zip(target).enumerate() {
            control.check()?;
            if let Some(source) = source {
                if !assignment_type_compatible_with_control(source, &target.ty, control)? {
                    let source = types.format_type_with_control(source, control)?;
                    let target = types.format_type_with_control(&target.ty, control)?;
                    detail = Some(format!(
                        "Cannot cast type {} to {} in column {}.",
                        source.as_str(),
                        target.as_str(),
                        index + 1
                    ));
                    break;
                }
            }
        }
    }
    if let Some(detail) = detail {
        let name = types.format_type_with_control(&layout.declared_type, control)?;
        return Err(SQLError::Diagnostic {
            sqlstate: "42846".into(),
            message: format!("cannot cast type record to {}", name.as_str()),
            detail: Some(detail),
            hint: None,
        });
    }
    Ok(())
}

/// Copy the selected live composite attributes while retaining their catalog owner.
pub fn sql_function_composite_columns_with_control(
    types: &dyn RoutineTypeCatalog,
    source: Option<&ColumnType>,
    control: &ProductionControl<'_>,
) -> Result<Option<Produced<Vec<Option<ColumnType>>>>, SQLError> {
    control.check()?;
    match source {
        Some(ColumnType::Composite(reference)) => {
            let descriptor = crate::expr::composites::descriptor_with_control(
                types.composite_types(),
                reference.oid,
                control,
            )?;
            let mut result = ProductionVec::new(*control);
            result.reserve(descriptor.attributes.len())?;
            for attribute in &descriptor.attributes {
                let (ty, memory) = attribute.ty.clone_with_control(control)?.into_parts();
                result.push_produced(control.finish(Some(ty), memory)?)?;
            }
            Ok(Some(result.finish()?))
        }
        Some(ColumnType::Domain { base, .. }) => {
            sql_function_composite_columns_with_control(types, Some(base), control)
        }
        _ => Ok(None),
    }
}
