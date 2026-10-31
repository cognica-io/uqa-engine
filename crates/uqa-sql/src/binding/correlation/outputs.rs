//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared correlation namespaces retain admitted names without evaluating their sources.

use super::{
    memory::{
        copy_ctes, copy_names, copy_scopes, extend_scopes, insert, insert_set, Analysis, Columns,
        Ctes, QueryScope, RelationColumns,
    },
    QueryPlan, RelationalPlan, SQLError, ScalarExpr, SourcePlan,
};
use std::sync::Arc;
use uqa_core::memory::{ProductionControl, ProductionVec};
mod functions;

impl QueryScope {
    fn qualified_columns(
        &self,
        qualifier: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Columns>, SQLError> {
        let Some(columns) = self.qualifiers.get(qualifier) else {
            return Ok(None);
        };
        let Some(input) = &self.enclosing else {
            return Ok(Some(Arc::clone(columns)));
        };
        let mut names = ProductionVec::new(*control);
        input.visit_qualified_star_positions(qualifier, control, |name, _, slot, _| {
            if !input.slot_is_local(slot) {
                names.push_produced(control.copy_text(name)?)?;
            }
            Ok(())
        })?;
        Ok(Some(
            RelationColumns::known(names.finish()?, control)?.share(control)?,
        ))
    }
}

pub(super) fn admits_preceding(source: &SourcePlan, inherited_lateral: bool) -> bool {
    inherited_lateral
        || matches!(
            source,
            SourcePlan::Function { .. }
                | SourcePlan::FunctionGroup { .. }
                | SourcePlan::Values { lateral: true, .. }
                | SourcePlan::Subquery { lateral: true, .. }
        )
}

pub(super) fn source_scope(
    context: Analysis<'_, '_>,
    source: &SourcePlan,
    ctes: &Ctes,
    scopes: &[QueryScope],
) -> Result<QueryScope, SQLError> {
    source_scope_in_namespace(context, source, ctes, scopes, &[], false)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves source-specific namespace and lateral lookup order"
)]
pub(super) fn source_scope_in_namespace(
    context: Analysis<'_, '_>,
    source: &SourcePlan,
    ctes: &Ctes,
    scopes: &[QueryScope],
    preceding: &[QueryScope],
    inherited_lateral: bool,
) -> Result<QueryScope, SQLError> {
    let control = &context.control;
    control.check()?;
    match source {
        SourcePlan::RetainedRows(source) => captured_scope(source.relation(), control),
        SourcePlan::Table {
            bound_columns,
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => {
            let columns = match bound_columns {
                Some(columns) => RelationColumns::known(
                    copy_names(columns.iter().map(String::as_str), control)?,
                    control,
                )?,
                None => relation_columns(context, name, ctes)?,
            }
            .apply_positional_aliases(column_aliases, control)?;
            QueryScope::relation(
                Some(alias.as_deref().unwrap_or(qualifier)),
                columns,
                control,
            )
        }
        SourcePlan::Join {
            left,
            right,
            kind,
            using,
            natural,
            alias,
            column_aliases,
            lateral,
            ..
        } => {
            let left = source_scope_in_namespace(context, left, ctes, scopes, preceding, false)?;
            let mut right_preceding = copy_scopes(preceding, control)?;
            right_preceding.push(left.copy_with_control(control)?, control)?;
            let right = source_scope_in_namespace(
                context,
                right,
                ctes,
                scopes,
                &right_preceding,
                *lateral,
            )?;
            let mut internal_relations = left.internal_relations;
            for relation in right.internal_relations {
                insert_set(&mut internal_relations, relation, control)?;
            }
            let columns =
                if (using.is_some() || *natural) && left.columns.complete && right.columns.complete
                {
                    let left_schema = crate::RowSchema::new_with_control(
                        copy_names(left.columns.ordered.iter().map(String::as_str), control)?,
                        control,
                    )?;
                    let right_schema = crate::RowSchema::new_with_control(
                        copy_names(right.columns.ordered.iter().map(String::as_str), control)?,
                        control,
                    )?;
                    let resolved = crate::semantics::resolve_join_using_with_control(
                        using.as_ref(),
                        *natural,
                        &left_schema,
                        &right_schema,
                        control,
                    )?
                    .expect("qualified join output");
                    RelationColumns::known(
                        crate::semantics::join_using_column_names_with_control(
                            *kind,
                            &left_schema,
                            &right_schema,
                            &resolved,
                            control,
                        )?,
                        control,
                    )?
                } else {
                    let mut columns = RelationColumns::known(
                        copy_names(
                            left.columns
                                .ordered
                                .iter()
                                .chain(right.columns.ordered.iter())
                                .map(String::as_str),
                            control,
                        )?,
                        control,
                    )?;
                    columns.complete = left.columns.complete && right.columns.complete;
                    columns
                }
                .apply_positional_aliases(column_aliases, control)?;
            if let Some(alias) = alias {
                return Ok(QueryScope {
                    internal_relations,
                    ..QueryScope::relation(Some(alias), columns, control)?
                });
            }
            let mut qualifiers = left.qualifiers;
            for (name, columns) in right.qualifiers {
                insert(&mut qualifiers, name, columns, control)?;
            }
            if let Some(using) = using {
                if let Some(alias) = &using.alias {
                    let columns = RelationColumns::known(
                        copy_names(using.columns.iter().map(String::as_str), control)?,
                        control,
                    )?
                    .share(control)?;
                    insert(&mut qualifiers, control.copy_text(alias)?, columns, control)?;
                }
            }
            Ok(QueryScope {
                qualifiers,
                internal_relations,
                columns: columns.share(control)?,
                enclosing: None,
            })
        }
        SourcePlan::Values {
            rows,
            alias,
            column_aliases,
            internal_relation,
            ..
        } => {
            if let Some(relation) = internal_relation {
                return captured_scope(*relation, control);
            }
            let columns = values_columns(rows.first().map_or(0, Vec::len), control)?
                .apply_positional_aliases(column_aliases, control)?;
            QueryScope::relation(alias.as_deref(), columns, control)
        }
        SourcePlan::Function {
            name,
            binding,
            args,
            output_name,
            alias,
            column_aliases,
            column_types,
            ordinality,
            ..
        } => {
            let columns = functions::columns(
                context,
                name,
                binding.as_ref(),
                args.len(),
                output_name,
                alias.as_deref(),
                column_aliases,
                !column_types.is_empty(),
                *ordinality,
            )?;
            QueryScope::relation(
                Some(alias.as_deref().unwrap_or(output_name)),
                columns,
                control,
            )
        }
        SourcePlan::FunctionGroup {
            functions: members,
            alias,
            column_aliases,
            ordinality,
        } => {
            let qualifier = alias
                .as_deref()
                .or_else(|| members.first().map(|member| member.output_name.as_str()));
            let mut ordered = ProductionVec::new(*control);
            let mut complete = true;
            for member in members {
                let columns = functions::columns(
                    context,
                    &member.name,
                    member.binding.as_ref(),
                    member.args.len(),
                    &member.output_name,
                    None,
                    &member.column_aliases,
                    !member.column_types.is_empty(),
                    false,
                )?;
                complete &= columns.complete;
                for name in columns.ordered.iter() {
                    ordered.push_produced(control.copy_text(name)?)?;
                }
            }
            let mut columns = RelationColumns::known(
                crate::semantics::apply_table_function_aliases_with_control(
                    &ordered,
                    column_aliases,
                    *ordinality,
                    control,
                )?,
                control,
            )?;
            columns.complete = complete;
            QueryScope::relation(qualifier, columns, control)
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
            captured_output,
            ..
        } => {
            let mut outer = copy_scopes(scopes, control)?;
            if admits_preceding(source, inherited_lateral) {
                extend_scopes(&mut outer, preceding, control)?;
            }
            let columns = query_output_columns(context, body, ctes, &outer)?
                .apply_positional_aliases(column_aliases, control)?;
            match captured_output {
                Some(relation) => captured_scope(*relation, control),
                None => QueryScope::relation(alias.as_deref(), columns, control),
            }
        }
    }
}

fn captured_scope(
    relation: crate::ast::InternalRelationId,
    control: &ProductionControl<'_>,
) -> Result<QueryScope, SQLError> {
    let mut scope = QueryScope::empty(control)?;
    insert_set(&mut scope.internal_relations, relation, control)?;
    Ok(scope)
}

fn relation_columns(
    context: Analysis<'_, '_>,
    name: &str,
    ctes: &Ctes,
) -> Result<RelationColumns, SQLError> {
    let control = &context.control;
    if let Some(reference) = crate::RelationIdentity::borrowed_reference(name)
        .filter(|reference| reference.schema.is_none())
    {
        let decoded = control.format(format_args!("{}", reference.name))?;
        if let Some(columns) = ctes.get(decoded.as_str()) {
            return columns.copy_with_control(control);
        }
    }
    if let Some(columns) = context
        .scoped_relations
        .and_then(|relations| relations.columns(name))
    {
        return RelationColumns::known(
            copy_names(columns.iter().map(String::as_str), control)?,
            control,
        );
    }
    if let Some(columns) =
        context
            .catalog
            .virtual_relation_columns_with_control(context.resolution, name, control)?
    {
        return RelationColumns::known(columns, control);
    }
    match context
        .catalog
        .table_resolved_with_control(context.resolution, name, control)
    {
        Ok(Some(table)) => {
            return RelationColumns::known(
                copy_names(
                    table.columns.iter().map(|column| column.name.as_str()),
                    control,
                )?,
                control,
            )
        }
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        _ => {}
    }
    let mut selected = None;
    context.catalog.with_view_definition_with_control(
        context.resolution,
        name,
        control,
        &mut |view| {
            selected = Some(match view.output_columns {
                Some(columns) => RelationColumns::known(
                    copy_names(columns.iter().map(String::as_str), control)?,
                    control,
                )?,
                None => query_output_columns(context, view.query, &Ctes::new(), &[])?,
            });
            Ok(())
        },
    )?;
    if let Some(columns) = selected {
        return Ok(columns);
    }
    match context.catalog.with_foreign_table_columns_with_control(
        context.resolution,
        name,
        control,
        &mut |columns| {
            selected = Some(RelationColumns::known(
                copy_names(columns.iter().map(|column| column.name.as_str()), control)?,
                control,
            )?);
            Ok(())
        },
    ) {
        Err(error) if matches!(error.sqlstate(), Some("53200" | "57014")) => return Err(error),
        _ => {}
    }
    match selected {
        Some(columns) => Ok(columns),
        None => RelationColumns::empty(false, control),
    }
}

fn values_columns(
    width: usize,
    control: &ProductionControl<'_>,
) -> Result<RelationColumns, SQLError> {
    let mut names = ProductionVec::new(*control);
    for index in 0..width {
        names.push_produced(control.format(format_args!("column{}", index + 1))?)?;
    }
    RelationColumns::known(names.finish()?, control)
}

pub(super) fn cte_output_columns(
    context: Analysis<'_, '_>,
    cte: &crate::plan::CtePlan,
    ctes: &Ctes,
    scopes: &[QueryScope],
) -> Result<RelationColumns, SQLError> {
    let columns = match cte.body.query() {
        Some(query) => query_output_columns(context, query, ctes, scopes)?,
        None => RelationColumns::empty(false, &context.control)?,
    };
    columns.apply_positional_aliases(&cte.columns, &context.control)
}

pub(super) fn query_output_columns(
    context: Analysis<'_, '_>,
    plan: &QueryPlan,
    parent_ctes: &Ctes,
    scopes: &[QueryScope],
) -> Result<RelationColumns, SQLError> {
    let control = &context.control;
    control.check()?;
    match &plan.root {
        RelationalPlan::QueryBlock(block)
            if !block.projections.iter().any(|projection| {
                matches!(
                    projection.expr,
                    ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
                )
            }) =>
        {
            let labels =
                crate::semantics::projection_labels_with_control(&block.projections, &[], control)?;
            return RelationColumns::known(copy_names(labels.iter(), control)?, control);
        }
        RelationalPlan::Values { rows, .. } => {
            return values_columns(rows.first().map_or(0, Vec::len), control)
        }
        _ => {}
    }
    let mut ctes = copy_ctes(parent_ctes, control)?;
    for cte in plan.ctes.iter().filter(|cte| cte.recursive) {
        let mut columns = RelationColumns::known(
            copy_names(cte.columns.iter().map(String::as_str), control)?,
            control,
        )?;
        columns.complete = false;
        insert(
            &mut ctes,
            control.copy_text(&cte.name)?,
            columns.share(control)?,
            control,
        )?;
    }
    for cte in &plan.ctes {
        let columns = cte_output_columns(context, cte, &ctes, scopes)?.share(control)?;
        insert(&mut ctes, control.copy_text(&cte.name)?, columns, control)?;
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            let local = match &block.from {
                Some(source) => source_scope(context, source, &ctes, scopes)?,
                None => QueryScope::empty(control)?,
            };
            let mut ordered = ProductionVec::new(*control);
            let mut complete = true;
            for projection in &block.projections {
                let expanded = match &projection.expr {
                    ScalarExpr::Star => Some(Arc::clone(&local.columns)),
                    ScalarExpr::QualifiedStar(qualifier) => {
                        let mut columns = local.qualified_columns(qualifier, control)?;
                        if columns.is_none() {
                            for scope in scopes.iter().rev() {
                                columns = scope.qualified_columns(qualifier, control)?;
                                if columns.is_some() {
                                    break;
                                }
                            }
                        }
                        columns
                    }
                    _ => {
                        crate::semantics::with_projection_label(projection, control, |name| {
                            ordered.push_produced(control.copy_text(name)?)?;
                            Ok(())
                        })?;
                        continue;
                    }
                };
                if let Some(columns) = expanded {
                    for name in columns.ordered.iter() {
                        ordered.push_produced(control.copy_text(name)?)?;
                    }
                    complete &= columns.complete;
                } else {
                    complete = false;
                }
            }
            let mut columns = RelationColumns::known(ordered.finish()?, control)?;
            columns.complete = complete;
            Ok(columns)
        }
        RelationalPlan::SetOp { left, .. } => query_output_columns(context, left, &ctes, scopes),
        RelationalPlan::Values { rows, .. } => {
            values_columns(rows.first().map_or(0, Vec::len), control)
        }
    }
}
