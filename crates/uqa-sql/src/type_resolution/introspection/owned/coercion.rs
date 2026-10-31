//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Selected coercions wrap or replace complete scalar owners.

use super::{ColumnType, OwnedBinder, OwnedScalarExpr, SQLError, ScalarExpr};
use crate::{expr::enums::is_enum_bearing, type_resolution::common::base_type};
use uqa_core::{memory::Produced, Value};

impl OwnedBinder<'_> {
    pub(super) fn common_cast(
        &self,
        mut expression: OwnedScalarExpr,
        target: &ColumnType,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let target = target.without_type_modifiers_with_control(control)?;
        let source = self
            .context
            .semantic(self.context.common_context(&expression))?
            .flatten();
        if let Some(source) = source {
            let source = source.without_type_modifiers_with_control(control)?;
            if *source == *target
                || crate::type_resolution::cast_compatibility::preserves_record_identity(
                    &source, &target,
                )
            {
                return Ok(expression);
            }
        }
        if crate::catalog::collations::expression::scalar_clause(&expression)?.is_some() {
            if crate::schema::composites::type_is_collatable(&target) {
                return expression.map_child(0, control, |mut child| {
                    while crate::catalog::collations::expression::scalar_clause(&child)?.is_some() {
                        child = child.select_child(0);
                    }
                    self.common_cast(child, &target)
                });
            }
            while crate::catalog::collations::expression::scalar_clause(&expression)?.is_some() {
                expression = expression.select_child(0);
            }
            return self.common_cast(expression, &target);
        }
        if let Some(folded) = self.fold_enum_literal(&expression, &target)? {
            drop(expression);
            return Ok(folded);
        }
        if let Some(folded) = self.read_unknown_literal(&expression, &target)? {
            drop(expression);
            return Ok(folded);
        }
        Ok(OwnedScalarExpr::cast(
            expression,
            target.catalog_name_with_control(control)?,
            true,
            control,
        )?)
    }

    pub(super) fn operand_cast(
        &self,
        expression: OwnedScalarExpr,
        target: &ColumnType,
    ) -> Result<OwnedScalarExpr, SQLError> {
        if matches!(&*expression, ScalarExpr::Literal(Value::Str(_)))
            && matches!(
                base_type(target),
                ColumnType::Text | ColumnType::Varchar(None)
            )
        {
            return Ok(expression);
        }
        self.common_cast(expression, target)
    }

    pub(super) fn wrap_declared(
        &self,
        expression: OwnedScalarExpr,
        ty: &ColumnType,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let name = ty.catalog_name_with_control(control)?;
        if matches!(&*expression, ScalarExpr::Cast {ty, ..} if ty.eq_ignore_ascii_case(&name)) {
            return Ok(expression);
        }
        Ok(OwnedScalarExpr::cast(expression, name, true, control)?)
    }

    fn read_unknown_literal(
        &self,
        expression: &ScalarExpr,
        target: &ColumnType,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        let ScalarExpr::Literal(value @ Value::Str(text)) = expression else {
            return Ok(None);
        };
        let stored = self.context.literal_binding == super::super::LiteralBinding::Stored;
        if crate::type_resolution::catalog_input_type(target)
            && !(stored && crate::expr::requires_catalog_constant_input(target))
        {
            return Ok(None);
        }
        let control = &self.context.control;
        let value = if stored && crate::expr::requires_catalog_constant_input(target) {
            let Some(catalog) = self
                .context
                .resolver
                .and_then(crate::FunctionTypeResolver::catalog_input_functions)
            else {
                return Ok(None);
            };
            catalog.read_unknown_input_with_control(text, target, control)
        } else {
            crate::assignment::conversion::convert_value_to_column_type_with_control(
                control.copy_value(value)?,
                target,
                control,
            )
        };
        let value = match value {
            Ok(value) => value,
            Err(error)
                if self.context.literal_binding != super::super::LiteralBinding::Ordinary
                    || matches!(error.sqlstate(), Some("53200" | "57014")) =>
            {
                return Err(error)
            }
            Err(_) => return Ok(None),
        };
        Ok(Some(OwnedScalarExpr::typed_literal(
            value,
            target.catalog_name_with_control(control)?,
            Some(target.clone_with_control(control)?),
            None,
            None,
            control,
        )?))
    }

    fn fold_enum_literal(
        &self,
        expression: &ScalarExpr,
        target: &ColumnType,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        if !is_enum_bearing(target) {
            return Ok(None);
        }
        let ScalarExpr::Literal(value @ (Value::Str(_) | Value::Null)) = expression else {
            return Ok(None);
        };
        let Some(catalog) = self
            .context
            .resolver
            .and_then(|resolver| resolver.enum_labels())
        else {
            return Ok(None);
        };
        let control = &self.context.control;
        let value = match crate::expr::enums::fold_unknown_literal_with_control(
            Some(catalog),
            value,
            target,
            control,
        ) {
            Ok(Some(value)) => value,
            Ok(None) => return Ok(None),
            Err(error)
                if self.context.literal_binding != super::super::LiteralBinding::Ordinary
                    || matches!(error.sqlstate(), Some("53200" | "57014")) =>
            {
                return Err(error)
            }
            Err(_) => return Ok(None),
        };
        Ok(Some(OwnedScalarExpr::typed_literal(
            value,
            target.catalog_name_with_control(control)?,
            Some(target.clone_with_control(control)?),
            None,
            None,
            control,
        )?))
    }

    pub(super) fn coerce_between(
        &self,
        mut expression: OwnedScalarExpr,
        low: Option<Produced<[ColumnType; 3]>>,
        high: Option<Produced<[ColumnType; 3]>>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        if let Some(types) = &low {
            expression =
                expression.map_child(1, control, |child| self.operand_cast(child, &types[1]))?;
        }
        if let Some(types) = &high {
            expression =
                expression.map_child(2, control, |child| self.operand_cast(child, &types[1]))?;
        }
        if let (Some(low), Some(high)) = (low, high) {
            if low[0] == high[0] {
                expression =
                    expression.map_child(0, control, |child| self.operand_cast(child, &low[0]))?;
            }
        }
        Ok(expression)
    }

    pub(super) fn bind_case(
        &self,
        expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let mut expression = expression.map_children(control, |_, child| self.bind(child))?;
        let ScalarExpr::Case {
            base,
            when,
            else_branch,
        } = &*expression
        else {
            unreachable!()
        };
        let base_count = usize::from(base.is_some());
        let arm_count = when.len();
        let has_else = else_branch.is_some();
        let target = if base_count == 0 {
            None
        } else {
            self.context.common_type(
                base.iter()
                    .map(Box::as_ref)
                    .chain(when.iter().map(|(condition, _)| condition)),
            )?
        };
        if let Some(target) = target {
            expression =
                expression.map_child(0, control, |child| self.common_cast(child, &target))?;
            for index in 0..arm_count {
                expression = expression.map_child(base_count + index * 2, control, |child| {
                    self.common_cast(child, &target)
                })?;
            }
        }
        let ScalarExpr::Case {
            when, else_branch, ..
        } = &*expression
        else {
            unreachable!()
        };
        let target = self.context.common_type(
            else_branch
                .iter()
                .map(Box::as_ref)
                .chain(when.iter().map(|(_, result)| result)),
        )?;
        if let Some(target) = target {
            for index in 0..arm_count {
                expression =
                    expression.map_child(base_count + index * 2 + 1, control, |child| {
                        self.common_cast(child, &target)
                    })?;
            }
            if has_else {
                expression =
                    expression.map_child(base_count + arm_count * 2, control, |child| {
                        self.common_cast(child, &target)
                    })?;
            }
        }
        Ok(expression)
    }

    pub(super) fn bind_composite(
        &self,
        expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        let expression = expression.map_children(control, |_, child| self.bind(child))?;
        let Some(resolver) = self.context.resolver else {
            return Ok(expression);
        };
        let ScalarExpr::CompositeRow { binding, .. } = &*expression else {
            unreachable!()
        };
        let target = self
            .context
            .semantic(resolver.resolve_type_name_with_control(&binding.ty, control))?
            .flatten();
        match target {
            Some(target) => Ok(expression.replace_composite_type(Some(target), control)?),
            None => Ok(expression),
        }
    }

    fn user_cast_target(&self, name: &str) -> Result<Option<Produced<ColumnType>>, SQLError> {
        let Some(resolver) = self.context.resolver else {
            return Ok(None);
        };
        Ok(self
            .context
            .semantic(resolver.resolve_type_name_with_control(name, &self.context.control))?
            .flatten()
            .filter(|target| super::super::helpers::is_user_defined_type(target)))
    }

    fn fold_literal_cast(
        &self,
        expression: &ScalarExpr,
        target: &ColumnType,
        name: &str,
    ) -> Result<Option<OwnedScalarExpr>, SQLError> {
        if is_enum_bearing(target) {
            return self.fold_enum_literal(expression, target);
        }
        if !matches!(target, ColumnType::Domain { .. }) {
            return Ok(None);
        }
        let base = base_type(target);
        if !is_enum_bearing(base) {
            return Ok(None);
        }
        let Some(constant) = self.fold_enum_literal(expression, base)? else {
            return Ok(None);
        };
        let control = &self.context.control;
        Ok(Some(OwnedScalarExpr::cast(
            constant,
            control.copy_text(name)?,
            true,
            control,
        )?))
    }

    fn fold_enum_array(
        &self,
        expression: OwnedScalarExpr,
        name: &str,
    ) -> Result<OwnedScalarExpr, SQLError> {
        if !matches!(&*expression, ScalarExpr::Array(_)) {
            return Ok(expression);
        }
        let Some(target) = self.user_cast_target(name)? else {
            return Ok(expression);
        };
        let Some(mut element) = crate::type_resolution::common::array_element_type(&target) else {
            return Ok(expression);
        };
        while let ColumnType::Array(inner) = element {
            self.context.control.check()?;
            element = inner;
        }
        if !is_enum_bearing(base_type(element)) {
            return Ok(expression);
        }
        self.fold_enum_array_items(expression, element)
    }

    fn fold_enum_array_items(
        &self,
        expression: OwnedScalarExpr,
        element: &ColumnType,
    ) -> Result<OwnedScalarExpr, SQLError> {
        expression.map_children(&self.context.control, |_, child| {
            if matches!(&*child, ScalarExpr::Array(_)) {
                return self.fold_enum_array_items(child, element);
            }
            let name = element.catalog_name_with_control(&self.context.control)?;
            match self.fold_literal_cast(&child, element, &name)? {
                Some(folded) => {
                    drop(child);
                    Ok(folded)
                }
                None => Ok(child),
            }
        })
    }

    pub(super) fn bind_cast(
        &self,
        mut expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.context.control;
        expression = expression.map_child_with_parent(0, control, |parent, child| {
            let ScalarExpr::Cast { ty, .. } = parent else {
                unreachable!()
            };
            self.fold_enum_array(child, ty)
        })?;
        let ScalarExpr::Cast { expr, ty, implicit } = &*expression else {
            unreachable!()
        };
        let source = if !implicit && self.context.cast_requires_source(ty)? {
            self.context
                .semantic(self.context.infer(expr))?
                .flatten()
                .map(|source| self.context.declared_source(ty, source))
                .transpose()?
                .flatten()
        } else {
            None
        };
        expression = expression.map_child(0, control, |child| self.bind(child))?;
        let ScalarExpr::Cast { expr, ty, .. } = &*expression else {
            unreachable!()
        };
        if matches!(
            expr.as_ref(),
            ScalarExpr::Literal(Value::Str(_) | Value::Null)
        ) {
            if let Some(target) = self.user_cast_target(ty)? {
                if let Some(folded) = self.fold_literal_cast(expr, &target, ty)? {
                    drop(expression);
                    return Ok(folded);
                }
            }
        }
        if let Some(source) = source {
            expression =
                expression.map_child(0, control, |child| self.wrap_declared(child, &source))?;
        }
        Ok(expression)
    }
}
