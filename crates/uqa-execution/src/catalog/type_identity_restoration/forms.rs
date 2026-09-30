//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Every stored form that records type names: table defaults, checks, generation expressions and partition keys and bounds, view queries, index keys and predicates, foreign-table columns, domain defaults and checks, routine declarations, defaults and bodies, trigger conditions, and rule conditions, actions, plans and dependencies. Each record is written back only when it changed, with the encoding its reader expects.

use serde_json::Value as Json;
use uqa_sql::ast::{
    ColumnDef, CreateDomain, Expr, IndexKey, PartitionBound, PartitionRangeDatum, PartitionSpec,
    Statement, TableCheck, TableConstraintSet,
};
use uqa_sql::binding::stored_types::{
    upgrade_expression_plan_type_names, upgrade_function_binding_type_names,
    upgrade_query_plan_type_names, upgrade_stored_expression_type_names,
    upgrade_stored_statement_type_names, upgrade_type_name, TypeIdentityResolver, TypeNameSite,
};
use uqa_sql::catalog::events::persistence::{StoredRuleCatalog, StoredTriggerCatalog};
use uqa_sql::catalog::stored_view::restoration::RestoredView;
use uqa_sql::SQLError;
use uqa_storage::CatalogFacade;

use super::{UserTypeNames, DEFAULT_SCHEMA, DOMAINS_METADATA_KEY, DOMAIN_RECORD_PREFIX};

const FUNCTIONS_METADATA_KEY: &str = "sql_functions_json";

pub(super) fn upgrade(catalog: &dyn CatalogFacade, names: &UserTypeNames) -> Result<(), SQLError> {
    let default_path = [DEFAULT_SCHEMA.to_string()];
    let mut resolve =
        |name: &str, site: TypeNameSite| Ok(names.identity(name, site, &default_path));
    tables(catalog, &mut resolve)?;
    views(catalog, &mut resolve)?;
    indexes(catalog, &mut resolve)?;
    foreign_tables(catalog, &mut resolve)?;
    domains(catalog, &mut resolve)?;
    triggers(catalog, &mut resolve)?;
    rules(catalog, &mut resolve)?;
    routines(catalog, names)
}

fn storage(error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("stored type identity upgrade: {error}"))
}

fn tables(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    for mut table in catalog.load_tables().map_err(storage)? {
        let mut changed = false;
        if !table.columns_json.is_empty() {
            let mut columns: Vec<ColumnDef> =
                serde_json::from_str(&table.columns_json).map_err(storage)?;
            if columns_type_names(&mut columns, resolve)? {
                table.columns_json = serde_json::to_string(&columns).map_err(storage)?;
                changed = true;
            }
        }
        if !table.constraints_json.is_empty() {
            let mut constraints: TableConstraintSet =
                serde_json::from_str(&table.constraints_json).map_err(storage)?;
            if constraints_type_names(&mut constraints, resolve)? {
                table.constraints_json = serde_json::to_string(&constraints).map_err(storage)?;
                changed = true;
            }
        }
        if changed {
            catalog.save_table(&table).map_err(storage)?;
        }
    }
    Ok(())
}

fn columns_type_names(
    columns: &mut [ColumnDef],
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let mut changed = false;
    for column in columns {
        for expression in column.default.iter_mut().chain(column.check.iter_mut()) {
            changed |= upgrade_stored_expression_type_names(expression, resolve)?;
        }
        if let Some(generated) = &mut column.generated {
            changed |= upgrade_stored_expression_type_names(&mut generated.expression, resolve)?;
            for binding in &mut generated.function_dependencies {
                changed |= upgrade_function_binding_type_names(binding, resolve)?;
            }
        }
    }
    Ok(changed)
}

fn checks_type_names(
    checks: &mut [TableCheck],
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let mut changed = false;
    for check in checks {
        changed |= upgrade_stored_expression_type_names(&mut check.expr, resolve)?;
        if let Some(partition) = &mut check.partition_constraint {
            changed |= partition_spec_type_names(&mut partition.spec, resolve)?;
            changed |= partition_bound_type_names(&mut partition.bound, resolve)?;
        }
    }
    Ok(changed)
}

