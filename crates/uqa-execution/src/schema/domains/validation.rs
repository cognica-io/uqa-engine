//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Validate only the selected domain constraint, borrowing projected values after retaining all dependent relation locks.

use crate::{
    catalog::{context::CatalogContext, CatalogReadView, RelationLookupMode},
    query::{
        local_table::{LocalTableRowSource, LocalTableScanConfig},
        relational::context::QueryExpressionFactory,
        table_read::QueryTableAccess,
    },
    row_locks::{
        binding::{lock_any_relation_identity, RelationLockCatalog, RelationLockSession},
        RelationLockMode,
    },
    RowSchema, RowSource,
};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{CancellationToken, RelationIdentity, Value};
use uqa_sql::{
    ast::{ColumnType, DomainCheck},
    catalog::domain::StoredDomain,
    expr::composites::CompositeTypeCatalog,
    schema::domains::{
        dependencies::{column_domain_dependency, DomainColumnDependency},
        removal::TypeObjectCatalog,
    },
    SQLError, ScalarExpr,
};

/// Retain the latest locked relation independently of an ordinary query's fixed isolation snapshot. Writes performed by a CHECK function must not change the rows in this validation scan.
pub trait DomainValidationTables: QueryTableAccess {
    fn latest_table_snapshot(
        &self,
        name: &str,
    ) -> Result<Arc<dyn crate::query::table_read::TableRead>, SQLError>;
}

pub struct DomainValidationInputs<'a, S: Clone + 'static> {
    pub catalog: CatalogContext<'a>,
    pub tables: &'a dyn DomainValidationTables,
    pub locks: &'a dyn RelationLockSession,
    pub identities: &'a dyn RelationLockCatalog,
    pub composites: &'a dyn CompositeTypeCatalog,
    pub expressions: &'a dyn QueryExpressionFactory<S>,
    pub cancellation: CancellationToken,
    pub plan_check: fn(&mut ScalarExpr) -> Result<(), SQLError>,
}

pub(super) struct DomainValueValidationContext<'a, 'b, S: Clone + 'static> {
    pub inputs: &'a DomainValidationInputs<'b, S>,
    pub types: &'b dyn TypeObjectCatalog,
}

struct ValidationRelation {
    identity: RelationIdentity,
    object_id: [u8; 16],
    columns: Vec<(String, ColumnType)>,
}

pub(super) fn validate_values<S: Clone + 'static>(
    context: &DomainValueValidationContext<'_, '_, S>,
    domain: &StoredDomain,
    constraint: Option<&DomainCheck>,
) -> Result<(), SQLError> {
    let check = constraint
        .map(|constraint| check::PreparedCheck::new(context.inputs, domain, constraint))
        .transpose()?;
    let (catalog, relations) = locked_relations(context, domain)?;
    for relation in relations {
        if let Some(view) = catalog.snapshot().definitions.views.get(&relation.identity) {
            for row in &view.materialized_rows {
                context.inputs.cancellation.check()?;
                for (column, _) in &relation.columns {
                    check_value(&relation, column, row.get(column), check.as_ref())?;
                }
            }
        } else {
            validate_table(context.inputs, &relation, check.as_ref())?;
        }
    }
    Ok(())
}

fn check_value(
    relation: &ValidationRelation,
    column: &str,
    value: Option<&Value>,
    check: Option<&check::PreparedCheck<'_>>,
) -> Result<(), SQLError> {
    let value = value.unwrap_or(&Value::Null);
    let failed = match check {
        Some(check) => check.violates(value)?,
        None => matches!(value, Value::Null),
    };
    if failed {
        Err(violation(&relation.identity.name, column, check.is_some()))
    } else {
        Ok(())
    }
}

fn validate_table<S: Clone + 'static>(
    inputs: &DomainValidationInputs<'_, S>,
    relation: &ValidationRelation,
    check: Option<&check::PreparedCheck<'_>>,
) -> Result<(), SQLError> {
    let name = relation.identity.qualified_name();
    let table = inputs.tables.latest_table_snapshot(&name)?;
    let columns = relation
        .columns
        .iter()
        .map(|(name, _)| name.clone())
        .collect::<Vec<_>>();
    let schema = RowSchema::with_types(
        columns.clone(),
        relation
            .columns
            .iter()
            .map(|(_, ty)| Some(ty.clone()))
            .collect(),
    );
    let mut source = LocalTableRowSource::new(LocalTableScanConfig {
        cancellation: inputs.cancellation.clone(),
        serializable: inputs.tables.serializable_read(&name)?,
        column_definitions: Arc::new(table.column_definitions()),
        table,
        columns: columns.clone(),
        schema: columns,
        physical_schema: schema,
        metadata: uqa_sql::plan::source_projection::RelationMetadataProjection::default(),
        table_oid: None,
        predicate: None,
        estimated_cardinality: inputs.tables.table_row_estimate(&name)?,
        lock_origin: None,
        recheck_pins: None,
        candidates: None,
        command_changes: inputs.tables.command_overlay_changes(&name)?,
        table_name: name,
    });
    loop {
        // One projected row is retained; validation memory does not grow with the table.
        let rows = source
            .next_physical_batch(1)
            .map_err(crate::physical::physical_exec_error)?;
        if rows.is_empty() {
            break;
        }
        for row in rows {
            for (index, (column, _)) in relation.columns.iter().enumerate() {
                check_value(relation, column, row.value(index), check)?;
            }
        }
    }
    Ok(())
}

