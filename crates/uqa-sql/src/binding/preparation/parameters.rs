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
mod grouping;
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
    query_schemas: Option<QuerySchemas>,
    /// Generated field references remain at stable addresses throughout analysis, including inference that does not retain an executable plan.
    row_expansions: RowExpansions,
}

/// Original expression identities for input constants and selected fixed calls, used only while one prepared plan remains in place. No pointer is dereferenced. Analysis borrows the original expressions (including grouping aliases and table-function arguments), so neither repeated text nor CTE analysis order can conflate two inputs.
/// A constant read during analysis and retained in the executable plan.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ConvertedInput {
    pub(super) expression: ScalarExpr,
    /// The conversion read a session setting, the transaction clock or the catalog, so an ordinary message must read the text again instead of reusing the plan.
    pub(super) session_dependent: bool,
}

#[derive(Default)]
pub(super) struct InputConstants(
    BTreeMap<NonNull<ScalarExpr>, ConvertedInput>,
    BTreeMap<NonNull<ScalarExpr>, crate::ast::FunctionBinding>,
    BTreeMap<NonNull<ScalarExpr>, super::membership::MembershipAnalysis>,
    collations::CollationInputs,
    results::SelectedResults,
    BTreeMap<NonNull<ScalarExpr>, ColumnType>,
    /// Each analyzed call's input collation, recorded on the binding the call keeps.
    BTreeMap<NonNull<ScalarExpr>, Option<crate::catalog::collations::CollationSelection>>,
    BTreeMap<NonNull<Option<crate::ast::FunctionBinding>>, crate::ast::FunctionBinding>,
    QuerySchemas,
    RowExpansions,
);

type RowExpansions = BTreeMap<NonNull<ScalarExpr>, Vec<(usize, Vec<ScalarExpr>)>>;

#[derive(Default)]
struct QuerySchemas {
    groupings: grouping::GroupingInputs,
    outputs: BTreeMap<NonNull<crate::plan::QueryPlan>, crate::RowSchema>,
    sources: BTreeMap<NonNull<crate::plan::SourcePlan>, crate::RowSchema>,
}

impl QuerySchemas {
    fn apply_query(&mut self, query: &mut crate::plan::QueryPlan) {
        query.analyzed_output = self.outputs.remove(&NonNull::from(&*query));
        query.analyzed_source = match &query.root {
            crate::plan::RelationalPlan::QueryBlock(block) => block
                .from
                .as_ref()
                .and_then(|source| self.take_source(source))
                .map(std::sync::Arc::new),
            _ => None,
        };
    }

