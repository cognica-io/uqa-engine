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

#[derive(Default)]
pub(super) struct CastInputs {
    pub(super) source: Option<Produced<ColumnType>>,
    pub(super) function: Option<Produced<crate::ast::FunctionBinding>>,
}

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

    /// `PostgreSQL` elides polymorphic coercions that leave their input's type and datum intact.
    pub(super) fn cast_preserves_operand(
        &self,
        name: &str,
        operand: &ScalarExpr,
    ) -> Result<bool, SQLError> {
        use crate::type_resolution::pseudo_cast::{self, Coercion};
        let local = name
            .strip_prefix("pg_catalog.")
            .or_else(|| name.strip_prefix("\"pg_catalog\"."))
            .unwrap_or(name)
            .trim_matches('"');
        if !(local.starts_with("any") || local == "unknown") {
            return Ok(false);
        }
        let Some(target) = crate::type_resolution::inference::resolved_type_name(
            name,
            self.resolver,
            &self.control,
        )?
        else {
            return Ok(false);
        };
        if !matches!(*target, ColumnType::Pseudo(_) | ColumnType::AnyArray) {
            return Ok(false);
        }
        let source = crate::type_resolution::common::common_context_expression_type_with_control(
            operand,
            self.schema,
            self.params,
            self.resolver,
            &self.control,
        )?;
        let coercion = pseudo_cast::select(
            &target,
            source.as_deref(),
            matches!(operand, ScalarExpr::Literal(uqa_core::Value::Str(_))),
            &self.control,
        )?;
        Ok(match coercion {
            Some(Coercion::Preserve) => true,
            Some(Coercion::Relabel(ty)) => source.as_deref() == Some(ty),
            _ => false,
        })
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

    pub(super) fn cast_inputs(
        &self,
        name: &str,
        implicit: bool,
        operand: &ScalarExpr,
    ) -> Result<CastInputs, SQLError> {
        if implicit {
            return Ok(CastInputs::default());
        }
        let Some(target) = self.source_sensitive_cast_target(name)? else {
            return Ok(CastInputs::default());
        };
        let source = self
            .semantic(self.infer(operand))?
            .flatten()
            .map(|source| self.declared_source(name, source))
            .transpose()?
            .flatten();
        if let Some(resolver) = self.resolver {
            if let Some(binding) = crate::type_resolution::declared_cast_function(
                source.as_deref(),
                &target,
                resolver,
                &self.control,
            )? {
                return Ok(CastInputs {
                    function: Some(binding),
                    source: None,
                });
            }
        }
        Ok(CastInputs {
            source,
            function: None,
        })
    }

    fn source_sensitive_cast_target(
        &self,
        name: &str,
    ) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let Some(target) = self
            .semantic(super::super::inference::resolved_type_name(
                name,
                self.resolver,
                &self.control,
            ))?
            .flatten()
        else {
            return Ok(None);
        };
        let mut base = &*target;
        while let ColumnType::Array(element) | ColumnType::Domain { base: element, .. } = base {
            self.control.check()?;
            base = element;
        }
        let selected = base.is_character_string()
            || matches!(
                base,
                ColumnType::Bytea
                    | ColumnType::Oid
                    | ColumnType::Xid
                    | ColumnType::Int2Vector
                    | ColumnType::OidVector
                    | ColumnType::InternalChar
                    | ColumnType::DeclaredMultirange(_)
            );
        Ok(selected.then_some(target))
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
