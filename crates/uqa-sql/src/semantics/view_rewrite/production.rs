//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Automatic view layers retain independently movable columns and scalar-query ownership.

use super::{
    context::{stored_view_schema_with_control, ViewRewriteContext},
    ColumnRestriction, ComputePlan, NotUpdatableReason, RelationalPlan, SQLError, ScalarExpr,
    SourcePlan, StoredViewKind, ViewCheckOption,
};
use crate::{
    catalog::view::ViewRewriteDefinitionRef,
    plan::{OwnedScalarExpr, ScalarQueryArena},
    RowSchema,
};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

mod columns;
mod frozen;

pub struct OwnedViewColumn {
    pub name: Produced<String>,
    pub collation_oid: Option<u32>,
    pub expression: OwnedScalarExpr,
    pub writable_source_column: Option<Produced<String>>,
}

pub struct OwnedAutomaticViewLayer {
    pub canonical_name: Produced<String>,
    pub source_name: Produced<String>,
    pub source_qualifier: Produced<String>,
    pub source_column_map: Produced<Vec<(String, String)>>,
    pub source_include_descendants: bool,
    pub source_schema: RowSchema,
    pub columns: Produced<Vec<OwnedViewColumn>>,
    pub predicate: Option<OwnedScalarExpr>,
    pub subqueries: ScalarQueryArena,
    pub check_option: ViewCheckOption,
}

impl OwnedAutomaticViewLayer {
    pub fn physical_source_column<'a>(&'a self, column: &'a str) -> &'a str {
        self.source_column_map
            .binary_search_by(|(name, _)| name.as_str().cmp(column))
            .map_or(column, |index| self.source_column_map[index].1.as_str())
    }
    pub fn column_restriction(&self, column: &OwnedViewColumn) -> ColumnRestriction {
        let name = match &*column.expression {
            ScalarExpr::Column(name) => Some(name),
            ScalarExpr::QualifiedColumn { qualifier, column }
                if super::source_qualifier_matches(
                    qualifier,
                    &self.source_qualifier,
                    &self.source_name,
                ) =>
            {
                Some(column)
            }
            _ => None,
        };
        if name.is_some_and(|name| {
            crate::schema::columns::POSTGRES_SYSTEM_COLUMNS.contains(&name.as_str())
        }) {
            ColumnRestriction::SystemColumn
        } else {
            ColumnRestriction::Computed
        }
    }
}

/// Shape analysis preserves the first rejection reason without constructing a layer for rejected views.
#[expect(
    clippy::large_enum_variant,
    reason = "the independently admitted layer moves without a temporary root allocation"
)]
pub enum OwnedViewQueryShape {
    Updatable(OwnedAutomaticViewLayer),
    NotUpdatable(NotUpdatableReason),
}

