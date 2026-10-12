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
use crate::type_resolution::{base_type, field_selection};

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
        let (ty, collation) =
            if let Some(qualifier) = field_selection::whole_row_qualifier(base, input) {
                if !input.has_qualified_column(qualifier, name) {
                    return Err(field_selection::missing_relation_column(qualifier, name));
                }
                (
                    input.qualified_type(qualifier, name).cloned(),
                    input.qualified_collation(qualifier, name).unwrap_or(0),
                )
            } else if let Some(field) = &binding.composite_field {
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
                let ty = arguments.values.first().and_then(|value| value.ty.as_ref());
                if !matches!(ty.map(base_type), Some(ColumnType::Composite(_))) {
                    return Ok(None);
                }
                let Some((ty, collation)) =
                    field_selection::value_field_metadata(ty, name, Some(self.routines))?
                else {
                    return Ok(None);
                };
                (Some(ty), collation)
            };
        let mut result = ExpressionType::independent(ty, &arguments.values);
        result.derivation = result
            .derivation
            .error_only()
            .combine(Derivation::implicit(i64::from(collation)));
        Ok(Some(result))
    }
}
