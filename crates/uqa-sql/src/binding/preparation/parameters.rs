//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered parameter occurrences and their independently resolved SQL types.

use crate::{plan::UnifiedPlan, ColumnType, SQLError, SQLParam, ScalarExpr};
use std::{collections::BTreeMap, ptr::NonNull};
use uqa_core::Value;

#[derive(Clone)]
pub(super) struct ExpressionType {
    pub(super) ty: Option<ColumnType>,
    pub(super) record_fields: Option<crate::schema::RecordFields>,
    occurrence: Option<usize>,
    // SQL unknown literals and parameters accept context coercion. Native
    // callbacks with no declared result type stay unresolved until execution.
    coercible_unknown: bool,
    /// The text of an `unknown` string literal, which the selected type's input function reads when a context coerces it.
    literal: Option<(NonNull<ScalarExpr>, String)>,
}

impl ExpressionType {
    pub(super) fn resolved(ty: Option<ColumnType>) -> Self {
        Self {
            ty,
            record_fields: None,
            occurrence: None,
            coercible_unknown: false,
            literal: None,
        }
    }

    pub(super) fn unknown() -> Self {
        Self {
            ty: None,
            record_fields: None,
            occurrence: None,
            coercible_unknown: true,
            literal: None,
        }
    }

    /// A bare string literal, `unknown` until its context selects a type that reads it.
    pub(super) fn unknown_literal(expression: &ScalarExpr, text: String) -> Self {
        Self {
            ty: None,
            record_fields: None,
            occurrence: None,
            coercible_unknown: true,
            literal: Some((NonNull::from(expression), text)),
        }
    }

    pub(super) fn is_deferred(&self) -> bool {
        self.ty.is_none() && !self.coercible_unknown
    }
}

pub(super) struct ParameterTypes<'a> {
    types: Vec<Option<ColumnType>>,
    occurrences: Vec<(usize, Option<ColumnType>)>,
    input_constants: Option<InputConstants>,
    aliases: Option<&'a dyn crate::schema::dependencies::oid_alias::OidAliasInput>,
    enum_labels: Option<&'a dyn crate::expr::enums::EnumLabelCatalog>,
    catalog_inputs: Option<&'a dyn crate::expr::CatalogInputFunctions>,
}

/// Original leaf identities, used only while one prepared plan remains in place. No pointer is dereferenced. Analysis borrows the original expressions (including grouping aliases and table-function arguments), so neither repeated text nor CTE analysis order can conflate two inputs.
#[derive(Default)]
pub(super) struct InputConstants(BTreeMap<NonNull<ScalarExpr>, ScalarExpr>);

impl InputConstants {
    /// Ordinary messages must repeat session/catalog-dependent input functions.
    /// Prepared definitions intentionally retain these same converted constants.
    pub(super) fn reusable_across_messages(&self) -> bool {
        self.0.values().all(|expression| {
            let ScalarExpr::TypedLiteral {
                bound_type: Some(ty),
                ..
            } = expression
            else {
                return false;
            };
            !crate::expr::requires_domain_array_input(ty)
                && !matches!(ty, ColumnType::Composite(_))
                && crate::type_resolution::cast_volatility(&ColumnType::Text, ty)
                    == crate::ast::FunctionVolatility::Immutable
        })
    }

    pub(super) fn apply_expression(mut self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        crate::plan::rewrite_scalar_expression(expression, &mut |node| {
            if let Some(constant) = self.0.remove(&NonNull::from(&*node)) {
                *node = constant;
            }
        });
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "default input constant did not belong to the analyzed expression".into(),
            ))
        }
    }

    pub(super) fn apply(mut self, plan: &mut UnifiedPlan) -> Result<(), SQLError> {
        // Replacing leaves preserves every other node's address; the plan is neither cloned nor moved between analysis and this walk.
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let Some(constant) = self.0.remove(&NonNull::from(&*expression)) {
                *expression = constant;
            }
        });
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "prepared input constant did not belong to the analyzed plan".into(),
            ))
        }
    }
}

impl<'a> ParameterTypes<'a> {
    pub(super) fn new(types: &[Option<ColumnType>]) -> Self {
        Self {
            types: types.to_vec(),
            occurrences: Vec::new(),
            input_constants: None,
            aliases: None,
            enum_labels: None,
            catalog_inputs: None,
        }
    }

    pub(super) fn with_input_constants(
        types: &[Option<ColumnType>],
        aliases: Option<&'a dyn crate::schema::dependencies::oid_alias::OidAliasInput>,
        enum_labels: Option<&'a dyn crate::expr::enums::EnumLabelCatalog>,
        catalog_inputs: Option<&'a dyn crate::expr::CatalogInputFunctions>,
    ) -> Self {
        Self {
            input_constants: Some(InputConstants::default()),
            aliases,
            enum_labels,
            catalog_inputs,
            ..Self::new(types)
        }
    }

