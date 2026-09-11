//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Joint routine/domain cascade expansion and namespace removal.

use super::{
    analysis_relations, commit_sql_function_drop, domain_dependencies,
    expand_column_drop_dependencies, expand_stored_routine_drop_dependents, relation_drop_closure,
    routine_object_dependents, routine_signature_types, sequence_drop_column_names, Arc, BTreeMap,
    BTreeSet, RoutineDropResolution, RoutineDropTarget, RoutineRemovalContext, SQLError,
    SQLFunctionDropPlan, SQLUserFunction,
};

pub fn drop_domain_types_and_routines(
    context: &RoutineRemovalContext<'_>,
    targets: &BTreeSet<u32>,
    cascade: bool,
) -> Result<(), SQLError> {
    if targets.is_empty() {
        return Ok(());
    }
    let registry = context.registry.routine_snapshot();
    let mut resolution = RoutineDropResolution {
        targets: Vec::new(),
        seen_targets: BTreeSet::new(),
        notices: Vec::new(),
    };
    let mut domains = targets.clone();
    expand_routine_domain_drop(context, &registry, &mut resolution, &mut domains)?;
    if !cascade
        && (domains != *targets
            || !resolution.targets.is_empty()
            || domain_dependencies::domain_drop_has_dependents(&context.domains, targets)?)
    {
        let message = if targets.len() == 1 {
            let oid = *targets.first().expect("one root domain");
            let name = crate::catalog::projection::resolve_regtype_output(
                &context.catalog,
                &uqa_sql::ast::ColumnType::Regtype,
                i64::from(oid),
            )
            .map_err(SQLError::Internal)?
            .ok_or_else(|| SQLError::Internal("DROP DOMAIN target disappeared".into()))?;
            format!("cannot drop type {name} because other objects depend on it")
        } else {
            "cannot drop desired object(s) because other objects depend on them".into()
        };
        return Err(SQLError::Routine {
            sqlstate: "2BP01".into(),
            message,
        });
    }
    let dependents = routine_object_dependents(context, &resolution.targets, true)?;
    commit_sql_function_drop(
        context,
        SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices: resolution.notices,
        },
    )
}

pub fn drop_schema_types_and_routines(
    context: &RoutineRemovalContext<'_>,
    schemas: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let registry = context.registry.routine_snapshot();
    let mut resolution = analysis_relations::schema_routine_drop_targets(&registry, schemas)?;
    let mut domains = context
        .domains
        .catalog
        .domain_definitions()
        .values()
        .filter(|domain| schemas.contains(&domain.identity.schema))
        .map(|domain| domain.oid)
        .collect();
    expand_routine_domain_drop(context, &registry, &mut resolution, &mut domains)?;
    let dependents = routine_object_dependents(context, &resolution.targets, true)?;
    commit_sql_function_drop(
        context,
        SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices: resolution.notices,
        },
    )
}

pub fn expand_routine_domain_drop(
    context: &RoutineRemovalContext<'_>,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    resolution: &mut RoutineDropResolution,
    domains: &mut BTreeSet<u32>,
) -> Result<(), SQLError> {
    expand_routine_domain_column_drop(context, registry, resolution, domains, BTreeSet::new())
}

pub fn expand_routine_domain_column_drop(
    context: &RoutineRemovalContext<'_>,
    registry: &BTreeMap<String, Vec<Arc<SQLUserFunction>>>,
    resolution: &mut RoutineDropResolution,
    domains: &mut BTreeSet<u32>,
    mut columns: BTreeSet<(String, String)>,
) -> Result<(), SQLError> {
    let mut relations = BTreeSet::new();
    loop {
        let previous = (
            resolution.targets.len(),
            domains.len(),
            relations.len(),
            columns.len(),
        );
        expand_stored_routine_drop_dependents(context, registry, true, resolution)?;
        let bindings = resolution
            .targets
            .iter()
            .map(RoutineDropTarget::binding)
            .collect::<Vec<_>>();
        domain_dependencies::expand_domain_drop_targets(&context.domains, domains, &bindings)?;
        let dependents = routine_object_dependents(context, &resolution.targets, true)?;
        columns.extend(domain_dependencies::domain_drop_column_names(
            &context.domains,
            domains,
        )?);
        columns.extend(
            dependents
                .columns
                .into_iter()
                .map(|(table, column, _)| (table, column)),
        );
        relations.extend(dependents.views);
        relations.extend(domain_dependencies::domain_drop_view_names(
            &context.domains,
            domains,
        )?);
        expand_column_drop_dependencies(context, &mut columns, &mut relations)?;
        relations = relation_drop_closure(context, relations)?;
        columns.extend(sequence_drop_column_names(context, &relations)?);
        for (name, overloads) in registry {
            for function in overloads {
                if uqa_sql::schema::domains::dependencies::routine_references_domain(
                    context.domains.types,
                    &function.def,
                    domains,
                )? || analysis_relations::stored_routine_references_relations(
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
        if previous
            == (
                resolution.targets.len(),
                domains.len(),
                relations.len(),
                columns.len(),
            )
        {
            break;
        }
    }
    Ok(())
}
