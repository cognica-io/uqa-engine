//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared field results reuse the original argument analysis and the selected attribute metadata.

use super::function_calls::ObservedFunctionArguments;
use super::{ColumnType, ExpressionType, Preparation, RowSchema, SQLError, ScalarExpr};
use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::catalog::collations::derivation::Derivation;
use crate::schema::{RecordDescriptor, RecordFieldMetadata};
use crate::type_resolution::{base_type, field_selection};
use std::sync::Arc;

pub(super) fn whole_row_descriptor(
    input: &RowSchema,
    qualifier: &str,
    control: &uqa_core::memory::ProductionControl<'_>,
) -> Result<Arc<RecordDescriptor>, SQLError> {
    use uqa_core::memory::ProductionVec;
    control.check()?;
    let mut names = ProductionVec::new(*control);
    let mut types = ProductionVec::new(*control);
    let mut fields = ProductionVec::new(*control);
    input.visit_qualified_star_positions(
        qualifier,
        control,
        |name, _, _, ty: Option<&ColumnType>| {
            names.push_produced(control.copy_text(name)?)?;
            let ty = if let Some(ty) = ty {
                let (ty, memory) = ty.clone_with_control(control)?.into_parts();
                control.finish(Some(ty), memory)?
            } else {
                control.finish(None, control.empty_reservation())?
            };
            types.push_produced(ty)?;
            fields.push_copy(RecordFieldMetadata {
                collation_oid: input.qualified_collation(qualifier, name).unwrap_or(0),
                sql_unknown: false,
            })?;
            Ok(())
        },
    )?;
    RecordDescriptor::from_parts_with_control(
        crate::schema::RecordFields::from_produced(types.finish()?, control)?,
        Some(names.finish()?),
        fields.finish()?,
        control,
    )
}

impl Preparation<'_> {
    pub(super) fn declared_field_selection(
        &self,
        binding: Option<&FunctionBinding>,
        args: &[ScalarExpr],
        arguments: &ObservedFunctionArguments,
        input: &RowSchema,
    ) -> Result<Option<ExpressionType>, SQLError> {
        let Some(binding) =
            binding.filter(|binding| binding.dispatch == Some(FunctionDispatch::FieldSelect))
        else {
            return Ok(None);
        };
        let [base, ScalarExpr::Literal(uqa_core::Value::Str(name))] = args else {
            return Err(SQLError::Internal(
                "field selection takes an expression and a field name".into(),
            ));
        };
        let mut sql_unknown = false;
        let control = self.scope.control();
        let copy_type =
            |ty: Option<&ColumnType>| -> Result<Option<super::ownership::AnalysisType>, SQLError> {
                ty.map(|ty| {
                    super::ownership::AnalysisType::from_produced(
                        ty.clone_with_control(&control)?,
                        &control,
                    )
                })
                .transpose()
            };
        let (ty, collation) =
            if let Some(qualifier) = field_selection::whole_row_qualifier(base, input) {
                if !input.has_qualified_column(qualifier, name) {
                    return Err(field_selection::missing_relation_column(qualifier, name));
                }
                (
                    copy_type(input.qualified_type(qualifier, name))?,
                    input.qualified_collation(qualifier, name).unwrap_or(0),
                )
            } else if let Some(field) = binding.composite_field() {
                (
                    copy_type(Some(&field.result_type))?,
                    field.collation_oid.unwrap_or_else(|| {
                        u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                            &field.result_type,
                        ))
                        .expect("field collation identity")
                    }),
                )
            } else {
                let Some(value) = arguments.values.first() else {
                    return Err(SQLError::Internal(
                        "field selection has no analyzed source".into(),
                    ));
                };
                if let Some(fields) = &value.row_constructor {
                    let position = field_selection::anonymous_field_position(name, fields.len())
                        .ok_or_else(|| field_selection::undefined_record_field(name))?;
                    let field = &fields[position];
                    sql_unknown = field.is_sql_unknown();
                    (field.ty.clone(), field.initial_collation)
                } else if let Some(descriptor) = &value.record_descriptor {
                    let position = descriptor
                        .position(name)
                        .ok_or_else(|| field_selection::undefined_record_field(name))?;
                    sql_unknown = descriptor.fields[position].sql_unknown;
                    (
                        copy_type(descriptor.types[position].as_ref())?,
                        descriptor.fields[position].collation_oid,
                    )
                } else if matches!(value.ty.as_deref().map(base_type), Some(ColumnType::Record))
                    && value.record_variable
                {
                    return Err(field_selection::unregistered_record());
                } else if !matches!(
                    value.ty.as_deref().map(base_type),
                    Some(ColumnType::Composite(_) | ColumnType::Record)
                ) {
                    return Ok(None);
                } else if let Some((ty, collation)) =
                    field_selection::value_field_metadata_with_control(
                        value.ty.as_deref(),
                        name,
                        Some(self.routines),
                        &control,
                    )?
                {
                    (
                        Some(super::ownership::AnalysisType::from_produced(ty, &control)?),
                        collation,
                    )
                } else {
                    return Ok(None);
                }
            };
        let mut result = ExpressionType::independent_owned(ty, &arguments.values);
        result.derivation = result
            .derivation
            .error_only()
            .combine(Derivation::implicit(i64::from(collation)));
        result.initial_collation = collation;
        if sql_unknown {
            result.mark_unknown_field();
        }
        Ok(Some(result))
    }
}
