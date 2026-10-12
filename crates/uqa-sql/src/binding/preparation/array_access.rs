//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Array inputs are analyzed before their bounds, in written lower/upper order.

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
        let array = self.expression(&args[0], input, subqueries)?;
        let result = if array.is_deferred() {
            None
        } else {
            let source = array.ty.as_ref();
            let base = source.map(crate::type_resolution::base_type);
            if !matches!(
                base,
                Some(ColumnType::Array(_) | ColumnType::Int2Vector | ColumnType::OidVector)
            ) {
                let name = base.map_or_else(
                    || Ok("unknown".into()),
                    |ty| self.routines.format_type_name(ty),
                )?;
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!(
                        "cannot subscript type {name} because it does not support subscripting"
                    ),
                });
            }
            if access.slice_bounds().is_some() {
                match base {
                    Some(ColumnType::Int2Vector) => {
                        Some(ColumnType::Array(Box::new(ColumnType::SmallInteger)))
                    }
                    Some(ColumnType::OidVector) => {
                        Some(ColumnType::Array(Box::new(ColumnType::Oid)))
                    }
                    _ => base.cloned(),
                }
            } else {
                base.and_then(crate::type_resolution::array_element_type)
                    .cloned()
            }
        };
        let mut values = vec![array];
        for (index, argument) in args.iter().enumerate().skip(1) {
            if access.is_omitted(index) {
                continue;
            }
            let mut value = self.expression(argument, input, subqueries)?;
            let needs_cast = value
                .ty
                .as_ref()
                .is_some_and(|ty| *ty != ColumnType::Integer)
                || matches!(argument, ScalarExpr::Literal(uqa_core::Value::Null));
            self.parameters
                .coerce_unknown(&mut value, &ColumnType::Integer)?;
            if value
                .ty
                .as_ref()
                .is_some_and(|ty| !crate::assignment_type_compatible(ty, &ColumnType::Integer))
            {
                return Err(SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: "array subscript must have type integer".into(),
                });
            }
            if needs_cast {
                self.parameters
                    .retain_input_coercion(argument, ColumnType::Integer);
            }
            values.push(value);
        }
        access.validate_dimensions()?;
        Ok(ExpressionType::combined(result, &values))
    }
}
