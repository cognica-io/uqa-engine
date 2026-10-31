//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered parameter occurrences and their independently resolved SQL types.

use super::ownership::{AnalysisMap, AnalysisType, AnalysisVec};
use crate::catalog::collations::derivation::Derivation;
use crate::{
    plan::{OwnedScalarExpr, UnifiedPlan},
    ColumnType, SQLError, SQLParam, ScalarExpr,
};
use std::ptr::NonNull;
use uqa_core::memory::{OwnedProductionControl, Produced, ProductionControl, ProductionVec};
use uqa_core::Value;

mod application;
mod call_collations;
mod collations;
mod completion;
mod constants;
mod expression_type;
mod grouping;
mod inputs;
mod results;
pub(super) use call_collations::CallCollations;

pub(super) struct ExpressionType {
    pub(super) ty: Option<AnalysisType>,
    /// Selected output and deferred collation diagnostics from the same typed children.
    pub(super) derivation: Derivation,
    /// Field selection observes a ROW input before the enclosing expression's collation assignment.
    pub(super) initial_collation: u32,
    pub(super) record_fields: Option<crate::schema::RecordFields>,
    pub(super) record_descriptor: Option<std::sync::Arc<crate::schema::RecordDescriptor>>,
    /// A record variable without a registered descriptor has a different field diagnostic from a computed record.
    pub(super) record_variable: bool,
    /// An anonymous ROW retains each analyzed input until a named cast selects its field coercions.
    pub(super) row_constructor: Option<AnalysisVec<Self>>,
    occurrence: Option<usize>,
    // SQL unknown literals and parameters accept context coercion. Native
    // callbacks with no declared result type stay unresolved until execution.
    unknown: UnknownOrigin,
    /// The text of an `unknown` string literal, which the selected type's input function reads when a context coerces it.
    literal: Option<(NonNull<ScalarExpr>, Produced<String>)>,
    collation: Option<NonNull<ScalarExpr>>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum UnknownOrigin {
    Deferred,
    Literal,
    Parameter,
    Field,
}

pub(super) struct ParameterTypes<'a> {
    pub(super) collations: super::collations::PendingCollations,
    types: AnalysisVec<Option<Produced<ColumnType>>>,
    fixed_values: Option<&'a [SQLParam]>,
    occurrences: AnalysisVec<(usize, Option<Produced<ColumnType>>)>,
    input_constants: Option<InputConstants>,
    aliases: Option<&'a dyn crate::schema::dependencies::oid_alias::OidAliasInput>,
    enum_labels: Option<&'a dyn crate::expr::enums::EnumLabelCatalog>,
    catalog_inputs: Option<&'a dyn crate::expr::CatalogInputFunctions>,
    call_collations: Option<CallCollations>,
    query_schemas: Option<QuerySchemas>,
    /// Generated field references remain at stable addresses throughout analysis, including inference that does not retain an executable plan.
    row_expansions: RowExpansions,
    rewritten_blocks: RewrittenBlocks,
    canonicalize_parameters: bool,
    composite_inputs: Option<AnalysisMap<NonNull<ScalarExpr>, OwnedScalarExpr>>,
    reference_sites: Option<AnalysisMap<NonNull<ScalarExpr>, usize>>,
    reference_inputs: Option<AnalysisMap<NonNull<ScalarExpr>, ConvertedInput>>,
    control: OwnedProductionControl,
}

/// Original expression identities for input constants and selected fixed calls, used only while one prepared plan remains in place. No pointer is dereferenced. Analysis borrows the original expressions (including grouping aliases and table-function arguments), so neither repeated text nor CTE analysis order can conflate two inputs.
/// A constant read during analysis and retained in the executable plan.
#[derive(Debug)]
pub(super) struct ConvertedInput {
    pub(super) expression: OwnedScalarExpr,
    /// The conversion read a session setting, the transaction clock or the catalog, so an ordinary message must read the text again instead of reusing the plan.
    pub(super) session_dependent: bool,
}

