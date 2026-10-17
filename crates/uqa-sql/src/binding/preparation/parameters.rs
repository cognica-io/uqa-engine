//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered parameter occurrences and their independently resolved SQL types.

use crate::catalog::collations::derivation::Derivation;
use crate::{plan::UnifiedPlan, ColumnType, SQLError, SQLParam, ScalarExpr};
use std::{borrow::Cow, collections::BTreeMap, ptr::NonNull};
use uqa_core::Value;

mod call_collations;
mod collations;
mod results;
pub(super) use call_collations::CallCollations;

#[derive(Clone)]
pub(super) struct ExpressionType {
    pub(super) ty: Option<ColumnType>,
    /// Selected output and deferred collation diagnostics from the same typed children.
    pub(super) derivation: Derivation,
    /// Field selection observes a ROW input before the enclosing expression's collation assignment.
    pub(super) initial_collation: u32,
    pub(super) record_fields: Option<crate::schema::RecordFields>,
    pub(super) record_descriptor: Option<std::sync::Arc<crate::schema::RecordDescriptor>>,
    /// A record variable without a registered descriptor has a different field diagnostic from a computed record.
    pub(super) record_variable: bool,
    /// An anonymous ROW retains each analyzed input until a named cast selects its field coercions.
    pub(super) row_constructor: Option<Vec<Self>>,
    occurrence: Option<usize>,
    // SQL unknown literals and parameters accept context coercion. Native
    // callbacks with no declared result type stay unresolved until execution.
    unknown: UnknownOrigin,
    /// The text of an `unknown` string literal, which the selected type's input function reads when a context coerces it.
    literal: Option<(NonNull<ScalarExpr>, String)>,
    collation: Option<NonNull<ScalarExpr>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnknownOrigin {
    Deferred,
    Literal,
    Parameter,
    Field,
}

impl ExpressionType {
    pub(super) fn default_collation(ty: Option<&ColumnType>) -> Derivation {
        Derivation::implicit(ty.map_or(0, crate::catalog::type_metadata::pg_type_collation_oid))
    }

    pub(super) fn combined(ty: Option<ColumnType>, values: &[Self]) -> Self {
        let selected = values.iter().fold(Derivation::None, |state, value| {
            state.combine(value.derivation)
        });
        let mut result = Self::resolved(ty);
        result.derivation = selected.result(result.derivation.oid());
        result
    }

    pub(super) fn comparison(left: &Self, right: &Self) -> Derivation {
        if let (Some(left), Some(right)) = (&left.row_constructor, &right.row_constructor) {
            left.iter()
                .zip(right)
                .fold(Derivation::None, |state, (left, right)| {
                    state.combine(left.derivation.combine(right.derivation).error_only())
                })
        } else {
            left.derivation.combine(right.derivation).error_only()
        }
    }

    pub(super) fn independent(ty: Option<ColumnType>, values: &[Self]) -> Self {
        let mut result = Self::resolved(ty);
        result.derivation = values.iter().fold(result.derivation, |state, value| {
            state.combine(value.derivation.error_only())
        });
        result
    }

    pub(super) fn resolved(ty: Option<ColumnType>) -> Self {
        Self {
            derivation: Self::default_collation(ty.as_ref()),
            initial_collation: 0,
            ty,
            record_fields: None,
            record_descriptor: None,
            record_variable: false,
            row_constructor: None,
            occurrence: None,
            unknown: UnknownOrigin::Deferred,
            literal: None,
            collation: None,
        }
    }

    pub(super) fn unknown() -> Self {
        Self {
            unknown: UnknownOrigin::Literal,
            ..Self::resolved(None)
        }
    }

    /// A bare string literal, `unknown` until its context selects a type that reads it.
    pub(super) fn unknown_literal(expression: &ScalarExpr, text: String) -> Self {
        Self {
            literal: Some((NonNull::from(expression), text)),
            ..Self::unknown()
        }
    }

    pub(super) fn is_deferred(&self) -> bool {
        self.ty.is_none() && self.unknown == UnknownOrigin::Deferred
    }

    pub(super) fn is_sql_unknown(&self) -> bool {
        self.ty.is_none() && self.unknown != UnknownOrigin::Deferred
    }

    pub(super) fn is_unknown_parameter(&self) -> bool {
        self.ty.is_none() && self.unknown == UnknownOrigin::Parameter
    }

    pub(super) fn mark_unknown_field(&mut self) {
        self.unknown = UnknownOrigin::Field;
    }