fn constraints_type_names(
    constraints: &mut TableConstraintSet,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let mut changed = checks_type_names(&mut constraints.checks, resolve)?;
    if let Some(spec) = &mut constraints.hierarchy.partition_spec {
        changed |= partition_spec_type_names(spec, resolve)?;
    }
    if let Some(bound) = &mut constraints.hierarchy.partition_bound {
        changed |= partition_bound_type_names(bound, resolve)?;
    }
    Ok(changed)
}

fn partition_spec_type_names(
    spec: &mut PartitionSpec,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    expressions_type_names(spec.keys.iter_mut(), resolve)
}

fn partition_bound_type_names(
    bound: &mut PartitionBound,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    match bound {
        PartitionBound::List(values) => expressions_type_names(values.iter_mut(), resolve),
        PartitionBound::Range { lower, upper } => expressions_type_names(
            lower
                .iter_mut()
                .chain(upper.iter_mut())
                .filter_map(|datum| match datum {
                    PartitionRangeDatum::Value(expression) => Some(expression),
                    PartitionRangeDatum::MinValue | PartitionRangeDatum::MaxValue => None,
                }),
            resolve,
        ),
        PartitionBound::Default | PartitionBound::Hash { .. } => Ok(false),
    }
}

fn expressions_type_names<'a>(
    expressions: impl Iterator<Item = &'a mut Expr>,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    let mut changed = false;
    for expression in expressions {
        changed |= upgrade_stored_expression_type_names(expression, resolve)?;
    }
    Ok(changed)
}

/// A view keeps its stored layout: current rows their definition, rows of the earliest layout a bare query plan, which later restoration converts.
fn views(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    for mut row in catalog.load_views().map_err(storage)? {
        let restored: RestoredView = serde_json::from_str(&row.definition_json).map_err(storage)?;
        let encoded = match restored {
            RestoredView::Current(mut definition) => {
                upgrade_query_plan_type_names(&mut definition.query, resolve)?
                    .then(|| serde_json::to_string(&definition))
            }
            RestoredView::Legacy(mut query) => upgrade_query_plan_type_names(&mut query, resolve)?
                .then(|| serde_json::to_string(&query)),
        };
        if let Some(encoded) = encoded {
            row.definition_json = encoded.map_err(storage)?;
            catalog.save_view(&row).map_err(storage)?;
        }
    }
    Ok(())
}

fn indexes(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    for mut row in catalog.load_catalog_indexes().map_err(storage)? {
        let mut changed = false;
        let mut keys: Vec<IndexKey> = serde_json::from_str(&row.columns_json).map_err(storage)?;
        let key_changed = expressions_type_names(
            keys.iter_mut().filter_map(|key| match key {
                IndexKey::Expression(expression) => Some(expression.as_mut()),
                IndexKey::Column(_) => None,
            }),
            resolve,
        )?;
        if key_changed {
            row.columns_json = serde_json::to_string(&keys).map_err(storage)?;
            changed = true;
        }
        if let Some(json) = &row.definition_json {
            let mut definition: uqa_sql::catalog::index::IndexDefinition =
                serde_json::from_str(json).map_err(storage)?;
            if let Some(predicate) = &mut definition.predicate {
                if upgrade_stored_expression_type_names(predicate, resolve)? {
                    row.definition_json =
                        Some(serde_json::to_string(&definition).map_err(storage)?);
                    changed = true;
                }
            }
        }
        if changed {
            catalog.save_catalog_index_row(&row).map_err(storage)?;
        }
    }
    Ok(())
}

fn foreign_tables(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    for mut row in catalog.load_foreign_tables().map_err(storage)? {
        let options = serde_json::from_str(&row.options_json).map_err(storage)?;
        // A legacy layout is written back in the current one, whose missing identity a later restoration step allocates.
        let (mut table, _) = crate::catalog::foreign::StoredForeignTable::from_catalog(
            row.relation.qualified_name(),
            row.server_name.clone(),
            options,
            &row.columns_json,
        )
        .map_err(storage)?;
        let mut changed = columns_type_names(&mut table.columns, resolve)?;
        changed |= checks_type_names(&mut table.checks, resolve)?;
        if changed {
            row.columns_json = table.schema_json().map_err(storage)?;
            catalog.save_foreign_table(&row).map_err(storage)?;
        }
    }
    Ok(())
}