pub(super) struct InputConstants(
    AnalysisMap<NonNull<ScalarExpr>, ConvertedInput>,
    AnalysisMap<NonNull<ScalarExpr>, Produced<crate::ast::FunctionBinding>>,
    AnalysisMap<NonNull<ScalarExpr>, super::membership::MembershipAnalysis>,
    collations::CollationInputs,
    results::SelectedResults,
    AnalysisMap<NonNull<ScalarExpr>, AnalysisType>,
    /// Each analyzed call's input collation, recorded on the binding the call keeps.
    AnalysisMap<NonNull<ScalarExpr>, Option<crate::catalog::collations::CollationSelection>>,
    AnalysisMap<
        NonNull<Option<crate::ast::FunctionBinding>>,
        Produced<crate::ast::FunctionBinding>,
    >,
    QuerySchemas,
    RowExpansions,
    RewrittenBlocks,
    AnalysisMap<NonNull<ScalarExpr>, usize>,
);

impl Default for InputConstants {
    fn default() -> Self {
        Self::new(&ProductionControl::uncontrolled())
    }
}

impl InputConstants {
    fn new(control: &ProductionControl<'_>) -> Self {
        Self(
            AnalysisMap::new(control),
            AnalysisMap::new(control),
            AnalysisMap::new(control),
            collations::CollationInputs::new(control),
            results::SelectedResults::new(control),
            AnalysisMap::new(control),
            AnalysisMap::new(control),
            AnalysisMap::new(control),
            QuerySchemas::new(control),
            RowExpansions::new(control),
            RewrittenBlocks::new(control),
            AnalysisMap::new(control),
        )
    }
}

type RowExpansion = AnalysisVec<(usize, Produced<Vec<OwnedScalarExpr>>)>;
type RowExpansions = AnalysisMap<NonNull<ScalarExpr>, RowExpansion>;
mod rewritten_blocks;
use rewritten_blocks::{install_rewritten_block, RewrittenBlocks};

#[derive(Default)]
struct QuerySchemas {
    groupings: grouping::GroupingInputs,
    outputs: AnalysisMap<NonNull<crate::plan::QueryPlan>, crate::RowSchema>,
    sources: AnalysisMap<NonNull<crate::plan::SourcePlan>, crate::RowSchema>,
}

