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

pub(super) fn whole_row_descriptor(input: &RowSchema, qualifier: &str) -> Arc<RecordDescriptor> {
    let columns = input.qualified_star_layout(qualifier);
    let fields = columns
        .iter()
        .map(|(name, _, _)| RecordFieldMetadata {
            collation_oid: input.qualified_collation(qualifier, name).unwrap_or(0),
            sql_unknown: false,
        })
        .collect();
    let (names, types): (Vec<_>, Vec<_>) =
        columns.into_iter().map(|(name, _, ty)| (name, ty)).unzip();
    Arc::new(RecordDescriptor {
        types: types.into(),
        names: Some(names.into()),
        fields,
    })
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
        let (ty, collation) = if let Some(qualifier) =
            field_selection::whole_row_qualifier(base, input)
        {
            if !input.has_qualified_column(qualifier, name) {
                return Err(field_selection::missing_relation_column(qualifier, name));
            }
            (
                input.qualified_type(qualifier, name).cloned(),
                input.qualified_collation(qualifier, name).unwrap_or(0),
            )
        } else if let Some(field) = binding.composite_field() {
            (
                Some(field.result_type.clone()),
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
                    descriptor.types[position].clone(),
                    descriptor.fields[position].collation_oid,
                )
            } else if matches!(value.ty.as_ref().map(base_type), Some(ColumnType::Record))
                && value.record_variable
            {
                return Err(field_selection::unregistered_record());
            } else if !matches!(
                value.ty.as_ref().map(base_type),
                Some(ColumnType::Composite(_) | ColumnType::Record)
            ) {
                return Ok(None);
            } else if let Some((ty, collation)) =
                field_selection::value_field_metadata(value.ty.as_ref(), name, Some(self.routines))?
            {
                (Some(ty), collation)
            } else {
                return Ok(None);
            }
        };
        let mut result = ExpressionType::independent(ty, &arguments.values);
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
