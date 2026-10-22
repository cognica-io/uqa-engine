//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Declared output namespaces used by correlation analysis, without executing sources.

use super::{
    Arc, BTreeMap, BTreeSet, CorrelationContext, QueryPlan, QueryScope, RelationColumns,
    RelationalPlan, SQLError, ScalarExpr, SourcePlan,
};

impl QueryScope {
    fn relation(qualifier: Option<&str>, columns: RelationColumns) -> Self {
        let columns = Arc::new(columns);
        Self {
            qualifiers: qualifier
                .map(|name| (name.to_owned(), Arc::clone(&columns)))
                .into_iter()
                .collect(),
            internal_relations: BTreeSet::new(),
            columns,
            enclosing: None,
        }
    }

    fn qualified_columns(&self, qualifier: &str) -> Option<Arc<RelationColumns>> {
        let columns = self.qualifiers.get(qualifier)?;
        Some(self.enclosing.as_ref().map_or_else(
            || Arc::clone(columns),
            |input| {
                Arc::new(RelationColumns::known(
                    input
                        .qualified_star_layout(qualifier)
                        .into_iter()
                        .filter(|(_, slot, _)| !input.slot_is_local(*slot))
                        .map(|(name, _, _)| name)
                        .collect(),
                ))
            },
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
    context: CorrelationContext<'_>,
    source: &SourcePlan,
    ctes: &BTreeMap<String, RelationColumns>,
    scopes: &[QueryScope],
) -> Result<QueryScope, SQLError> {
    source_scope_in_namespace(context, source, ctes, scopes, &[], false)
}

#[expect(
    clippy::too_many_lines,
    reason = "preserves each source's declared namespace"
)]
pub(super) fn source_scope_in_namespace(
    context: CorrelationContext<'_>,
    source: &SourcePlan,
    ctes: &BTreeMap<String, RelationColumns>,
    scopes: &[QueryScope],
    preceding: &[QueryScope],
    inherited_lateral: bool,
) -> Result<QueryScope, SQLError> {
    match source {
        SourcePlan::RetainedRows(source) => Ok(QueryScope {
            internal_relations: [source.relation()].into_iter().collect(),
            ..QueryScope::relation(None, RelationColumns::empty_known())
        }),
        SourcePlan::Table {
            bound_columns,
            name,
            qualifier,
            alias,
            column_aliases,
            ..
        } => {
            let mut columns = match bound_columns {
                Some(columns) => RelationColumns::known(columns.clone()),
                None => relation_columns(context, name, ctes)?,
            };
            columns.apply_positional_aliases(column_aliases);
            Ok(QueryScope::relation(
                Some(alias.as_ref().unwrap_or(qualifier)),
                columns,
            ))
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
            let mut right_preceding = preceding.to_vec();
            right_preceding.push(left.clone());
            let right = source_scope_in_namespace(
                context,
                right,
                ctes,
                scopes,
                &right_preceding,
                *lateral,
            )?;
            let mut internal_relations = left.internal_relations;
            internal_relations.extend(right.internal_relations);
            let mut columns =
                if (using.is_some() || *natural) && left.columns.complete && right.columns.complete
                {
                    let left_schema = crate::RowSchema::new(left.columns.ordered.clone());
                    let right_schema = crate::RowSchema::new(right.columns.ordered.clone());
                    let resolved = crate::semantics::resolve_join_using(
                        using.as_ref(),
                        *natural,
                        &left_schema,
                        &right_schema,
                    )?
                    .expect("qualified join has a resolved output");
                    RelationColumns::known(
                        crate::semantics::join_using_output_schema(
                            *kind,
                            &left_schema,
                            &right_schema,
                            &resolved,
                        )?
                        .columns()
                        .to_vec(),
                    )
                } else {
                    let mut columns = RelationColumns::known(
                        left.columns
                            .ordered
                            .iter()
                            .chain(&right.columns.ordered)
                            .cloned()
                            .collect(),
                    );
                    columns.complete = left.columns.complete && right.columns.complete;
                    columns
                };
            columns.apply_positional_aliases(column_aliases);
            if let Some(alias) = alias {
                return Ok(QueryScope {
                    internal_relations,
                    ..QueryScope::relation(Some(alias), columns)
                });
            }
            let mut qualifiers = left.qualifiers;
            qualifiers.extend(right.qualifiers);
            if let Some(using) = using {
                if let Some(alias) = &using.alias {
                    qualifiers.insert(
                        alias.clone(),
                        Arc::new(RelationColumns::known(using.columns.clone())),
                    );
                }
            }
            Ok(QueryScope {
                qualifiers,
                internal_relations,
                columns: Arc::new(columns),
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
            if let Some(internal_relation) = internal_relation {
                return Ok(QueryScope {
                    internal_relations: [*internal_relation].into_iter().collect(),
                    ..QueryScope::relation(None, RelationColumns::empty_known())
                });
            }
            let mut columns = values_columns(rows.first().map_or(0, Vec::len));
            columns.apply_positional_aliases(column_aliases);
            Ok(QueryScope::relation(alias.as_deref(), columns))
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
            let qualifier = Some(alias.as_deref().unwrap_or(output_name));
            if let Some((binding, function)) = binding.as_ref().and_then(|binding| {
                crate::routines::selected_bound_function(context.routines, binding)
                    .map(|function| (binding, function))
            }) {
                let columns = crate::semantics::user_table_function_columns(
                    context.routines,
                    &function,
                    binding.invocation.as_deref(),
                    output_name,
                    alias.as_deref(),
                    column_aliases,
                    *ordinality,
                )?;
                return Ok(QueryScope::relation(
                    qualifier,
                    RelationColumns::known(columns),
                ));
            }
            let builtin = binding.as_ref().is_none_or(|binding| binding.builtin)
                && crate::semantics::is_builtin_table_function(
                    &crate::semantics::builtin_function_dispatch_name(name),
                );
            let ordered = if builtin {
                crate::semantics::table_function_empty_schema(
                    name,
                    output_name,
                    alias.as_deref(),
                    column_aliases,
                    args.len(),
                    *ordinality,
                )
            } else {
                crate::semantics::apply_table_function_aliases(
                    column_aliases.clone(),
                    &[],
                    *ordinality,
                )
            };
            let mut columns = RelationColumns::known(ordered);
            columns.complete = builtin || !column_types.is_empty();
            Ok(QueryScope::relation(qualifier, columns))
        }
        SourcePlan::FunctionGroup {
            functions,
            alias,
            column_aliases,
            ordinality,
        } => {
            let qualifier = alias.as_deref().or_else(|| {
                functions
                    .first()
                    .map(|function| function.output_name.as_str())
            });
            let mut ordered = Vec::new();
            let mut complete = true;
            for function in functions {
                let member = crate::binding::sources::table_function_member_source(function);
                let columns = source_scope(context, &member, ctes, scopes)?.columns;
                complete &= columns.complete;
                ordered.extend(columns.ordered.iter().cloned());
            }
            let mut columns =
                RelationColumns::known(crate::semantics::apply_table_function_aliases(
                    ordered,
                    column_aliases,
                    *ordinality,
                ));
            columns.complete = complete;
            Ok(QueryScope::relation(qualifier, columns))
        }
        SourcePlan::Subquery {
            body,
            alias,
            column_aliases,
            ..
        } => {
            let mut outer = scopes.to_vec();
            if admits_preceding(source, inherited_lateral) {
                outer.extend_from_slice(preceding);
            }
            let mut columns = query_output_columns(context, body, ctes, &outer)?;
            columns.apply_positional_aliases(column_aliases);
            if let SourcePlan::Subquery {
                captured_output: Some(relation),
                ..
            } = source
            {
                let mut scope = QueryScope::relation(None, RelationColumns::empty_known());
                scope.internal_relations.insert(*relation);
                Ok(scope)
            } else {
                Ok(QueryScope::relation(alias.as_deref(), columns))
            }
        }
    }
}

fn relation_columns(
    context: CorrelationContext<'_>,
    name: &str,
    ctes: &BTreeMap<String, RelationColumns>,
) -> Result<RelationColumns, SQLError> {
    if let Some(columns) = crate::semantics::cte_reference_name(name)
        .as_ref()
        .and_then(|name| ctes.get(name))
    {
        return Ok(columns.clone());
    }
    if let Some(columns) = context
        .scoped_relations
        .and_then(|relations| relations.columns(name))
    {
        return Ok(RelationColumns::known(columns.to_vec()));
    }
    let catalog = context.catalog;
    let resolution = context.resolution;
    if let Some(columns) = catalog.virtual_relation_schema(resolution, name)? {
        return Ok(RelationColumns::known(
            columns.into_iter().map(|(name, _)| name).collect(),
        ));
    }
    if let Ok(Some(table)) = catalog.table_resolved(resolution, name) {
        return Ok(RelationColumns::known(
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        ));
    }
    if let Some(view) = catalog.view_resolved(resolution, name)? {
        return match view.output_columns {
            Some(columns) => Ok(RelationColumns::known(columns)),
            None => query_output_columns(context, &view.query, &BTreeMap::new(), &[]),
        };
    }
    if let Ok(Some(table)) = catalog.foreign_table_resolved(resolution, name) {
        return Ok(RelationColumns::known(
            table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect(),
        ));
    }
    Ok(RelationColumns::default())
}

fn values_columns(width: usize) -> RelationColumns {
    RelationColumns::known(
        (0..width)
            .map(|index| format!("column{}", index + 1))
            .collect(),
    )
}

pub(super) fn cte_output_columns(
    context: CorrelationContext<'_>,
    cte: &crate::plan::CtePlan,
    ctes: &BTreeMap<String, RelationColumns>,
    scopes: &[QueryScope],
) -> Result<RelationColumns, SQLError> {
    let mut columns = match cte.body.query() {
        Some(query) => query_output_columns(context, query, ctes, scopes)?,
        None => RelationColumns::default(),
    };
    columns.apply_positional_aliases(&cte.columns);
    Ok(columns)
}

pub(super) fn query_output_columns(
    context: CorrelationContext<'_>,
    plan: &QueryPlan,
    parent_ctes: &BTreeMap<String, RelationColumns>,
    scopes: &[QueryScope],
) -> Result<RelationColumns, SQLError> {
    match &plan.root {
        RelationalPlan::QueryBlock(block)
            if !block.projections.iter().any(|projection| {
                matches!(
                    projection.expr,
                    ScalarExpr::Star | ScalarExpr::QualifiedStar(_)
                )
            }) =>
        {
            return Ok(RelationColumns::known(
                crate::semantics::projection_columns(&block.projections),
            ));
        }
        RelationalPlan::Values { rows, .. } => {
            return Ok(values_columns(rows.first().map_or(0, Vec::len)));
        }
        _ => {}
    }
    let mut ctes = parent_ctes.clone();
    for cte in plan.ctes.iter().filter(|cte| cte.recursive) {
        let mut columns = RelationColumns::known(cte.columns.clone());
        columns.complete = false;
        ctes.insert(cte.name.clone(), columns);
    }
    for cte in &plan.ctes {
        let columns = cte_output_columns(context, cte, &ctes, scopes)?;
        ctes.insert(cte.name.clone(), columns);
    }
    match &plan.root {
        RelationalPlan::QueryBlock(block) => {
            let local = match &block.from {
                Some(source) => source_scope(context, source, &ctes, scopes)?,
                None => QueryScope::relation(None, RelationColumns::empty_known()),
            };
            let mut ordered = Vec::new();
            let mut complete = true;
            for projection in &block.projections {
                let expanded = match &projection.expr {
                    ScalarExpr::Star => Some(Arc::clone(&local.columns)),
                    ScalarExpr::QualifiedStar(qualifier) => {
                        local.qualified_columns(qualifier).or_else(|| {
                            scopes
                                .iter()
                                .rev()
                                .find_map(|scope| scope.qualified_columns(qualifier))
                        })
                    }
                    _ => {
                        ordered.extend(crate::semantics::projection_columns(std::slice::from_ref(
                            projection,
                        )));
                        continue;
                    }
                };
                if let Some(columns) = expanded {
                    ordered.extend(columns.ordered.iter().cloned());
                    complete &= columns.complete;
                } else {
                    complete = false;
                }
            }
            let mut columns = RelationColumns::known(ordered);
            columns.complete = complete;
            Ok(columns)
        }
        RelationalPlan::SetOp { left, .. } => query_output_columns(context, left, &ctes, scopes),
        RelationalPlan::Values { rows, .. } => Ok(values_columns(rows.first().map_or(0, Vec::len))),
    }
}
