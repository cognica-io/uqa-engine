//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array inputs are analyzed before their bounds, in written lower/upper order.

use super::ownership::{AnalysisType, AnalysisVec};
use super::{ColumnType, ExpressionType, Preparation, QueryPlan, RowSchema, SQLError, ScalarExpr};
use crate::ast::ArrayAccess;

impl Preparation<'_> {
    pub(super) fn array_access(
        &mut self,
        access: ArrayAccess,
        args: &[ScalarExpr],
        input: &RowSchema,
        subqueries: &[QueryPlan],
    ) -> Result<ExpressionType, SQLError> {
        let owner = self.scope.control().capture();
        let control = owner.control();
        let array = self.expression(&args[0], input, subqueries)?;
        let result = if array.is_deferred() {
            None
        } else {
            let source = array.ty.as_deref();
            let base = source.map(crate::type_resolution::base_type);
            if !matches!(
                base,
                Some(ColumnType::Array(_) | ColumnType::Int2Vector | ColumnType::OidVector)
            ) {
                let name = base.map_or_else(
                    || control.copy_text("unknown").map_err(Into::into),
                    |ty| self.routines.format_type_name_with_control(ty, &control),
                )?;
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!(
                        "cannot subscript type {} because it does not support subscripting",
                        name.as_str()
                    ),
                });
            }
            if access.slice_bounds().is_some() {
                match base {
                    Some(ColumnType::Int2Vector) => {
                        Some(AnalysisType::Inline(ColumnType::SmallInteger).array(&control)?)
                    }
                    Some(ColumnType::OidVector) => {
                        Some(AnalysisType::Inline(ColumnType::Oid).array(&control)?)
                    }
                    _ => base
                        .map(|ty| {
                            AnalysisType::from_produced(ty.clone_with_control(&control)?, &control)
                        })
                        .transpose()?,
                }
            } else {
                base.and_then(crate::type_resolution::array_element_type)
                    .map(|ty| {
                        AnalysisType::from_produced(ty.clone_with_control(&control)?, &control)
                    })
                    .transpose()?
            }
        };
        let mut values = AnalysisVec::new(&control);
        values.push(array, &control)?;
        for (index, argument) in args.iter().enumerate().skip(1) {
            if access.is_omitted(index) {
                continue;
            }
            let mut value = self.expression(argument, input, subqueries)?;
            let needs_cast = value
                .ty
                .as_deref()
                .is_some_and(|ty| *ty != ColumnType::Integer)
                || matches!(argument, ScalarExpr::Literal(uqa_core::Value::Null));
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Integer)?;
            if value
                .ty
                .as_deref()
                .map(|ty| {
                    crate::assignment_type_compatible_with_control(
                        ty,
                        &ColumnType::Integer,
                        &control,
                    )
                })
                .transpose()?
                .is_some_and(|valid| !valid)
            {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "array subscript must have type integer".into(),
                });
            }
            if needs_cast {
                self.parameters
                    .retain_input_coercion(argument, AnalysisType::Inline(ColumnType::Integer))?;
            }
            values.push(value, &control)?;
        }
        access.validate_dimensions()?;
        Ok(ExpressionType::combined_owned(result, &values))
    }
}