fn domain_type_names(
    definition: &mut CreateDomain,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<bool, SQLError> {
    expressions_type_names(
        definition.default.iter_mut().chain(
            definition
                .checks
                .iter_mut()
                .map(|check| &mut check.expression),
        ),
        resolve,
    )
}

/// Per-domain records, and domains still kept in the domain catalog itself in an earlier format, whose owners a later restoration step converts; there only the definition is decoded.
fn domains(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    for (key, json) in catalog
        .metadata_with_prefix(DOMAIN_RECORD_PREFIX)
        .map_err(storage)?
    {
        let mut domain: uqa_sql::catalog::domain::StoredDomain =
            serde_json::from_str(&json).map_err(storage)?;
        if domain_type_names(&mut domain.definition, resolve)? {
            catalog
                .set_metadata(&key, &serde_json::to_string(&domain).map_err(storage)?)
                .map_err(storage)?;
        }
    }
    let Some(json) = catalog
        .get_metadata(DOMAINS_METADATA_KEY)
        .map_err(storage)?
    else {
        return Ok(());
    };
    let mut value: Json = serde_json::from_str(&json).map_err(storage)?;
    let embedded = match value.get("domain_catalog_format") {
        Some(_) => value.get_mut("domains"),
        None => Some(&mut value),
    };
    let Some(Json::Object(domains)) = embedded else {
        return Ok(());
    };
    let mut changed = false;
    for definition in domains
        .values_mut()
        .filter_map(|domain| domain.get_mut("definition"))
    {
        changed |= typed_json(definition, |definition: &mut CreateDomain| {
            domain_type_names(definition, resolve)
        })?;
    }
    if changed {
        catalog
            .set_metadata(
                DOMAINS_METADATA_KEY,
                &serde_json::to_string(&value).map_err(storage)?,
            )
            .map_err(storage)?;
    }
    Ok(())
}

fn triggers(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    let key = crate::schema::events::persistence::TRIGGERS_METADATA_KEY;
    let Some(json) = catalog.get_metadata(key).map_err(storage)? else {
        return Ok(());
    };
    let mut stored: StoredTriggerCatalog = serde_json::from_str(&json).map_err(storage)?;
    let changed = expressions_type_names(
        stored
            .triggers
            .iter_mut()
            .filter_map(|trigger| trigger.definition.when.as_mut()),
        resolve,
    )?;
    if changed {
        catalog
            .set_metadata(key, &serde_json::to_string(&stored).map_err(storage)?)
            .map_err(storage)?;
    }
    Ok(())
}

/// A rule's condition, actions, condition plan and routine dependencies, and then its SQL text, which renders the upgraded identities.
fn rules(
    catalog: &dyn CatalogFacade,
    resolve: &mut TypeIdentityResolver<'_>,
) -> Result<(), SQLError> {
    let key = crate::schema::events::persistence::RULES_METADATA_KEY;
    let Some(json) = catalog.get_metadata(key).map_err(storage)? else {
        return Ok(());
    };
    let mut stored: StoredRuleCatalog = serde_json::from_str(&json).map_err(storage)?;
    let mut changed = false;
    for rule in &mut stored.rules {
        let mut rule_changed =
            expressions_type_names(rule.definition.condition.iter_mut(), resolve)?;
        for action in &mut rule.definition.actions {
            rule_changed |= upgrade_stored_statement_type_names(action, resolve)?;
        }
        if let Some(plan) = &mut rule.condition_plan {
            rule_changed |= upgrade_expression_plan_type_names(plan, resolve)?;
        }
        if let Some(dependencies) = &mut rule.dependencies {
            let mut routines = std::mem::take(&mut dependencies.routines)
                .into_iter()
                .collect::<Vec<_>>();
            for routine in &mut routines {
                for argument in &mut routine.argument_types {
                    rule_changed |= upgrade_type_name(argument, TypeNameSite::Canonical, resolve)?;
                }
            }
            dependencies.routines = routines.into_iter().collect();
        }
        if rule_changed {
            uqa_sql::catalog::events::synchronize_rule_sql_text(&mut rule.definition)?;
            changed = true;
        }
    }
    if changed {
        catalog
            .set_metadata(key, &serde_json::to_string(&stored).map_err(storage)?)
            .map_err(storage)?;
    }
    Ok(())
}

/// Routine declarations, parameter defaults and SQL-standard bodies in every routine catalog format. The earliest formats keep role names where the current format keeps role identities, so each definition's typed parts are decoded on their own; unqualified names mean the search path the routine was created under.
fn routines(catalog: &dyn CatalogFacade, names: &UserTypeNames) -> Result<(), SQLError> {
    let Some(json) = catalog
        .get_metadata(FUNCTIONS_METADATA_KEY)
        .map_err(storage)?
    else {
        return Ok(());
    };
    let mut value: Json = serde_json::from_str(&json).map_err(storage)?;
    let definitions = if value.get("routine_catalog_format").is_some() {
        value.get_mut("definitions")
    } else {
        Some(&mut value)
    };
    let Some(Json::Object(definitions)) = definitions else {
        return Ok(());
    };
    let mut changed = false;
    for definition in definitions
        .values_mut()
        .filter_map(Json::as_array_mut)
        .flatten()
    {
        changed |= routine_type_names(definition, names)?;
    }
    if changed {
        catalog
            .set_metadata(
                FUNCTIONS_METADATA_KEY,
                &serde_json::to_string(&value).map_err(storage)?,
            )
            .map_err(storage)?;
    }
    Ok(())
}

fn routine_type_names(definition: &mut Json, names: &UserTypeNames) -> Result<bool, SQLError> {
    let search_path = match definition.get("creation_search_path") {
        Some(Json::Array(schemas)) if !schemas.is_empty() => schemas
            .iter()
            .filter_map(|schema| schema.as_str().map(str::to_string))
            .collect(),
        _ => vec![DEFAULT_SCHEMA.to_string()],
    };
    let mut resolve = |name: &str, site: TypeNameSite| Ok(names.identity(name, site, &search_path));
    let mut changed = false;
    if let Some(Json::Array(parameters)) = definition.get_mut("params") {
        for parameter in parameters {
            if let Some(Json::String(type_name)) = parameter.get_mut("type_name") {
                changed |= upgrade_type_name(type_name, TypeNameSite::Written, &mut resolve)?;
            }
            if let Some(default) = parameter
                .get_mut("default")
                .filter(|default| !default.is_null())
            {
                changed |= typed_json(default, |default: &mut Expr| {
                    upgrade_stored_expression_type_names(default, &mut resolve)
                })?;
            }
        }
    }
    if let Some(returns) = definition.get_mut("returns") {
        for form in ["Scalar", "SetOf"] {
            if let Some(Json::String(type_name)) = returns
                .get_mut(form)
                .and_then(|declared| declared.get_mut("type_name"))
            {
                changed |= upgrade_type_name(type_name, TypeNameSite::Written, &mut resolve)?;
            }
        }
    }
    if let Some(statements) = definition
        .get_mut("body")
        .and_then(|body| body.get_mut("Statements"))
    {
        changed |= typed_json(statements, |statements: &mut Vec<Statement>| {
            let mut changed = false;
            for statement in statements {
                changed |= upgrade_stored_statement_type_names(statement, &mut resolve)?;
            }
            Ok(changed)
        })?;
    }
    Ok(changed)
}

/// Decode one part of a stored document as `T`, upgrade it, and write it back when it changed.
fn typed_json<T: serde::de::DeserializeOwned + serde::Serialize>(
    value: &mut Json,
    upgrade: impl FnOnce(&mut T) -> Result<bool, SQLError>,
) -> Result<bool, SQLError> {
    let mut typed: T = serde_json::from_value(value.clone()).map_err(storage)?;
    if !upgrade(&mut typed)? {
        return Ok(false);
    }
    *value = serde_json::to_value(&typed).map_err(storage)?;
    Ok(true)
}
