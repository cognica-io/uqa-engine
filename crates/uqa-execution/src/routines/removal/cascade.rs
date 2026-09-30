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
    // `performMultipleDeletions`: the dependents a RESTRICT drop refuses and a cascading drop reports.
    let cascade_notice = report_type_drop(context, targets, cascade)?;
    let mut domains = targets.clone();
    expand_routine_domain_drop(context, &registry, &mut resolution, &mut domains)?;
    if !cascade
        && (domains != *targets
            || !resolution.targets.is_empty()
            || domain_dependencies::domain_drop_has_dependents(&context.domains, targets)?)
    {
        return Err(SQLError::Internal(
            "DROP TYPE found dependents that the catalog dependencies do not record".into(),
        ));
    }
    let mut notices = resolution.notices;
    if let Some(notice) = cascade_notice {
        notices.insert(0, ("NOTICE", notice.message));
    }
    let dependents = routine_object_dependents(context, &resolution.targets, true)?;
    commit_sql_function_drop(
        context,
        SQLFunctionDropPlan {
            domains,
            targets: resolution.targets,
            dependents,
            notices,
        },
    )
}

/// Search the catalog's dependencies from the named types as `findDependentObjects` does, and report them as `reportDependentObjects` does: a RESTRICT drop with dependents fails listing them, and a cascading drop returns its notice.
fn report_type_drop(
    context: &RoutineRemovalContext<'_>,
    targets: &BTreeSet<u32>,
    cascade: bool,
) -> Result<Option<uqa_sql::catalog::dependencies::CascadeNotice>, SQLError> {
    use uqa_sql::catalog::dependencies::{DeletionTargets, ObjectAddress, TYPE_CLASS};
    let catalog = context.catalog.catalog_read_view();
    let mut resolution = context
        .catalog
        .session_execution_view()
        .relation_name_resolution();
    resolution.set_lookup_mode(crate::catalog::RelationLookupMode::Bound);
    let dependencies = crate::catalog::projection::CatalogDependencies::build(
        &context.catalog,
        &catalog,
        &resolution,
    )?;
    let originals = targets
        .iter()
        .map(|oid| ObjectAddress::whole(TYPE_CLASS, *oid))
        .collect::<Vec<_>>();
    let describe = |object| dependencies.describe(&context.catalog, object);
    let deletion = DeletionTargets::collect(dependencies.graph(), &originals, &describe)?;
    let original = match originals.as_slice() {
        [original] => Some(*original),
        _ => None,
    };
    deletion.report(cascade, original, &describe)
}

pub fn drop_schema_types_and_routines(
    context: &RoutineRemovalContext<'_>,
    schemas: &BTreeSet<String>,
) -> Result<(), SQLError> {
    let registry = context.registry.routine_snapshot();
    let mut resolution = analysis_relations::schema_routine_drop_targets(&registry, schemas)?;
    let mut domains: BTreeSet<u32> = context
        .domains
        .catalog
        .domain_definitions()
        .values()
        .filter(|domain| schemas.contains(&domain.identity.schema))
        .map(|domain| domain.oid)
        .collect();
    // Enum types of the schemas go with them, as DROP SCHEMA ... CASCADE drops every type in the schema.
    domains.extend(
        context
            .catalog
            .catalog_read_view()
            .enums()
            .filter(|definition| schemas.contains(&definition.identity.schema))
            .map(|definition| definition.oid),
    );
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