    fn take_source(
        &mut self,
        source: &crate::plan::SourcePlan,
    ) -> Option<crate::plan::AnalyzedSource> {
        let schema = self.sources.remove(&NonNull::from(source))?;
        let children = match source {
            crate::plan::SourcePlan::Join { left, right, .. } => Some(Box::new((
                self.take_source(left)?,
                self.take_source(right)?,
            ))),
            _ => None,
        };
        Some(crate::plan::AnalyzedSource { schema, children })
    }
}

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
            .all(|input| !input.session_dependent)
    }

    fn apply_node(&mut self, expression: &mut ScalarExpr) -> Result<(), SQLError> {
        let identity = NonNull::from(&*expression);
        if let Some(mut expanded) = self.9.remove(&identity) {
            for (_, fields) in &mut expanded {
                for field in fields {
                    let mut result = Ok(());
                    crate::plan::rewrite_scalar_expression(field, &mut |node| {
                        if result.is_ok() {
                            result = self.apply_node(node);
                        }
                    });
                    result?;
                }
            }
            let ScalarExpr::Row(items) = expression else {
                return Err(SQLError::Internal(
                    "analyzed ROW expansion lost its constructor".into(),
                ));
            };
            let mut expanded = expanded.into_iter().peekable();
            for (position, item) in std::mem::take(items).into_iter().enumerate() {
                if expanded.peek().is_some_and(|(index, _)| *index == position) {
                    items.extend(expanded.next().expect("selected expansion").1);
                } else {
                    items.push(item);
                }
            }
        }
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
            *expression = constant.expression;
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
        self.apply_expression_with_subqueries(
            expression,
            &mut Vec::new(),
            &super::tests::NoRoutines,
            &[],
        )
    }

    pub(super) fn apply_expression_with_subqueries(
        mut self,
        expression: &mut ScalarExpr,
        subqueries: &mut Vec<crate::plan::QueryPlan>,
        routines: &dyn super::RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        for query in subqueries.iter_mut() {
            query.visit_queries_mut(&mut |query| self.apply_query(query));
            query.visit_sources_mut(&mut |source| self.apply_source(source));
        }
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
        for query in subqueries.iter_mut() {
            self.8.groupings.apply_query(query, routines, params)?;
        }
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

    pub(super) fn apply(
        mut self,
        plan: &mut UnifiedPlan,
        routines: &dyn super::RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        plan.visit_queries_mut(&mut |query| self.apply_query(query));
        // Source selections are consumed before expression rewrites can move their enclosing nodes.
        plan.visit_sources_mut(&mut |source| self.apply_source(source));
        // Children are rewritten before their enclosing clauses move them; every retained identity is consumed at its original address.
        let mut result = Ok(());
        plan.rewrite_scalar_expressions(&mut |expression| {
            if result.is_ok() {
                result = self.apply_node(expression);
            }
        });
        result?;
        self.8.groupings.apply_plan(plan, routines, params)?;
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
            && self.7.is_empty()
            && self.8.outputs.is_empty()
            && self.8.sources.is_empty()
            && self.8.groupings.is_empty()
            && self.9.is_empty()
    }

    fn apply_query(&mut self, query: &mut crate::plan::QueryPlan) {
        self.8.apply_query(query);
    }

    fn apply_source(&mut self, source: &mut crate::plan::SourcePlan) {
        // Command inputs have no enclosing QueryPlan; their validated descriptors are not needed by query-output pruning.
        self.8.sources.remove(&NonNull::from(&*source));
        let mut apply = |binding: &mut Option<crate::ast::FunctionBinding>| {
            if let Some(selected) = self.7.remove(&NonNull::from(&*binding)) {
                *binding = Some(selected);
            }
        };
        match source {
            crate::plan::SourcePlan::Function { binding, .. } => apply(binding),
            crate::plan::SourcePlan::FunctionGroup { functions, .. } => {
                for function in functions {
                    apply(&mut function.binding);
                }
            }
            _ => {}
        }
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
    pub(super) fn take_row_expansion(
        &mut self,
        expression: &ScalarExpr,
    ) -> Vec<(usize, Vec<ScalarExpr>)> {
        self.row_expansions
            .remove(&NonNull::from(expression))
            .unwrap_or_default()
    }

    pub(super) fn retain_row_expansion(
        &mut self,
        expression: &ScalarExpr,
        fields: Vec<(usize, Vec<ScalarExpr>)>,
    ) {
        if !fields.is_empty() {
            self.row_expansions
                .insert(NonNull::from(expression), fields);
        }
    }
    pub(super) fn with_query_schemas(mut self) -> Self {
        self.query_schemas = Some(QuerySchemas::default());
        self
    }

    pub(super) fn apply_query_schemas(
        self,
        query: &mut crate::plan::QueryPlan,
        routines: &dyn super::RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let mut schemas = self.query_schemas.unwrap_or_default();
        query.visit_queries_mut(&mut |query| schemas.apply_query(query));
        schemas.groupings.apply_query(query, routines, params)
    }

    fn query_schemas(&self) -> Option<&QuerySchemas> {
        self.input_constants
            .as_ref()
            .map(|constants| &constants.8)
            .or(self.query_schemas.as_ref())
    }

    fn query_schemas_mut(&mut self) -> Option<&mut QuerySchemas> {
        self.input_constants
            .as_mut()
            .map(|constants| &mut constants.8)
            .or(self.query_schemas.as_mut())
    }

    pub(super) fn retain_source_schema(
        &mut self,
        source: &crate::plan::SourcePlan,
        schema: &crate::RowSchema,
    ) {
        if let Some(schemas) = self.query_schemas_mut() {
            schemas
                .sources
                .insert(NonNull::from(source), schema.clone());
        }
    }

    pub(super) fn query_output_schema(
        &self,
        query: &crate::plan::QueryPlan,
    ) -> Option<&crate::RowSchema> {
        self.query_schemas()?.outputs.get(&NonNull::from(query))
    }

    pub(super) fn retain_set_operand_coercions(
        &mut self,
        query: &crate::plan::QueryPlan,
        positions: &[usize],
        output: &super::QueryOutput,
    ) {
        if positions.is_empty() {
            return;
        }
        let Some(schema) = self
            .query_schemas_mut()
            .and_then(|schemas| schemas.outputs.get_mut(&NonNull::from(query)))
        else {
            return;
        };
        let original = schema.clone();
        *schema = crate::RowSchema::with_column_metadata(
            None,
            original.columns().iter().enumerate().map(|(index, name)| {
                let (ty, collation) = if positions.contains(&index) {
                    let value = &output.types[index];
                    (
                        value.ty.as_ref().map(ColumnType::without_type_modifiers),
                        value
                            .ty
                            .as_ref()
                            .filter(|ty| crate::schema::composites::type_is_collatable(ty))
                            .map(|ty| {
                                u32::try_from(crate::catalog::type_metadata::pg_type_collation_oid(
                                    ty,
                                ))
                                .expect("selected collation is an OID")
                            }),
                    )
                } else {
                    (
                        original.column_type(index).cloned(),
                        original.column_collation(index),
                    )
                };
                (name.clone(), ty, collation)
            }),
        )
        .with_record_metadata((0..original.columns().len()).filter_map(|index| {
            original
                .record_metadata(index)
                .cloned()
                .map(|metadata| (index, metadata))
        }));
    }

    pub(super) fn retain_query_output(
        &mut self,
        query: &crate::plan::QueryPlan,
        output: &super::QueryOutput,
    ) {
        if let Some(schemas) = self.query_schemas_mut() {
            schemas
                .outputs
                .insert(NonNull::from(query), output.schema());
        }
    }

    pub(super) fn retain_input_coercion(&mut self, expression: &ScalarExpr, target: ColumnType) {
        if let Some(constants) = &mut self.input_constants {
            constants.5.insert(NonNull::from(expression), target);
        }
    }

    pub(super) fn retain_result_metadata(mut self) -> Self {
        if let Some(constants) = &mut self.input_constants {
            constants.4.enabled = true;
        }
        self
    }

    pub(super) fn retain_result_metadata_for(
        &mut self,
        expression: &ScalarExpr,
        value: &ExpressionType,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.4.record(expression, value)?;
        }
        Ok(())
    }

    pub(super) fn take_literal(&mut self, expression: &ScalarExpr) -> Option<ConvertedInput> {
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
            query_schemas: None,
            row_expansions: RowExpansions::default(),
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
            || selected
                .dispatch
                .is_some_and(crate::ast::FunctionDispatch::is_concatenation)
        {
            if let Some(constants) = &mut self.input_constants {
                constants
                    .1
                    .insert(NonNull::from(expression), selected.clone());
            }
        }
    }

    /// Keep the FROM call's selected identity and invocation for planning without resolving it again outside its row scope.
    pub(super) fn retain_source_binding(
        &mut self,
        original: NonNull<Option<crate::ast::FunctionBinding>>,
        selected: crate::ast::FunctionBinding,
    ) {
        if let Some(constants) = &mut self.input_constants {
            constants.7.insert(original, selected);
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
        let mut constants = self.input_constants.take().unwrap_or_default();
        constants.9 = std::mem::take(&mut self.row_expansions);
        constants
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
            matches!(
                constants.0.get(&origin),
                Some(ConvertedInput {
                    expression: ScalarExpr::TypedLiteral { bound_type: Some(ty), .. },
                    ..
                }) if *ty == input_type
            )
        }) {
            return Ok(());
        }
        if let Some(input) = self.input_value(text, &input_type)? {
            if let Some(constants) = &mut self.input_constants {
                constants.0.insert(
                    origin,
                    ConvertedInput {
                        expression: ScalarExpr::TypedLiteral {
                            collation_oid: None,
                            composite_source: None,
                            value: input.value,
                            ty: input_type.catalog_name(),
                            bound_type: Some(input_type),
                            parameter_index: None,
                        },
                        session_dependent: input.session_dependent,
                    },
                );
            }
        }
        Ok(())
    }

    fn input_value(
        &self,
        text: &str,
        input_type: &ColumnType,
    ) -> Result<Option<ReadInput>, SQLError> {
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
            return catalog
                .read_unknown_input(text, input_type)
                .map(|value| Some(ReadInput::session_dependent(value)));
        }
        if self.input_constants.is_some() && crate::expr::enums::is_enum_bearing(input_type) {
            if let Some(value) = crate::expr::enums::fold_unknown_literal(
                self.enum_labels,
                &Value::Str(text.into()),
                input_type,
            )? {
                return Ok(Some(ReadInput::session_dependent(value)));
            }
        }
        if crate::type_resolution::catalog_input_type(input_type) {
            return self.aliases.map_or(Ok(None), |catalog| {
                crate::schema::dependencies::oid_alias::read_unknown_constant(
                    catalog, input_type, text,
                )
                .map(|value| value.map(ReadInput::session_dependent))
            });
        }
        let value = crate::expr::cast_value_from(
            &Value::Str(text.into()),
            &input_type.catalog_name(),
            None,
        )?;
        let session_dependent = input_reads_session(text, input_type, &value);
        Ok(Some(ReadInput {
            value,
            session_dependent,
        }))
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