fn violation(table: &str, column: &str, check: bool) -> SQLError {
    SQLError::Routine {
        sqlstate: if check { "23514" } else { "23502" }.into(),
        message: format!(
            "column \"{column}\" of table \"{table}\" contains {}",
            if check {
                "values that violate the new constraint"
            } else {
                "null values"
            }
        ),
    }
}

fn locked_relations<S: Clone + 'static>(
    context: &DomainValueValidationContext<'_, '_, S>,
    domain: &StoredDomain,
) -> Result<(CatalogReadView, Vec<ValidationRelation>), SQLError> {
    let mut locked = BTreeSet::new();
    loop {
        context.inputs.cancellation.check()?;
        context
            .inputs
            .catalog
            .catalog
            .refreshed_catalog_snapshot()?;
        let catalog = context.inputs.catalog.catalog.current_catalog_snapshot();
        let relations = dependent_relations(context, &catalog, domain)?;
        let mut acquired = false;
        for relation in &relations {
            if locked.contains(&relation.object_id) {
                continue;
            }
            if lock_any_relation_identity(
                context.inputs.identities,
                context.inputs.locks,
                relation.identity.qualified_name(),
                relation.object_id,
                RelationLockMode::Share,
            )?
            .is_some()
            {
                locked.insert(relation.object_id);
            }
            acquired = true;
        }
        if !acquired {
            return Ok((catalog, relations));
        }
    }
}

fn dependent_relations<S: Clone + 'static>(
    context: &DomainValueValidationContext<'_, '_, S>,
    catalog: &CatalogReadView,
    domain: &StoredDomain,
) -> Result<Vec<ValidationRelation>, SQLError> {
    let mut relations = Vec::new();
    let mut resolution = context.inputs.catalog.session.relation_name_resolution();
    resolution.set_lookup_mode(RelationLookupMode::Bound);
    for identity in catalog.snapshot().tables.keys() {
        let Some(table) = catalog.table(&resolution, &identity.qualified_name())? else {
            continue;
        };
        let columns = affected_columns(
            context,
            domain,
            identity,
            table
                .columns
                .iter()
                .map(|column| (&column.name, &column.ty)),
        )?;
        if !columns.is_empty() {
            relations.push(ValidationRelation {
                identity: identity.clone(),
                object_id: table.object_id,
                columns,
            });
        }
    }
    for (identity, view) in catalog
        .snapshot()
        .definitions
        .views
        .iter()
        .filter(|(_, view)| view.kind == crate::catalog::view::StoredViewKind::Materialized)
    {
        let columns = affected_columns(
            context,
            domain,
            identity,
            view.output_columns
                .iter()
                .flatten()
                .zip(&view.materialized_column_types)
                .filter_map(|(name, ty)| ty.as_ref().map(|ty| (name, ty))),
        )?;
        if !columns.is_empty() {
            relations.push(ValidationRelation {
                identity: identity.clone(),
                object_id: view.object_id,
                columns,
            });
        }
    }
    Ok(relations)
}

fn affected_columns<'a, S: Clone + 'static>(
    context: &DomainValueValidationContext<'_, '_, S>,
    domain: &StoredDomain,
    relation: &RelationIdentity,
    columns: impl Iterator<Item = (&'a String, &'a ColumnType)>,
) -> Result<Vec<(String, ColumnType)>, SQLError> {
    let mut affected = Vec::new();
    for (name, ty) in columns {
        match column_domain_dependency(ty, domain.oid, context.types, context.inputs.composites)? {
            DomainColumnDependency::None => {}
            DomainColumnDependency::Direct => affected.push((name.clone(), ty.clone())),
            DomainColumnDependency::Container => {
                let label = context
                    .types
                    .format_drop_type(i64::from(domain.oid))
                    .map_err(SQLError::Internal)?
                    .unwrap_or_else(|| domain.identity.qualified_name());
                return Err(SQLError::Routine {
                    sqlstate: "0A000".into(),
                    message: format!(
                        "cannot alter type \"{label}\" because column \"{}.{name}\" uses it",
                        relation.name
                    ),
                });
            }
        }
    }
    Ok(affected)
}

mod check;
