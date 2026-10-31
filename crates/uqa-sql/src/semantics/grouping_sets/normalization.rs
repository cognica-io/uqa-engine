//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Grouping identities normalize private scalar owners without copying unadmitted syntax or input values.

use crate::{
    plan::OwnedScalarExpr, ColumnType, FunctionTypeResolver, RowSchema, SQLError, SQLParam,
    ScalarExpr,
};
use uqa_core::{
    memory::{Produced, ProductionControl, ProductionVec},
    Value,
};

pub(super) struct Normalizer<'a> {
    pub(super) engine: &'a dyn FunctionTypeResolver,
    pub(super) schema: &'a RowSchema,
    pub(super) params: &'a [SQLParam],
    pub(super) control: ProductionControl<'a>,
}

impl Normalizer<'_> {
    pub(super) fn normalize(
        &self,
        expression: OwnedScalarExpr,
    ) -> Result<OwnedScalarExpr, SQLError> {
        expression.rewrite(&self.control, |expression| self.node(expression))
    }

    fn infer(&self, expression: &ScalarExpr) -> Result<Option<Produced<ColumnType>>, SQLError> {
        crate::type_resolution::scalar_type_with_resolver_and_control(
            expression,
            self.schema,
            self.params,
            Some(self.engine),
            &self.control,
        )
    }

    fn declared(&self, name: &str) -> Result<Produced<ColumnType>, SQLError> {
        let name = self.control.copy_text(name)?;
        let (name, memory) = name.into_parts();
        let declared = self.control.finish(ColumnType::Named(name), memory)?;
        crate::type_resolution::resolve_declared_column_type_with_control(
            self.engine,
            &declared,
            &self.control,
        )
    }

    fn retained(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;
        let ScalarExpr::RetainedLiteral(literal) = &*expression else {
            unreachable!("retained literal");
        };

        // RetainedLiteral is untagged in serialized IR. Its admitted materialization has the identical identity and cannot trigger an uncontrolled second storage read during serialization.
        let value = literal.value.read_with_control(control)?;
        let materialized = match &literal.declaration {
            Some(name) => OwnedScalarExpr::typed_literal(
                value,
                control.copy_text(name)?,
                literal
                    .ty
                    .as_ref()
                    .map(|ty| ty.clone_with_control(control))
                    .transpose()?,
                literal.collation_oid,
                None,
                control,
            )?,
            None => OwnedScalarExpr::literal(value, control)?,
        };
        self.normalize(materialized)
    }

    fn function(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;

        expression = expression.canonicalize_call_name(control)?;
        let ScalarExpr::Func { name, args, .. } = &*expression else {
            unreachable!()
        };
        let count = args.len();
        let mut types = ProductionVec::new(*control);
        for argument in args {
            let (ty, memory) = match self.infer(argument)? {
                Some(ty) => {
                    let (ty, memory) = ty.into_parts();
                    (Some(ty), memory)
                }
                None => (None, control.empty_reservation()),
            };
            types.push_produced(control.finish(ty, memory)?)?;
        }
        let types = types.finish()?;
        let targets = crate::type_resolution::builtin_function_argument_targets_with_control(
            name, &types, control,
        )?;
        expression.map_children(control, |index, child| {
            if index < count {
                self.unknown(child, targets[index].as_ref())
            } else {
                self.normalize(child)
            }
        })
    }

    fn window(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;
        let ScalarExpr::WindowCall { args, filter, .. } = &*expression else {
            unreachable!("window call");
        };

        let args = args.len();
        let filter = usize::from(filter.is_some());
        let spec_end = expression.child_count() - filter;
        // The established normalizer visits the specification before arguments, then the filter.
        for index in args..spec_end {
            expression = expression.map_child(index, control, |child| self.normalize(child))?;
        }
        expression = expression.canonicalize_call_name(control)?;
        for index in 0..args {
            expression = expression.map_child(index, control, |child| self.normalize(child))?;
        }
        if filter != 0 {
            expression = expression.map_child(spec_end, control, |child| self.normalize(child))?;
        }
        Ok(expression)
    }

    fn collated(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;

        expression = expression.map_child(0, control, |child| self.normalize(child))?;
        let ScalarExpr::Collated {
            expr,
            ty,
            collation,
        } = &*expression
        else {
            unreachable!()
        };
        let oid = match (expr.as_ref(), collation.implicit_oid()) {
            (ScalarExpr::TypedLiteral { bound_type, .. }, Some(oid))
                if bound_type.as_ref() == Some(ty) =>
            {
                Some(oid)
            }
            _ => None,
        };
        if let Some(oid) = oid {
            self.normalize(
                expression
                    .select_child(0)
                    .set_literal_collation(oid, control)?,
            )
        } else {
            Ok(expression)
        }
    }

    fn typed_literal(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;
        let ScalarExpr::TypedLiteral {
            value,
            ty,
            bound_type,
            parameter_index,
            collation_oid,
            composite_source,
        } = &*expression
        else {
            unreachable!("typed literal");
        };

        let declared = match bound_type {
            Some(ty) => ty.clone_with_control(control)?,
            None => self.declared(ty)?,
        };
        let collation_oid = collation_oid.filter(|oid| {
            i64::from(*oid) != crate::catalog::type_metadata::pg_type_collation_oid(&declared)
        });
        let literal = OwnedScalarExpr::literal(control.copy_value(value)?, control)?;
        if parameter_index.is_none()
            && composite_source.is_none()
            && collation_oid.is_none()
            && self.infer(&literal)?.as_deref() == Some(&*declared)
        {
            Ok(literal)
        } else {
            Ok(expression.replace_literal_type(
                declared.catalog_name_with_control(control)?,
                declared,
                collation_oid,
                control,
            )?)
        }
    }

    fn node(&self, mut expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;
        match &*expression {
            ScalarExpr::RetainedLiteral(_) => self.retained(expression),
            ScalarExpr::Column(column) => match self.schema.column_slot(column) {
                Some(slot) => Ok(ScalarExpr::Position(slot).copy_for_rewrite(control)?),
                None => Ok(expression),
            },
            ScalarExpr::QualifiedColumn { qualifier, column } => {
                match self.schema.qualified_slot(qualifier, column) {
                    Some(slot) => Ok(ScalarExpr::Position(slot).copy_for_rewrite(control)?),
                    None => Ok(expression),
                }
            }
            ScalarExpr::Func { .. } => self.function(expression),
            ScalarExpr::Binary { lhs, rhs, .. } => {
                let left_type = self.infer(lhs)?;
                let right_type = self.infer(rhs)?;
                expression.map_children(control, |index, child| {
                    let target = if index == 0 {
                        left_type
                            .is_none()
                            .then_some(right_type.as_deref())
                            .flatten()
                    } else {
                        right_type
                            .is_none()
                            .then_some(left_type.as_deref())
                            .flatten()
                    };
                    self.unknown(child, target)
                })
            }
            ScalarExpr::UnaryMinus(_) => {
                expression = expression.map_child(0, control, |child| self.normalize(child))?;
                let ScalarExpr::UnaryMinus(inner) = &*expression else {
                    unreachable!()
                };
                if let ScalarExpr::Literal(
                    value @ (Value::Int(_) | Value::Float(_) | Value::Decimal(_)),
                ) = inner.as_ref()
                {
                    Ok(OwnedScalarExpr::literal(
                        crate::expr::negate_value_with_control(value, None, control)?,
                        control,
                    )?)
                } else {
                    Ok(expression)
                }
            }
            ScalarExpr::Between { expr, .. } | ScalarExpr::InList { expr, .. } => {
                let target = self.infer(expr)?;
                expression.map_children(control, |index, child| {
                    if index == 0 {
                        self.normalize(child)
                    } else {
                        self.unknown(child, target.as_deref())
                    }
                })
            }
            ScalarExpr::WindowCall { .. } => self.window(expression),
            ScalarExpr::Collated { .. } => self.collated(expression),
            ScalarExpr::Cast { .. } => self.cast(expression),
            ScalarExpr::TypedLiteral { .. } => self.typed_literal(expression),
            ScalarExpr::Array(_)
            | ScalarExpr::Row(_)
            | ScalarExpr::CompositeRow { .. }
            | ScalarExpr::Not(_)
            | ScalarExpr::And(_)
            | ScalarExpr::Or(_)
            | ScalarExpr::IsNull { .. }
            | ScalarExpr::Case { .. }
            | ScalarExpr::InSubquery { .. } => {
                expression.map_children(control, |_, child| self.normalize(child))
            }
            ScalarExpr::Star
            | ScalarExpr::QualifiedStar(_)
            | ScalarExpr::Default
            | ScalarExpr::Position(_)
            | ScalarExpr::InternalColumn(_)
            | ScalarExpr::Literal(_)
            | ScalarExpr::Param(_)
            | ScalarExpr::ScalarSubquery(_)
            | ScalarExpr::Exists { .. } => Ok(expression),
        }
    }

    fn cast(&self, expression: OwnedScalarExpr) -> Result<OwnedScalarExpr, SQLError> {
        let control = &self.control;
        let ScalarExpr::Cast { expr, ty, .. } = &*expression else {
            unreachable!()
        };
        let source = self.infer(expr)?;
        let target = self.declared(ty)?;
        let expression = self.normalize(expression.select_child(0))?;
        if source.as_deref() == Some(&*target) {
            return Ok(expression);
        }
        if super::input_requires_catalog(&target) {
            return Ok(OwnedScalarExpr::cast(
                expression,
                target.catalog_name_with_control(control)?,
                false,
                control,
            )?);
        }
        if let ScalarExpr::Literal(value @ Value::Str(_)) = &*expression {
            let input_type = if matches!(
                &*target,
                ColumnType::Interval | ColumnType::IntervalWithFields { .. }
            ) {
                target.clone_with_control(control)?
            } else {
                target.without_type_modifiers_with_control(control)?
            };
            let value = crate::expr::cast_value_from_with_control(
                value,
                &input_type.sql_name_with_control(control)?,
                None,
                control,
            )?;
            let input = self.normalize(OwnedScalarExpr::typed_literal(
                value,
                input_type.catalog_name_with_control(control)?,
                Some(input_type.clone_with_control(control)?),
                None,
                None,
                control,
            )?)?;
            if *input_type == *target {
                Ok(input)
            } else {
                Ok(OwnedScalarExpr::cast(
                    input,
                    target.catalog_name_with_control(control)?,
                    false,
                    control,
                )?)
            }
        } else if matches!(&*expression, ScalarExpr::Literal(Value::Null)) {
            Ok(OwnedScalarExpr::typed_literal(
                expression.into_literal_value(control)?,
                target.catalog_name_with_control(control)?,
                Some(target),
                None,
                None,
                control,
            )?)
        } else {
            Ok(OwnedScalarExpr::cast(
                expression,
                target.catalog_name_with_control(control)?,
                false,
                control,
            )?)
        }
    }

    fn unknown(
        &self,
        expression: OwnedScalarExpr,
        target: Option<&ColumnType>,
    ) -> Result<OwnedScalarExpr, SQLError> {
        let expression = if matches!(&*expression, ScalarExpr::Literal(Value::Null)) {
            match target {
                Some(target) => OwnedScalarExpr::cast(
                    expression,
                    target.catalog_name_with_control(&self.control)?,
                    true,
                    &self.control,
                )?,
                None => expression,
            }
        } else {
            expression
        };
        self.normalize(expression)
    }
}