pub fn automatic_view_layer_owned(
    services: ViewRewriteContext<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedAutomaticViewLayer>, SQLError> {
    Ok(match view_query_shape_owned(services, name, control)? {
        Some(OwnedViewQueryShape::Updatable(layer)) => Some(layer),
        Some(OwnedViewQueryShape::NotUpdatable(_)) | None => None,
    })
}

pub fn view_query_shape_owned(
    services: ViewRewriteContext<'_>,
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Option<OwnedViewQueryShape>, SQLError> {
    let Some(canonical) = services
        .catalog
        .try_resolve_view_name_with_control(name, control)?
    else {
        return Ok(None);
    };
    let mut result = None;
    services
        .catalog
        .with_view_definition_with_control(&canonical, control, &mut |definition| {
            let definition = definition.ok_or_else(|| SQLError::UnknownTable(name.into()))?;
            if definition.kind == StoredViewKind::View {
                result = Some(shape_from_definition(
                    services, &canonical, definition, control,
                )?);
            }
            Ok(())
        })?;
    Ok(result)
}

fn restriction(
    services: ViewRewriteContext<'_>,
    query: &crate::plan::QueryPlan,
    control: &ProductionControl<'_>,
) -> Result<Option<NotUpdatableReason>, SQLError> {
    control.check()?;
    let block = match &query.root {
        RelationalPlan::QueryBlock(block) => block,
        RelationalPlan::SetOp { .. } => return Ok(Some(NotUpdatableReason::SetOperation)),
        RelationalPlan::Values { .. } => return Ok(Some(NotUpdatableReason::NotSingleRelation)),
    };
    Ok(if block.distinct || !block.distinct_on.is_empty() {
        Some(NotUpdatableReason::Distinct)
    } else if !block.group_by.is_empty() || !block.grouping_sets.is_empty() || block.group_distinct
    {
        Some(NotUpdatableReason::GroupBy)
    } else if block.having.is_some() {
        Some(NotUpdatableReason::Having)
    } else if !query.ctes.is_empty() {
        Some(NotUpdatableReason::With)
    } else if block.limit.is_some() || block.offset.is_some() || block.with_ties {
        Some(NotUpdatableReason::LimitOffset)
    } else if matches!(block.compute, ComputePlan::Aggregate)
        || (matches!(block.compute, ComputePlan::Window)
            && crate::semantics::aggregates::has_aggregate_with_control(
                services.routines,
                &block.projections,
                control,
            )?)
    {
        Some(NotUpdatableReason::Aggregate)
    } else if matches!(block.compute, ComputePlan::Window) {
        Some(NotUpdatableReason::Window)
    } else {
        None
    })
}

pub(super) fn shape_from_definition(
    services: ViewRewriteContext<'_>,
    canonical: &str,
    definition: ViewRewriteDefinitionRef<'_>,
    control: &ProductionControl<'_>,
) -> Result<OwnedViewQueryShape, SQLError> {
    if let Some(reason) = restriction(services, definition.query, control)? {
        return Ok(OwnedViewQueryShape::NotUpdatable(reason));
    }
    let RelationalPlan::QueryBlock(block) = &definition.query.root else {
        unreachable!("non-query blocks have a structural restriction");
    };
    let scope = services.catalog.binding_scope_with_control(control)?;
    let source_schema = match block.from.as_ref() {
        Some(source) => crate::binding::analyze_source_plan_schema_with_control(
            services.routines,
            source,
            &[],
            scope.context_with_control(control)?,
            None,
            control,
        )?,
        None => RowSchema::new_with_control(
            control.finish(Vec::new(), control.empty_reservation())?,
            control,
        )?,
    };
    let resolver = crate::binding::scoped_types::BindingTypeResolver {
        routines: services.routines,
        scope: &scope,
    };
    for projection in &block.projections {
        if crate::semantics::sets::validation::expression_may_return_set_with_control(
            services.routines,
            &resolver,
            &projection.expr,
            &source_schema,
            &[],
            control,
        )? {
            return Ok(OwnedViewQueryShape::NotUpdatable(
                NotUpdatableReason::SetReturning,
            ));
        }
    }
    let Some(SourcePlan::Table {
        name: source_name,
        qualifier,
        alias,
        column_aliases,
        include_descendants,
        ..
    }) = block.from.as_ref()
    else {
        return Ok(OwnedViewQueryShape::NotUpdatable(
            NotUpdatableReason::NotSingleRelation,
        ));
    };
    if services
        .catalog
        .target_view_kind_with_control(source_name, control)?
        == Some(StoredViewKind::Materialized)
    {
        return Ok(OwnedViewQueryShape::NotUpdatable(
            NotUpdatableReason::NotSingleRelation,
        ));
    }
    let source_qualifier = alias.as_deref().unwrap_or(qualifier);
    let source_columns = super::relation_columns_with_control(services, source_name, control)?;
    let mut selected = ProductionMap::new(*control);
    for (position, column) in source_columns.iter().enumerate() {
        selected.insert_copy(
            column_aliases.get(position).unwrap_or(column).as_str(),
            column.as_str(),
        )?;
    }
    let source_map = retain_column_mapping(&selected, control)?;
    let output = stored_view_schema_with_control(services, definition, control)?;
    let columns = columns::produce(
        block,
        source_name,
        source_qualifier,
        &source_columns,
        column_aliases,
        &selected,
        &output,
        canonical,
        control,
    )?;
    Ok(OwnedViewQueryShape::Updatable(OwnedAutomaticViewLayer {
        canonical_name: control.copy_text(canonical)?,
        source_name: control.copy_text(source_name)?,
        source_qualifier: control.copy_text(source_qualifier)?,
        source_column_map: source_map,
        source_include_descendants: *include_descendants,
        source_schema,
        columns,
        predicate: block
            .r#where
            .as_ref()
            .map(|expression| expression.copy_for_rewrite(control))
            .transpose()?,
        subqueries: ScalarQueryArena::copy_from(&block.subqueries, control)?,
        check_option: ViewCheckOption::from_options(definition.options),
    }))
}

fn retain_column_mapping(
    selected: &ProductionMap<'_, &str, &str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<(String, String)>>, SQLError> {
    let mut source_map = ProductionVec::new(*control);
    for (visible, physical) in selected {
        let visible = control.copy_text(visible)?;
        let physical = control.copy_text(physical)?;
        let (visible, left) = visible.into_parts();
        let (physical, right) = physical.into_parts();
        source_map
            .push_produced(control.finish((visible, physical), control.combine(left, right))?)?;
    }
    Ok(source_map.finish()?)
}

#[cfg(test)]
mod tests;
