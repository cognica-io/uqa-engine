//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Comparison operands take the argument types of their selected SQL operators before planning. An `unknown` literal or parameter is coerced to the type its operator declares, as `PostgreSQL` parse analysis coerces it, and numeric operands keep their selected coercions.

use super::{Binder, BindingCall, ColumnType, Produced, SQLError, ScalarExpr};
use crate::ast::{BinaryOp, FunctionDispatch};
use crate::type_resolution::{common::base_type, operators::binary_operator_types_with_control};
use uqa_core::Value;

impl Binder<'_, '_> {
    /// Selected operand and result types for a comparison whose operands need coercion.
    pub(super) fn comparison_types(
        &self,
        op: BinaryOp,
        left: &ScalarExpr,
        right: &ScalarExpr,
    ) -> Result<Option<Produced<[ColumnType; 3]>>, SQLError> {
        // An arithmetic operator reads an `unknown` operand with the operand type it selects, as `make_op` coerces it; two typed operands keep their own types.
        let reads_unknown_operand =
            is_arithmetic(op) && unknown_input(left) != unknown_input(right);
        if !is_comparison(op) && !reads_unknown_operand {
            return Ok(None);
        }
        let left_type = self.common_context(left)?;
        let right_type = self.common_context(right)?;
        self.operator_types(op, left, left_type, right, right_type)
    }

    fn operator_types(
        &self,
        op: BinaryOp,
        left: &ScalarExpr,
        left_type: Option<Produced<ColumnType>>,
        right: &ScalarExpr,
        right_type: Option<Produced<ColumnType>>,
    ) -> Result<Option<Produced<[ColumnType; 3]>>, SQLError> {
        // A schema-less column is a dynamic carrier, not a PostgreSQL unknown literal. Its runtime value cannot be narrowed using only the other operand.
        if (left_type.is_none() && !unknown_input(left))
            || (right_type.is_none() && !unknown_input(right))
        {
            return Ok(None);
        }
        let coerced = left_type
            .as_deref()
            .into_iter()
            .chain(right_type.as_deref())
            .any(|ty| coerced_operand_type(base_type(ty)));
        if !coerced && left_type.is_some() && right_type.is_some() {
            return Ok(None);
        }
        binary_operator_types_with_control(
            op,
            left_type.as_deref(),
            right_type.as_deref(),
            &self.control,
        )
        .map(Some)
    }

    /// Coerce one operand to the argument type its selected operator declares. An `unknown` literal already carries text, so a text target needs no conversion.
    pub(super) fn operand_cast(
        &mut self,
        expression: &mut ScalarExpr,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        if matches!(expression, ScalarExpr::Literal(Value::Str(_)))
            && matches!(
                base_type(target),
                ColumnType::Text | ColumnType::Varchar(None)
            )
        {
            return Ok(());
        }
        self.common_cast(expression, target)
    }

    /// Bind both operands of a comparison to the selected operator's argument types.
    pub(super) fn coerce_comparison(
        &mut self,
        types: Option<Produced<[ColumnType; 3]>>,
        lhs: &mut ScalarExpr,
        rhs: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        if let Some(types) = types {
            self.operand_cast(lhs, &types[0])?;
            self.operand_cast(rhs, &types[1])?;
        }
        Ok(())
    }

    /// `a BETWEEN b AND c` resolves `a >= b` and `a <= c` independently. The shared operand is cast only when both operators select the same type for it.
    pub(super) fn coerce_between(
        &mut self,
        types: [Option<Produced<[ColumnType; 3]>>; 2],
        value: &mut ScalarExpr,
        low: &mut ScalarExpr,
        high: &mut ScalarExpr,
    ) -> Result<(), SQLError> {
        let [low_types, high_types] = types;
        if let Some([_, low_type, _]) = low_types.as_deref() {
            self.operand_cast(low, low_type)?;
        }
        if let Some([_, high_type, _]) = high_types.as_deref() {
            self.operand_cast(high, high_type)?;
        }
        if let (Some(low_types), Some(high_types)) = (low_types, high_types) {
            if low_types[0] == high_types[0] {
                self.operand_cast(value, &low_types[0])?;
            }
        }
        Ok(())
    }

    /// `a IN (b, c, ...)` compares every item with `a` using the common type of all of them. Without a common type each item keeps the type selected by its own equality operator.
    pub(super) fn coerce_in_list(
        &mut self,
        value: &mut ScalarExpr,
        list: &mut [ScalarExpr],
    ) -> Result<(), SQLError> {
        for expression in std::iter::once(&*value).chain(list.iter()) {
            if !unknown_input(expression)
                && self
                    .semantic(self.common_context(expression))?
                    .flatten()
                    .is_none()
            {
                return Ok(());
            }
        }
        let common = self.common_type(std::iter::once(&*value).chain(list.iter()))?;
        if let Some(common) = common {
            self.operand_cast(value, &common)?;
            for item in list {
                self.operand_cast(item, &common)?;
            }
            return Ok(());
        }
        for item in list {
            if let Some(types) = self
                .semantic(self.comparison_types(BinaryOp::Equal, value, item))?
                .flatten()
            {
                self.operand_cast(item, &types[1])?;
            }
        }
        Ok(())
    }