    pub(super) fn leaf(ty: Option<ColumnType>) -> Self {
        let mut value = Self::resolved(ty);
        value.initial_collation =
            u32::try_from(value.derivation.oid()).expect("type collation OID");
        value
    }

    pub(super) fn record_metadata(&self) -> Option<crate::schema::RecordMetadata> {
        (self.record_fields.is_some() || self.record_descriptor.is_some()).then(|| {
            crate::schema::RecordMetadata {
                types: self.record_fields.clone(),
                descriptor: self.record_descriptor.clone(),
            }
        })
    }
}

pub(super) struct ParameterTypes<'a> {
    pub(super) collations: super::collations::PendingCollations,
    types: Vec<Option<ColumnType>>,
    fixed_values: Option<&'a [SQLParam]>,
    occurrences: Vec<(usize, Option<ColumnType>)>,
    input_constants: Option<InputConstants>,
    aliases: Option<&'a dyn crate::schema::dependencies::oid_alias::OidAliasInput>,
    enum_labels: Option<&'a dyn crate::expr::enums::EnumLabelCatalog>,
    catalog_inputs: Option<&'a dyn crate::expr::CatalogInputFunctions>,
    call_collations: Option<CallCollations>,
}

/// Original expression identities for input constants and selected fixed calls, used only while one prepared plan remains in place. No pointer is dereferenced. Analysis borrows the original expressions (including grouping aliases and table-function arguments), so neither repeated text nor CTE analysis order can conflate two inputs.
#[derive(Default)]
pub(super) struct InputConstants(
    BTreeMap<NonNull<ScalarExpr>, ScalarExpr>,
    BTreeMap<NonNull<ScalarExpr>, crate::ast::FunctionBinding>,
    BTreeMap<NonNull<ScalarExpr>, super::membership::MembershipAnalysis>,
    collations::CollationInputs,
    results::CollationResults,
    BTreeMap<NonNull<ScalarExpr>, ColumnType>,
    /// Each analyzed call's input collation, recorded on the binding the call keeps.
    BTreeMap<NonNull<ScalarExpr>, Option<crate::catalog::collations::CollationSelection>>,
);