impl QuerySchemas {
    fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            groupings: grouping::GroupingInputs::new(control),
            outputs: AnalysisMap::new(control),
            sources: AnalysisMap::new(control),
        }
    }
    fn apply_query(&mut self, query: &mut crate::plan::QueryPlan) {
        query.analyzed_output = self.outputs.remove(&NonNull::from(&*query));
        query.analyzed_source = match &query.root {
            crate::plan::RelationalPlan::QueryBlock(block) => block
                .from
                .as_ref()
                .and_then(|source| self.take_source(source))
                .map(|source| std::sync::Arc::new(source).into()),
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

impl<'a> ParameterTypes<'a> {
    pub(super) fn take_row_expansion(&mut self, expression: &ScalarExpr) -> RowExpansion {
        self.row_expansions
            .remove(&NonNull::from(expression))
            .unwrap_or_else(|| AnalysisVec::new(&self.control()))
    }

    pub(super) fn retain_row_expansion(
        &mut self,
        expression: &ScalarExpr,
        fields: RowExpansion,
    ) -> Result<(), SQLError> {
        if !fields.is_empty() {
            self.row_expansions
                .insert(NonNull::from(expression), fields)?;
        }
        Ok(())
    }
    pub(super) fn with_query_schemas(mut self) -> Self {
        self.query_schemas = Some(QuerySchemas::new(&self.control()));
        self
    }

    pub(super) fn apply_query_schemas(
        mut self,
        query: &mut crate::plan::QueryPlan,
        routines: &dyn super::RoutineResolution,
        params: &[SQLParam],
    ) -> Result<(), SQLError> {
        let mut schemas = self.query_schemas.unwrap_or_default();
        query.visit_queries_mut(&mut |query| {
            install_rewritten_block(&mut self.rewritten_blocks, query);
            schemas.apply_query(query);
        });
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
    ) -> Result<(), SQLError> {
        if let Some(schemas) = self.query_schemas_mut() {
            schemas
                .sources
                .insert(NonNull::from(source), schema.clone())?;
        }
        Ok(())
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
    ) -> Result<(), SQLError> {
        if positions.is_empty() {
            return Ok(());
        }
        let owner = self.control.clone();
        let control = owner.control();
        let Some(schema) = self
            .query_schemas_mut()
            .and_then(|schemas| schemas.outputs.get_mut(&NonNull::from(query)))
        else {
            return Ok(());
        };
        let original = schema.clone();
        let mut types = AnalysisVec::new(&control);
        types.reserve(original.len(), &control)?;
        for index in 0..original.len() {
            types.push(
                if positions.contains(&index) {
                    output.types[index]
                        .ty
                        .as_deref()
                        .map(|ty| ty.without_type_modifiers_with_control(&control))
                        .transpose()?
                } else {
                    None
                },
                &control,
            )?;
        }
        *schema = crate::RowSchema::with_output_metadata_with_control(
            None,
            original.columns().iter().enumerate().map(|(index, name)| {
                let (ty, collation) = if positions.contains(&index) {
                    let value = &output.types[index];
                    (
                        types[index].as_deref(),
                        value
                            .ty
                            .as_deref()
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
                        original.column_type(index),
                        original.column_collation(index),
                    )
                };
                (
                    name.as_str(),
                    ty,
                    collation,
                    original.record_metadata(index).cloned(),
                )
            }),
            false,
            &control,
        )?;
        Ok(())
    }

    pub(super) fn retain_query_output(
        &mut self,
        query: &crate::plan::QueryPlan,
        output: &super::QueryOutput,
    ) -> Result<(), SQLError> {
        let owner = self.control.clone();
        if let Some(schemas) = self.query_schemas_mut() {
            schemas
                .outputs
                .insert(NonNull::from(query), output.schema(&owner.control())?)?;
        }
        Ok(())
    }

    pub(super) fn retain_input_coercion(
        &mut self,
        expression: &ScalarExpr,
        target: AnalysisType,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.5.insert(NonNull::from(expression), target)?;
        }
        Ok(())
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

    pub(super) fn take_literal(
        &mut self,
        expression: &ScalarExpr,
    ) -> Result<Option<ConvertedInput>, SQLError> {
        let control = self.control.clone();
        let Some(constants) = self.input_constants.as_mut() else {
            return Ok(None);
        };
        constants
            .3
            .take_literal(expression, &mut constants.0, &control.control())
    }

    pub(super) fn retain_collation(
        &mut self,
        expression: &ScalarExpr,
        value: &mut ExpressionType,
        oid: u32,
    ) -> Result<(), SQLError> {
        let origin = NonNull::from(expression);
        if let Some(constants) = &mut self.input_constants {
            constants.3.select(origin, value.collation, oid)?;
        }
        value.collation = Some(origin);
        value.derivation = value.derivation.explicit(i64::from(oid));
        value.initial_collation = oid;
        Ok(())
    }

    pub(super) fn retain_membership(
        &mut self,
        expression: &ScalarExpr,
        analysis: super::membership::MembershipAnalysis,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.2.insert(NonNull::from(expression), analysis)?;
        }
        Ok(())
    }

    pub(super) fn new(types: &[Option<ColumnType>]) -> Self {
        Self::new_with_control(types, &ProductionControl::uncontrolled())
            .expect("ordinary analysis parameter types")
    }

    pub(super) fn new_with_control(
        types: &[Option<ColumnType>],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut admitted = AnalysisVec::new(control);
        admitted.reserve(types.len(), control)?;
        for ty in types {
            admitted.push(
                ty.as_ref()
                    .map(|ty| ty.clone_with_control(control))
                    .transpose()?,
                control,
            )?;
        }
        Ok(Self {
            types: admitted,
            fixed_values: None,
            occurrences: AnalysisVec::new(control),
            input_constants: None,
            collations: super::collations::PendingCollations::new(control),
            aliases: None,
            enum_labels: None,
            catalog_inputs: None,
            call_collations: None,
            query_schemas: None,
            row_expansions: RowExpansions::new(control),
            rewritten_blocks: RewrittenBlocks::new(control),
            canonicalize_parameters: false,
            composite_inputs: None,
            reference_sites: None,
            reference_inputs: None,
            control: control.capture(),
        })
    }

    pub(super) fn control(&self) -> ProductionControl<'_> {
        self.control.control()
    }

    pub(super) fn with_reference_retention(mut self) -> Self {
        self.reference_sites = Some(AnalysisMap::new(&self.control()));
        self.reference_inputs = Some(AnalysisMap::new(&self.control()));
        self
    }

    pub(super) fn take_reference_constants(&mut self) -> InputConstants {
        let mut references = InputConstants::new(&self.control());
        references.0 = self
            .reference_inputs
            .take()
            .expect("reference input observation mode");
        references.9 = std::mem::take(&mut self.row_expansions);
        references.10 = std::mem::take(&mut self.rewritten_blocks);
        references.11 = self
            .reference_sites
            .take()
            .expect("named reference observation mode");
        references
    }

    pub(super) fn with_composite_retention(mut self) -> Self {
        self.composite_inputs = Some(AnalysisMap::new(&self.control()));
        self
    }

    pub(super) fn retains_composite_inputs(&self) -> bool {
        self.composite_inputs.is_some()
    }

    pub(super) fn retain_composite_input(
        &mut self,
        expression: &ScalarExpr,
        replacement: OwnedScalarExpr,
    ) -> Result<(), SQLError> {
        self.composite_inputs
            .as_mut()
            .expect("composite observation mode")
            .insert(NonNull::from(expression), replacement)?;
        Ok(())
    }

    pub(super) fn take_composite_inputs(
        &mut self,
    ) -> AnalysisMap<NonNull<ScalarExpr>, OwnedScalarExpr> {
        self.composite_inputs
            .take()
            .expect("composite observation mode")
    }

    pub(super) fn with_routine_parameter_binding(mut self) -> Self {
        self.canonicalize_parameters = true;
        self
    }

    pub(super) fn named_reference(
        &mut self,
        expression: &ScalarExpr,
        number: usize,
    ) -> Result<ExpressionType, SQLError> {
        if let Some(sites) = &mut self.reference_sites {
            sites.insert(NonNull::from(expression), number)?;
        }
        if self.canonicalize_parameters {
            if let Some(constants) = &mut self.input_constants {
                constants.11.insert(NonNull::from(expression), number)?;
            }
        }
        self.reference(number)
    }

    /// Output binding borrows caller parameters and preserves their explicit declarations.
    pub(super) fn fixed(values: &'a [SQLParam]) -> Self {
        Self::fixed_with_control(values, &ProductionControl::uncontrolled())
            .expect("ordinary fixed parameter analysis")
    }

    pub(super) fn fixed_with_control(
        values: &'a [SQLParam],
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            fixed_values: Some(values),
            ..Self::new_with_control(&[], control)?
        })
    }

    pub(super) fn with_stored_inputs(mut self) -> Self {
        self.input_constants = Some(InputConstants::new(&self.control()));
        self
    }

    pub(super) fn retain_stored_reference(
        &mut self,
        expression: &ScalarExpr,
        column: crate::ast::InternalColumnRef,
    ) -> Result<(), SQLError> {
        let owner = self.control.clone();
        let control = owner.control();
        if let Some(constants) = &mut self.input_constants {
            constants.0.insert(
                NonNull::from(expression),
                ConvertedInput {
                    expression: ScalarExpr::InternalColumn(column).copy_for_rewrite(&control)?,
                    session_dependent: false,
                },
            )?;
        } else if let Some(references) = &mut self.reference_inputs {
            references.insert(
                NonNull::from(expression),
                ConvertedInput {
                    expression: ScalarExpr::InternalColumn(column).copy_for_rewrite(&control)?,
                    session_dependent: false,
                },
            )?;
        }
        Ok(())
    }

    pub(super) fn retain_stored_expansion(
        &mut self,
        expression: &ScalarExpr,
        replacement: OwnedScalarExpr,
    ) -> Result<(), SQLError> {
        self.reference_inputs
            .as_mut()
            .expect("stored reference observation mode")
            .insert(
                NonNull::from(expression),
                ConvertedInput {
                    expression: replacement,
                    session_dependent: false,
                },
            )?;
        Ok(())
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
        Self::with_input_constants_with_control(
            types,
            aliases,
            enum_labels,
            catalog_inputs,
            &ProductionControl::uncontrolled(),
        )
        .expect("ordinary input analysis")
    }

    pub(super) fn with_input_constants_with_control(
        types: &[Option<ColumnType>],
        aliases: Option<&'a dyn crate::schema::dependencies::oid_alias::OidAliasInput>,
        enum_labels: Option<&'a dyn crate::expr::enums::EnumLabelCatalog>,
        catalog_inputs: Option<&'a dyn crate::expr::CatalogInputFunctions>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            input_constants: Some(InputConstants::new(control)),
            aliases,
            enum_labels,
            catalog_inputs,
            ..Self::new_with_control(types, control)?
        })
    }

    /// Keep the declaration selected by analysis on the original call, without
    /// copying its arguments or repeating type inference before optimization.
    pub(super) fn retain_call(
        &mut self,
        expression: &ScalarExpr,
        selected: &crate::ast::FunctionBinding,
    ) -> Result<(), SQLError> {
        let owner = self.control.clone();
        let control = owner.control();
        if let Some(calls) = &mut self.call_collations {
            calls.record_observer(expression, selected)?;
        }
        if !selected.builtin
            || crate::fixed_builtin_return_type_with_control(selected, &control)?.is_some()
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
                constants.1.insert(
                    NonNull::from(expression),
                    selected.clone_with_control(&control)?,
                )?;
            }
        }
        Ok(())
    }

    /// Keep the FROM call's selected identity and invocation for planning without resolving it again outside its row scope.
    pub(super) fn retain_source_binding(
        &mut self,
        original: NonNull<Option<crate::ast::FunctionBinding>>,
        selected: Produced<crate::ast::FunctionBinding>,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.7.insert(original, selected)?;
        }
        Ok(())
    }

    /// Record a call's input collation on whichever binding the call keeps after analysis.
    pub(super) fn retain_input_collation(
        &mut self,
        expression: &ScalarExpr,
        collation: Option<crate::catalog::collations::CollationSelection>,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.6.insert(NonNull::from(expression), collation)?;
        } else if let Some(calls) = &mut self.call_collations {
            calls.record(expression, collation)?;
        }
        Ok(())
    }

    /// Keep a built-in selected by name on a call that had no binding, so its input collation reaches execution.
    pub(super) fn retain_builtin_selection(
        &mut self,
        expression: &ScalarExpr,
        selected: Produced<crate::ast::FunctionBinding>,
    ) -> Result<(), SQLError> {
        if let Some(constants) = &mut self.input_constants {
            constants.1.insert(NonNull::from(expression), selected)?;
        } else if let Some(calls) = &mut self.call_collations {
            calls.record_builtin(expression, selected)?;
        }
        Ok(())
    }

    /// Record only the input collations of calls in an expression whose routines are already bound.
    pub(super) fn with_call_collations(mut self) -> Self {
        self.call_collations = Some(CallCollations::new(&self.control()));
        self
    }

    pub(super) fn take_call_collations(&mut self) -> CallCollations {
        self.call_collations.take().unwrap_or_default()
    }

    pub(super) fn take_input_constants(&mut self) -> InputConstants {
        let mut constants = self.input_constants.take().unwrap_or_default();
        constants.9 = std::mem::take(&mut self.row_expansions);
        constants.10 = std::mem::take(&mut self.rewritten_blocks);
        constants
    }

    pub(super) fn reference(&mut self, number: usize) -> Result<ExpressionType, SQLError> {
        if let Some(values) = self.fixed_values {
            let parameter = ScalarExpr::Param(number);
            let control = self.control();
            let ty = crate::scalar_type_with_control(
                &parameter,
                &crate::RowSchema::empty_with_control(&control)?,
                values,
                &control,
            )?;
            let ty = if crate::type_resolution::effective_overload_argument_type_ref_with_params(
                &parameter,
                ty.as_deref(),
                values,
            )
            .is_some()
            {
                ty
            } else {
                None
            };
            let mut value = ExpressionType::resolved_produced(ty, &control)?;
            value.initial_collation =
                u32::try_from(value.derivation.oid()).expect("parameter collation OID");
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
        let control = self.control.control();
        let ty = self.types[index]
            .as_deref()
            .map(|ty| ty.clone_with_control(&control))
            .transpose()?;
        let occurrence = self.occurrences.len();
        self.occurrences.push(
            (
                index,
                self.types[index]
                    .as_deref()
                    .map(|ty| ty.clone_with_control(&control))
                    .transpose()?,
            ),
            &control,
        )?;
        let mut value = ExpressionType {
            occurrence: Some(occurrence),
            unknown: UnknownOrigin::Parameter,
            ..ExpressionType::resolved_produced(ty, &control)?
        };
        value.initial_collation =
            u32::try_from(value.derivation.oid()).expect("type collation OID");
        Ok(value)
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
        let control_owner = self.control.clone();
        let control = control_owner.control();
        let target = target.without_type_modifiers_with_control(&control)?;
        if let Some(constants) = &mut self.input_constants {
            constants.3.coerce(expression.collation, &target)?;
        }
        if let Some(occurrence) = expression.occurrence {
            let (index, observed) = &mut self.occurrences[occurrence];
            if self.types[*index]
                .as_deref()
                .is_some_and(|ty| *ty != *target)
            {
                return Err(error(
                    "42P08",
                    format!("inconsistent types deduced for parameter ${}", *index + 1),
                ));
            }
            self.types[*index] = Some(target.clone_with_control(&control)?);
            *observed = Some(target.clone_with_control(&control)?);
        }
        expression.derivation =
            expression
                .derivation
                .result(crate::catalog::type_metadata::pg_type_collation_oid(
                    &target,
                ));
        expression.replace_type(Some(target), &control)?;
        expression.initial_collation =
            u32::try_from(expression.derivation.oid()).expect("coerced collation OID");
        Ok(())
    }

    pub(super) fn values(&self) -> Result<AnalysisParameters<'_>, SQLError> {
        if let Some(values) = self.fixed_values {
            return Ok(AnalysisParameters::Borrowed(values));
        }
        let control = self.control();
        let mut output = ProductionVec::new(control);
        output.reserve(self.types.len())?;
        for ty in &self.types {
            let (parameter, memory) = match ty {
                Some(ty) => {
                    let (ty, memory) = ty.clone_with_control(&control)?.into_parts();
                    (SQLParam::typed_scalar(Value::Null, ty), memory)
                }
                None => (SQLParam::Scalar(Value::Null), control.empty_reservation()),
            };
            output.push_produced(control.finish(parameter, memory)?)?;
        }
        Ok(AnalysisParameters::Owned(output.finish()?))
    }
}

pub(super) enum AnalysisParameters<'a> {
    Borrowed(&'a [SQLParam]),
    Owned(Produced<Vec<SQLParam>>),
}

impl std::ops::Deref for AnalysisParameters<'_> {
    type Target = [SQLParam];
    fn deref(&self) -> &[SQLParam] {
        match self {
            Self::Borrowed(values) => values,
            Self::Owned(values) => values,
        }
    }
}

pub(super) fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

/// Whether an ordinary message must read `text` again. Domain arrays and composites read the catalog; a type whose input function is stable reads a session setting or the transaction clock, unless the text reproduces `value` under every setting.
fn input_reads_session(
    text: &str,
    input_type: &ColumnType,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    if crate::expr::requires_domain_array_input(input_type)
        || matches!(input_type, ColumnType::Composite(_))
    {
        return Ok(true);
    }
    if crate::type_resolution::cast_volatility_with_control(&ColumnType::Text, input_type, control)?
        == crate::ast::FunctionVolatility::Immutable
    {
        return Ok(false);
    }
    Ok(!(temporal_input_type(input_type)
        && crate::expr::text_input_is_session_independent(text, input_type, value)))
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