    pub(super) fn take_input_constants(&mut self) -> InputConstants {
        self.input_constants.take().unwrap_or_default()
    }

    pub(super) fn reference(&mut self, number: usize) -> Result<ExpressionType, SQLError> {
        let index = number
            .checked_sub(1)
            .filter(|index| *index < self.types.len())
            .ok_or_else(|| error("42P02", format!("there is no parameter ${number}")))?;
        let ty = self.types[index].clone();
        let occurrence = self.occurrences.len();
        self.occurrences.push((index, ty.clone()));
        Ok(ExpressionType {
            ty,
            record_fields: None,
            occurrence: Some(occurrence),
            coercible_unknown: true,
            literal: None,
        })
    }

    pub(super) fn coerce_unknown(
        &mut self,
        expression: &mut ExpressionType,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        if expression.ty.is_some() || !expression.coercible_unknown {
            return Ok(());
        }
        let target = target.without_type_modifiers();
        if let Some((origin, text)) = &expression.literal {
            self.read_literal(*origin, text, &target)?;
        }
        if let Some(occurrence) = expression.occurrence {
            let (index, observed) = &mut self.occurrences[occurrence];
            if self.types[*index].as_ref().is_some_and(|ty| *ty != target) {
                return Err(error(
                    "42P08",
                    format!("inconsistent types deduced for parameter ${}", *index + 1),
                ));
            }
            self.types[*index] = Some(target.clone());
            *observed = Some(target.clone());
        }
        expression.ty = Some(target);
        Ok(())
    }

    fn read_literal(
        &mut self,
        origin: NonNull<ScalarExpr>,
        text: &str,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        // Scalar domains read their base input and keep an outer runtime check;
        // an array input invokes the domain input function for every element.
        let mut input_type = target;
        while let ColumnType::Domain { base, .. } = input_type {
            input_type = base;
        }
        let input_type = input_type.without_type_modifiers();
        if self.input_constants.as_ref().is_some_and(|constants| {
            matches!(constants.0.get(&origin), Some(ScalarExpr::TypedLiteral { bound_type: Some(ty), .. }) if *ty == input_type)
        }) {
            return Ok(());
        }
        if let Some(value) = self.input_value(text, &input_type)? {
            if let Some(constants) = &mut self.input_constants {
                constants.0.insert(
                    origin,
                    ScalarExpr::TypedLiteral {
                        value,
                        ty: input_type.catalog_name(),
                        bound_type: Some(input_type),
                        parameter_index: None,
                    },
                );
            }
        }
        Ok(())
    }

    fn input_value(&self, text: &str, input_type: &ColumnType) -> Result<Option<Value>, SQLError> {
        if crate::expr::requires_domain_array_input(input_type) {
            if self.input_constants.is_none() {
                return Ok(None);
            }
            let catalog = self.catalog_inputs.ok_or_else(|| {
                SQLError::Internal(format!(
                    "catalog input functions are unavailable for {}",
                    input_type.sql_name()
                ))
            })?;
            return catalog.read_unknown_input(text, input_type).map(Some);
        }
        if self.input_constants.is_some() && crate::expr::enums::is_enum_bearing(input_type) {
            if let Some(value) = crate::expr::enums::fold_unknown_literal(
                self.enum_labels,
                &Value::Str(text.into()),
                input_type,
            )? {
                return Ok(Some(value));
            }
        }
        if crate::type_resolution::catalog_input_type(input_type) {
            return self.aliases.map_or(Ok(None), |catalog| {
                crate::schema::dependencies::oid_alias::read_unknown_constant(
                    catalog, input_type, text,
                )
            });
        }
        crate::expr::cast_value_from(&Value::Str(text.into()), &input_type.catalog_name(), None)
            .map(Some)
    }

    pub(super) fn values(&self) -> Vec<SQLParam> {
        self.types
            .iter()
            .map(|ty| match ty {
                Some(ty) => SQLParam::typed_scalar(Value::Null, ty.clone()),
                None => SQLParam::Scalar(Value::Null),
            })
            .collect()
    }

    pub(super) fn finish(self) -> Result<Vec<Option<ColumnType>>, SQLError> {
        for (index, observed) in self.occurrences {
            if observed != self.types[index] {
                return Err(error(
                    "42P08",
                    format!("could not determine data type of parameter ${}", index + 1),
                ));
            }
        }
        if let Some(index) = self.types.iter().position(Option::is_none) {
            return Err(error(
                "42P18",
                format!("could not determine data type of parameter ${}", index + 1),
            ));
        }
        Ok(self.types)
    }
}

pub(super) fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

#[cfg(test)]
mod tests;