/// A text input read by its input function, with whether an ordinary message must read it again.
struct ReadInput {
    value: Value,
    session_dependent: bool,
}

impl ReadInput {
    /// A value read through the catalog, whose descriptors a later message may see changed.
    fn session_dependent(value: Value) -> Self {
        Self {
            value,
            session_dependent: true,
        }
    }
}

/// Whether an ordinary message must read `text` again. Domain arrays and composites read the catalog; a type whose input function is stable reads a session setting or the transaction clock, unless the text reproduces `value` under every setting.
fn input_reads_session(text: &str, input_type: &ColumnType, value: &Value) -> bool {
    if crate::expr::requires_domain_array_input(input_type)
        || matches!(input_type, ColumnType::Composite(_))
    {
        return true;
    }
    if crate::type_resolution::cast_volatility(&ColumnType::Text, input_type)
        == crate::ast::FunctionVolatility::Immutable
    {
        return false;
    }
    !(temporal_input_type(input_type)
        && crate::expr::text_input_is_session_independent(text, input_type, value))
}

/// Types whose text input reads only the thread's date order, interval style and transaction clock, and arrays of them.
fn temporal_input_type(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Array(element) => temporal_input_type(element),
        ColumnType::Date
        | ColumnType::Time
        | ColumnType::TimePrecision(_)
        | ColumnType::TimeTz
        | ColumnType::TimeTzPrecision(_)
        | ColumnType::Timestamp
        | ColumnType::TimestampPrecision(_)
        | ColumnType::TimestampTz
        | ColumnType::TimestampTzPrecision(_)
        | ColumnType::Interval
        | ColumnType::IntervalWithFields { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