impl InputConstants {
    /// Ordinary messages must repeat session/catalog-dependent input functions.
    /// Prepared definitions intentionally retain these same converted constants.
    pub(super) fn reusable_across_messages(&self) -> bool {
        self.0
            .values()
            .chain(
                self.2
                    .values()
                    .flat_map(|membership| membership.left_constants.iter().flatten()),
            )
            .all(|mut expression| {
                loop {
                    match crate::catalog::collations::expression::scalar_clause(expression) {
                        Ok(Some(clause)) => expression = clause.argument,
                        Ok(None) => break,
                        Err(_) => return false,
                    }
                }
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

    fn apply_node(&mut self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        let identity = NonNull::from(&*expression);
        if let Some(selected) = self.1.remove(&identity) {
            if let ScalarExpr::Func { binding, .. } = expression {
                *binding = Some(selected);
                crate::type_resolution::json_path::normalize_retained(expression)?;
            }
        }
        if let Some(collation) = self.6.remove(&identity) {
            if let ScalarExpr::Func {
                binding: Some(binding),
                ..
            } = expression
            {
                binding.input_collation = collation;
            }
        }
        if let Some(constant) = self.0.remove(&identity) {
            *expression = constant;
        }
        self.3.apply(identity, expression)?;
        self.4.apply(identity, expression)?;
        if let Some(target) = self.5.remove(&identity) {
            let inner = std::mem::replace(expression, ScalarExpr::Literal(Value::Null));
            *expression = ScalarExpr::Cast {
                expr: Box::new(inner),
                ty: target.catalog_name(),
                implicit: true,
            };
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn apply_expression(self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        self.apply_expression_with_subqueries(expression, &mut Vec::new())
    }

    pub(super) fn apply_expression_with_subqueries(
        mut self,
        expression: &mut ScalarExpr,
        subqueries: &mut Vec<crate::plan::QueryPlan>,
    ) -> Result<(), SQLError> {
        let mut result = Ok(());
        let mut apply = |node: &mut ScalarExpr| {
            if result.is_ok() {
                result = self.apply_node(node);
            }
        };
        crate::plan::rewrite_scalar_expression(expression, &mut apply);
        for query in subqueries.iter_mut() {
            query.rewrite_scalar_expressions(&mut apply);
        }
        result?;
        if !self.2.is_empty() {
            crate::plan::subqueries::rewrite_expression_with_arena(
                expression,
                subqueries,
                &mut |node, arena| self.apply_membership(node, arena),
            )?;
        }
        if self.is_consumed() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "default input constant did not belong to the analyzed expression".into(),
            ))
        }
    }

    pub(super) fn apply(mut self, plan: &mut UnifiedPlan) -> Result<(), SQLError> {
        // Children are rewritten before their enclosing clauses move them; every retained identity is consumed at its original address.
        let mut result = Ok(());
        plan.rewrite_scalar_expressions(&mut |expression| {
            if result.is_ok() {
                result = self.apply_node(expression);
            }
        });
        result?;
        if !self.2.is_empty() {
            crate::plan::subqueries::rewrite_with_arenas(plan, &mut |node, arena| {
                self.apply_membership(node, arena)
            })?;
        }
        if self.is_consumed() {
            Ok(())
        } else {
            Err(SQLError::Internal(
                "prepared input constant did not belong to the analyzed plan".into(),
            ))
        }
    }

    fn is_consumed(&self) -> bool {
        self.0.is_empty()
            && self.1.is_empty()
            && self.2.is_empty()
            && self.3.is_empty()
            && self.4.is_empty()
            && self.5.is_empty()
            && self.6.is_empty()
    }

    fn apply_membership(
        &mut self,
        node: &mut ScalarExpr,
        arena: &mut Vec<crate::plan::QueryPlan>,
    ) -> Result<(), SQLError> {
        if let Some(analysis) = self.2.remove(&NonNull::from(&*node)) {
            analysis.apply(node, arena)?;
        }
        Ok(())
    }
}

impl<'a> ParameterTypes<'a> {
    pub(super) fn retain_input_coercion(&mut self, expression: &ScalarExpr, target: ColumnType) {
        if let Some(constants) = &mut self.input_constants {
            constants.5.insert(NonNull::from(expression), target);
        }
    }

    pub(super) fn retain_result_collations(mut self) -> Self {
        if let Some(constants) = &mut self.input_constants {
            constants.4.enabled = true;
        }
        self
    }

    pub(super) fn retain_result_collation(
        &mut self,
        expression: &ScalarExpr,
        value: &ExpressionType,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.4.record(expression, value)?;
        }
        Ok(())
    }

    pub(super) fn take_literal(&mut self, expression: &ScalarExpr) -> Option<ScalarExpr> {
        let constants = self.input_constants.as_mut()?;
        constants.3.take_literal(expression, &mut constants.0)
    }

    pub(super) fn retain_collation(
        &mut self,
        expression: &ScalarExpr,
        value: &mut ExpressionType,
        oid: u32,
    ) {
        let origin = NonNull::from(expression);
        if let Some(constants) = &mut self.input_constants {
            constants.3.select(origin, value.collation, oid);
        }
        value.collation = Some(origin);
        value.derivation = value.derivation.explicit(i64::from(oid));
        value.initial_collation = oid;
    }

    pub(super) fn retain_membership(
        &mut self,
        expression: &ScalarExpr,
        analysis: super::membership::MembershipAnalysis,
    ) {
        if let Some(constants) = &mut self.input_constants {
            constants.2.insert(NonNull::from(expression), analysis);
        }
    }

    pub(super) fn new(types: &[Option<ColumnType>]) -> Self {
        Self {
            types: types.to_vec(),
            fixed_values: None,
            occurrences: Vec::new(),
            input_constants: None,
            collations: super::collations::PendingCollations::default(),
            aliases: None,
            enum_labels: None,
            catalog_inputs: None,
            call_collations: None,
        }
    }

    /// Output binding borrows caller parameters and preserves their explicit declarations.
    pub(super) fn fixed(values: &'a [SQLParam]) -> Self {
        Self {
            fixed_values: Some(values),
            ..Self::new(&[])
        }
    }

    /// Fixed output binding can describe a subtree after ordered statement analysis, without its enclosing SQL clause.
    pub(super) fn is_output_binding(&self) -> bool {
        self.fixed_values.is_some()
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

    /// Keep the declaration selected by analysis on the original call, without
    /// copying its arguments or repeating type inference before optimization.
    pub(super) fn retain_call(
        &mut self,
        expression: &ScalarExpr,
        selected: &crate::ast::FunctionBinding,
    ) {
        if !selected.builtin
            || crate::fixed_builtin_return_type(selected).is_some()
            || crate::catalog::json_path::JsonPathFunction::lookup(&selected.name).is_some()
            || matches!(
                selected.dispatch,
                Some(crate::ast::FunctionDispatch::ArraySlicesWithBounds(_))
            )
        {
            if let Some(constants) = &mut self.input_constants {
                constants
                    .1
                    .insert(NonNull::from(expression), selected.clone());
            }
        }
    }

    /// Record a call's input collation on whichever binding the call keeps after analysis.
    pub(super) fn retain_input_collation(
        &mut self,
        expression: &ScalarExpr,
        collation: Option<crate::catalog::collations::CollationSelection>,
    ) {
        if let Some(constants) = &mut self.input_constants {
            constants.6.insert(NonNull::from(expression), collation);
        } else if let Some(calls) = &mut self.call_collations {
            calls.record(expression, collation);
        }
    }

    /// Keep a built-in selected by name on a call that had no binding, so its input collation reaches execution.
    pub(super) fn retain_builtin_selection(
        &mut self,
        expression: &ScalarExpr,
        selected: crate::ast::FunctionBinding,
    ) {
        if let Some(constants) = &mut self.input_constants {
            constants.1.insert(NonNull::from(expression), selected);
        } else if let Some(calls) = &mut self.call_collations {
            calls.record_builtin(expression, selected);
        }
    }

    /// Record only the input collations of calls in an expression whose routines are already bound.
    pub(super) fn with_call_collations(mut self) -> Self {
        self.call_collations = Some(CallCollations::default());
        self
    }

    pub(super) fn take_call_collations(&mut self) -> CallCollations {
        self.call_collations.take().unwrap_or_default()
    }

    pub(super) fn take_input_constants(&mut self) -> InputConstants {
        self.input_constants.take().unwrap_or_default()
    }

    pub(super) fn reference(&mut self, number: usize) -> Result<ExpressionType, SQLError> {
        if let Some(values) = self.fixed_values {
            let parameter = ScalarExpr::Param(number);
            let ty = crate::effective_overload_argument_type_with_params(
                &parameter,
                crate::scalar_type(&parameter, &crate::RowSchema::default(), values)?,
                values,
            );
            let mut value = ExpressionType::leaf(ty);
            if number
                .checked_sub(1)
                .is_some_and(|index| index < values.len())
            {
                value.unknown = UnknownOrigin::Parameter;
            }
            return Ok(value);
        }
        let index = number
            .checked_sub(1)
            .filter(|index| *index < self.types.len())
            .ok_or_else(|| error("42P02", format!("there is no parameter ${number}")))?;
        let ty = self.types[index].clone();
        let occurrence = self.occurrences.len();
        self.occurrences.push((index, ty.clone()));
        Ok(ExpressionType {
            occurrence: Some(occurrence),
            unknown: UnknownOrigin::Parameter,
            ..ExpressionType::leaf(ty)
        })
    }

    pub(super) fn coerce_unknown(
        &mut self,
        expression: &mut ExpressionType,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        if !expression.is_sql_unknown() {
            return Ok(());
        }
        if expression.unknown == UnknownOrigin::Field {
            return Err(crate::type_resolution::field_selection::unknown_conversion_error(target));
        }
        if let Some((origin, text)) = &expression.literal {
            self.read_literal(*origin, text, target)?;
        }
        let target = target.without_type_modifiers();
        if let Some(constants) = &mut self.input_constants {
            constants.3.coerce(expression.collation, &target)?;
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
        expression.derivation =
            expression
                .derivation
                .result(crate::catalog::type_metadata::pg_type_collation_oid(
                    &target,
                ));
        expression.ty = Some(target);
        expression.initial_collation =
            u32::try_from(expression.derivation.oid()).expect("coerced collation OID");
        Ok(())
    }

    fn read_literal(
        &mut self,
        origin: NonNull<ScalarExpr>,
        text: &str,
        target: &ColumnType,
    ) -> Result<(), SQLError> {
        let input_type = crate::type_resolution::literal_input_type_with_control(
            target,
            &uqa_core::memory::ProductionControl::uncontrolled(),
        )?
        .into_uncontrolled()
        .expect("ordinary literal input has no reservation");
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
                        collation_oid: None,
                        composite_source: None,
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
        if crate::expr::requires_catalog_constant_input(input_type) {
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

    pub(super) fn values(&self) -> Cow<'_, [SQLParam]> {
        if let Some(values) = self.fixed_values {
            return Cow::Borrowed(values);
        }
        Cow::Owned(
            self.types
                .iter()
                .map(|ty| match ty {
                    Some(ty) => SQLParam::typed_scalar(Value::Null, ty.clone()),
                    None => SQLParam::Scalar(Value::Null),
                })
                .collect(),
        )
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
