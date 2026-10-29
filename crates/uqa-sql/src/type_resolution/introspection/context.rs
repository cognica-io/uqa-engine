//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Read-only binding decisions share type inference and resource propagation across scalar owners.

use super::{
    ColumnType, FunctionTypeResolver, LiteralBinding, SQLError, SQLParam, ScalarExpr,
    ScalarTypeSchema,
};
use crate::{
    ast::BinaryOp,
    type_resolution::{
        common::{base_type, unknown_input},
        operators::binary_operator_types_with_control,
        scalar_type_inner_with_control,
    },
};
use uqa_core::memory::{Produced, ProductionControl};

#[derive(Clone, Copy)]
pub(super) struct BinderContext<'a> {
    pub(super) schema: &'a dyn ScalarTypeSchema,
    pub(super) params: &'a [SQLParam],
    pub(super) resolver: Option<&'a dyn FunctionTypeResolver>,
    pub(super) control: ProductionControl<'a>,
    pub(super) literal_binding: LiteralBinding,
}

impl BinderContext<'_> {
    pub(super) fn semantic<T>(&self, result: Result<T, SQLError>) -> Result<Option<T>, SQLError> {
        match result {
            Ok(value) => Ok(Some(value)),
            Err(error)
                if self.control.budget().is_some()
                    && matches!(error.sqlstate(), Some("53200" | "57014")) =>
            {
                Err(error)
            }
            Err(_) => Ok(None),
        }
    }

    pub(super) fn infer(
        &self,
        expression: &ScalarExpr,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        scalar_type_inner_with_control(
            expression,
            self.schema,
            self.params,
            self.resolver,
            &self.control,
        )
    }

    pub(super) fn common_type<'a>(
        &self,
        expressions: impl IntoIterator<Item = &'a ScalarExpr>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let mut common = None;
        let mut saw_expression = false;
        let mut saw_unknown = false;
        for expression in expressions {
            saw_expression = true;
            // `select_common_type` passes over `unknown` literals, which the selected type then reads.
            if crate::type_resolution::is_unknown_literal(expression) {
                saw_unknown = true;
                continue;
            }
            let Some(ty) = self.semantic(self.common_context(expression))? else {
                return Ok(None);
            };
            let Some(merged) = self.semantic(crate::type_resolution::common::merge_value_types(
                common,
                ty,
                &self.control,
            ))?
            else {
                return Ok(None);
            };
            common = merged;
        }
        if saw_unknown {
            if let Some(selected) = common.as_ref() {
                common = Some(base_type(selected).clone_with_control(&self.control)?);
            }
        }
        if saw_expression && common.is_none() {
            common = Some(ColumnType::Text.clone_with_control(&self.control)?);
        }
        Ok(common)
    }

    pub(super) fn common_context(
        &self,
        expression: &ScalarExpr,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        crate::type_resolution::common::common_context_expression_type_with_control(
            expression,
            self.schema,
            self.params,
            self.resolver,
            &self.control,
        )
    }

    pub(super) fn cast_requires_source(&self, target: &str) -> Result<bool, SQLError> {
        let Some(target) = self
            .semantic(super::super::inference::resolved_type_name(
                target,
                self.resolver,
                &self.control,
            ))?
            .flatten()
        else {
            return Ok(false);
        };
        let mut target = &*target;
        while let ColumnType::Array(element) | ColumnType::Domain { base: element, .. } = target {
            self.control.check()?;
            target = element;
        }
        Ok(target.is_character_string()
            || matches!(
                target,
                ColumnType::Bytea
                    | ColumnType::Oid
                    | ColumnType::Xid
                    | ColumnType::Int2Vector
                    | ColumnType::OidVector
                    | ColumnType::InternalChar
            ))
    }

    pub(super) fn declared_source(
        &self,
        target: &str,
        source: Produced<ColumnType>,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        if self.is_character_target(target)? {
            return match base_type(&source) {
                source @ (ColumnType::Int2Vector
                | ColumnType::OidVector
                | ColumnType::Real
                | ColumnType::InternalChar) => Ok(Some(source.clone_with_control(&self.control)?)),
                _ => Ok(None),
            };
        }
        Ok(Some(source))
    }

    pub(super) fn is_character_target(&self, target: &str) -> Result<bool, SQLError> {
        Ok(self
            .semantic(ColumnType::from_sql_name_with_control(
                target,
                &self.control,
            ))?
            .is_some_and(|ty| ty.is_character_string()))
    }

    pub(super) fn binary_operand_types(
        &self,
        op: BinaryOp,
        left: &ScalarExpr,
        right: &ScalarExpr,
    ) -> Result<Option<Produced<[ColumnType; 3]>>, SQLError> {
        let left_type = self.common_context(left)?;
        let right_type = self.common_context(right)?;
        self.operator_types(op, left, left_type, right, right_type)
    }

    pub(super) fn operator_types(
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
        binary_operator_types_with_control(
            op,
            left_type.as_deref(),
            right_type.as_deref(),
            &self.control,
        )
        .map(Some)
    }
}