    /// Comparison syntax represented as a call: `IS DISTINCT FROM`, `BETWEEN SYMMETRIC`, `op ANY/ALL (array)` and `NULLIF`.
    pub(super) fn coerce_comparison_call(
        &mut self,
        call: &mut BindingCall,
    ) -> Result<(), SQLError> {
        let Some(binding) = call.binding.as_ref().filter(|binding| binding.builtin) else {
            return Ok(());
        };
        match binding.dispatch {
            Some(FunctionDispatch::IsDistinct) => self.coerce_argument_pair(&mut call.arguments),
            Some(FunctionDispatch::BetweenSymmetric) => {
                let [value, low, high] = call.arguments.as_mut_slice() else {
                    return Ok(());
                };
                let types = [
                    self.semantic(self.comparison_types(BinaryOp::GreaterEqual, value, low))?
                        .flatten(),
                    self.semantic(self.comparison_types(BinaryOp::LessEqual, value, high))?
                        .flatten(),
                ];
                self.coerce_between(types, value, low, high)
            }
            Some(FunctionDispatch::AnyOperator | FunctionDispatch::AllOperator) => {
                self.coerce_quantified_comparison(&mut call.arguments)
            }
            None if binding.name == "nullif" => self.coerce_argument_pair(&mut call.arguments),
            _ => Ok(()),
        }
    }

    fn coerce_argument_pair(&mut self, arguments: &mut [ScalarExpr]) -> Result<(), SQLError> {
        let [left, right] = arguments else {
            return Ok(());
        };
        let types = self
            .semantic(self.comparison_types(BinaryOp::Equal, left, right))?
            .flatten();
        self.coerce_comparison(types, left, right)
    }

    /// `a op ANY (array)` resolves `op` against the array's element type, or against `unknown` when the array is an untyped literal, then casts the array to an array of the selected right operand type.
    pub(super) fn coerce_quantified_comparison(
        &mut self,
        arguments: &mut [ScalarExpr],
    ) -> Result<(), SQLError> {
        let [value, array, ScalarExpr::Literal(Value::Str(operator))] = arguments else {
            return Ok(());
        };
        let Some(op) = comparison_operator(operator) else {
            return Ok(());
        };
        let value_type = self.semantic(self.common_context(value))?.flatten();
        let array_type = self.semantic(self.common_context(array))?.flatten();
        let element_type = match array_type.as_deref().map(base_type) {
            None => None,
            Some(ColumnType::Array(element)) => Some(element.clone_with_control(&self.control)?),
            Some(_) => return Ok(()),
        };
        let element = element_type
            .as_deref()
            .map(|element| base_type(element).clone_with_control(&self.control))
            .transpose()?;
        let Some(types) = self
            .semantic(self.operator_types(op, value, value_type, array, element_type))?
            .flatten()
        else {
            return Ok(());
        };
        self.operand_cast(value, &types[0])?;
        // An untyped array literal takes the array type of the selected operand; a typed array whose elements the operator does not declare, such as `regclass[]` against the `oid` operators, is relabeled to it. An `ARRAY[...]` constructor keeps its element type: a written cast of a constructor is pushed into its elements when it is analyzed, so a cast around the constructor would print as the written form.
        let relabel = !matches!(array, ScalarExpr::Array(_))
            && element
                .as_deref()
                .is_some_and(|element| *element != types[1]);
        if array_type.is_none() || relabel {
            let array_target = ColumnType::array_with_control(
                types[1].clone_with_control(&self.control)?,
                &self.control,
            )?;
            self.common_cast(array, &array_target)?;
        }
        Ok(())
    }
}

fn is_comparison(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Equal
            | BinaryOp::NotEqual
            | BinaryOp::Less
            | BinaryOp::LessEqual
            | BinaryOp::Greater
            | BinaryOp::GreaterEqual
    )
}

fn is_arithmetic(op: BinaryOp) -> bool {
    matches!(
        op,
        BinaryOp::Add | BinaryOp::Subtract | BinaryOp::Multiply | BinaryOp::Divide
    )
}

fn comparison_operator(operator: &str) -> Option<BinaryOp> {
    Some(match operator {
        "=" => BinaryOp::Equal,
        "<>" | "!=" => BinaryOp::NotEqual,
        "<" => BinaryOp::Less,
        "<=" => BinaryOp::LessEqual,
        ">" => BinaryOp::Greater,
        ">=" => BinaryOp::GreaterEqual,
        _ => return None,
    })
}

fn unknown_input(expression: &ScalarExpr) -> bool {
    matches!(
        expression,
        ScalarExpr::Literal(Value::Null | Value::Str(_)) | ScalarExpr::Param(_)
    )
}

/// Operand types whose comparisons record their coercions: the numeric types, which select among several operator signatures, and `oid` with its alias types, whose comparisons are the `oid` operators that an alias operand is relabeled to, as `PostgreSQL` relabels it.
fn coerced_operand_type(ty: &ColumnType) -> bool {
    matches!(
        ty,
        ColumnType::SmallInteger
            | ColumnType::Integer
            | ColumnType::BigInteger
            | ColumnType::Real
            | ColumnType::DoublePrecision
            | ColumnType::Numeric { .. }
            | ColumnType::Oid
            | ColumnType::Regclass
            | ColumnType::Regtype
            | ColumnType::Regproc
            | ColumnType::Regprocedure
            | ColumnType::Regnamespace
            | ColumnType::Regrole
    )
}

#[cfg(test)]
mod tests;
