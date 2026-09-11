//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation and sequence closure for routine removal.

use super::{
    analysis_relations, commit_sql_function_drop, expand_column_drop_dependencies,
    expand_routine_domain_column_drop, relation_dependents_drop_error, routine_object_dependents,
    routine_signature_types, BTreeSet, RoutineDropResolution, RoutineDropTarget,
    RoutineRemovalContext, SQLError, SQLFunctionDropPlan,
};

pub fn sequence_drop_column_names(
    context: &RoutineRemovalContext<'_>,
    relations: &BTreeSet<String>,
) -> Result<BTreeSet<(String, String)>, SQLError> {
    let sequences = context
        .dependencies
        .sequences
        .sequence_names()
        .into_iter()
        .filter(|name| relations.contains(name))
        .collect::<Vec<_>>();
    let mut columns = BTreeSet::new();
    for sequence in sequences {
        for dependent in context
            .dependencies
            .sequences
            .sequence_schema_expression_dependents(&sequence)
            .map_err(|error| {
                SQLError::Internal(format!("inspect sequence column dependencies: {error}"))
            })?
        {
            if let uqa_sql::schema::sequences::dependents::SequenceSchemaDependent::GeneratedColumn {
                table,
                column,
                ..
            } = dependent
            {
                columns.insert((table, column));
            }
        }
    }
    Ok(columns)
}

pub fn relation_drop_closure(
    context: &RoutineRemovalContext<'_>,
    mut relations: BTreeSet<String>,
) -> Result<BTreeSet<String>, SQLError> {
    let mut owners: BTreeSet<_> = context
        .dependencies
        .catalog
        .routine_table_schemas()
        .into_iter()
        .filter(|(name, _)| relations.contains(name))
        .map(|(_, table)| table.object_id())
        .collect();
    owners.extend(
        context
            .dependencies
            .catalog
            .routine_foreign_tables()
            .iter()
            .filter_map(|(identity, table)| {
                relations
                    .contains(&identity.qualified_name())
                    .then_some(table.object_id)
            }),
    );
    relations.extend(
        context
            .dependencies
            .sequences
            .sequence_names_owned_by_tables(&owners)
            .map_err(|error| SQLError::Internal(format!("inspect owned sequences: {error}")))?,
    );
    let mut pending = relations.iter().cloned().collect::<Vec<_>>();
    while let Some(relation) = pending.pop() {
        let mut views = context
            .dependencies
            .views
            .views_depending_on_relation(&relation)
            .map_err(|error| {
                SQLError::Internal(format!("inspect relation dependencies: {error}"))
            })?;
        views.extend(
            context
                .dependencies
                .views
                .views_depending_on_sequence(&relation)
                .map_err(|error| {
                    SQLError::Internal(format!("inspect sequence dependencies: {error}"))
                })?,
        );
        for view in views {
            if relations.insert(view.clone()) {
                pending.push(view);
            }
        }
    }
    Ok(relations)
}

pub fn drop_relation_routine_dependents(
    context: &RoutineRemovalContext<'_>,
    names: &[String],
    cascade: bool,
    kind: &str,
) -> Result<(), SQLError> {
    if names.is_empty() {
        return Ok(());
    }
    let registry = context.registry.routine_snapshot();
    if registry.is_empty() {
        return Ok(());
    }
    let mut relations = names.iter().cloned().collect::<BTreeSet<_>>();
    let mut resolution = RoutineDropResolution {
        targets: Vec::new(),
        seen_targets: BTreeSet::new(),
        notices: Vec::new(),
    };
    let mut domains = BTreeSet::new();
    loop {
        let previous = (relations.len(), resolution.targets.len(), domains.len());
        relations = relation_drop_closure(context, relations)?;
        let mut columns = sequence_drop_column_names(context, &relations)?;
        expand_column_drop_dependencies(context, &mut columns, &mut relations)?;
        for (name, overloads) in &registry {
            for function in overloads {
                if analysis_relations::stored_routine_references_relations(
                    &context.catalog,
                    &function.def,
                    &relations,
                )? || analysis_relations::stored_routine_references_columns(
                    context.dependencies.columns,
                    &function.def,
                    &columns,
                )? {
                    let target = RoutineDropTarget {
                        object_id: function.def.object_id,
                        name: name.clone(),
                        argument_types: routine_signature_types(&function.def),
                        is_procedure: function.def.is_procedure,
                    };
                    if resolution.seen_targets.insert(target.clone()) {
                        resolution.targets.push(target);
                    }
                }
            }
        }
        if resolution.targets.is_empty() {
            return Ok(());
        }
        if !cascade {
            return Err(relation_dependents_drop_error(context, names, kind)?);
        }
        expand_routine_domain_column_drop(
            context,
            &registry,
            &mut resolution,
            &mut domains,
            columns,
        )?;
        let dependents = routine_object_dependents(context, &resolution.targets, true)?;
        relations.extend(dependents.views.iter().cloned());
        if previous == (relations.len(), resolution.targets.len(), domains.len()) {
            return commit_sql_function_drop(
                context,
                SQLFunctionDropPlan {
                    domains,
                    targets: resolution.targets,
                    dependents,
                    notices: resolution.notices,
                },
            );
        }
    }
}
