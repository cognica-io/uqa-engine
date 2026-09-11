//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve table grant targets and validate retained relation metadata in catalog order.
use super::context::{TableGrantContext, TableGrantState};
use crate::catalog::view::{StoredView, StoredViewKind};
use uqa_sql::{
    ast::GrantTableTarget,
    catalog::security::{
        grants::bind_grant_schemas,
        table::RequestedTablePrivileges,
        table_grants::{
            targets::bind_named_table_grants, validate_requested_columns, ForeignTableGrantTarget,
            ResolvedTableGrantTarget,
        },
    },
    SQLError,
};
type RetainedTableGrantTarget<'a, 'scope> = (
    &'a ResolvedTableGrantTarget,
    Box<dyn TableGrantState + 'scope>,
);

pub(super) fn validated_table_grant_targets<'a, 'scope>(
    context: &'scope TableGrantContext<'_>,
    targets: &'a [ResolvedTableGrantTarget],
    requested: &RequestedTablePrivileges,
) -> Result<Vec<RetainedTableGrantTarget<'a, 'scope>>, SQLError> {
    let tables = context.tables.tables();
    targets
        .iter()
        .filter(|target| target.kind == "table")
        .map(|target| {
            let table = tables.retained(&target.relation).ok_or_else(|| {
                SQLError::Internal(format!("table `{}` disappeared", target.name))
            })?;
            let columns = table.column_names();
            validate_requested_columns(&target.relation, &columns, requested)?;
            Ok((target, table))
        })
        .collect()
}
pub(super) fn validated_view_grant_targets<'a>(
    context: &TableGrantContext<'_>,
    targets: &'a [ResolvedTableGrantTarget],
    requested: &RequestedTablePrivileges,
) -> Result<Vec<(&'a ResolvedTableGrantTarget, StoredView)>, SQLError> {
    let views = context.registry.views();
    let selected = targets
        .iter()
        .filter(|target| matches!(target.kind, "view" | "materialized view"))
        .map(|target| {
            let view = views
                .get(&target.relation)
                .cloned()
                .ok_or_else(|| SQLError::Internal(format!("view `{}` disappeared", target.name)))?;
            Ok((target, view))
        })
        .collect::<Result<Vec<_>, SQLError>>()?;
    drop(views);
    for (target, view) in &selected {
        let columns = view.output_columns.as_deref().ok_or_else(|| {
            SQLError::Internal(format!(
                "loaded view `{}` has no durable public column metadata",
                target.relation.qualified_name()
            ))
        })?;
        validate_requested_columns(&target.relation, columns, requested)?;
    }
    Ok(selected)
}
pub(super) fn validated_foreign_table_grant_targets<'a>(
    context: &TableGrantContext<'_>,
    targets: &'a [ResolvedTableGrantTarget],
    requested: &RequestedTablePrivileges,
) -> Result<Vec<ForeignTableGrantTarget<'a>>, SQLError> {
    let tables = context.registry.foreign_tables();
    let securities = context.registry.foreign_security();
    targets
        .iter()
        .filter(|target| target.kind == "foreign table")
        .map(|target| {
            let table = tables.get(&target.relation).ok_or_else(|| {
                SQLError::Internal(format!("foreign table `{}` disappeared", target.name))
            })?;
            let security = securities.get(&target.relation).cloned().ok_or_else(|| {
                SQLError::Internal(format!(
                    "foreign table `{}` has no loaded security metadata",
                    target.name
                ))
            })?;
            let columns = table
                .columns
                .iter()
                .map(|column| column.name.clone())
                .collect::<Vec<_>>();
            validate_requested_columns(&target.relation, &columns, requested)?;
            Ok((target, security, columns))
        })
        .collect()
}
impl TableGrantContext<'_> {
    pub(super) fn resolve_table_grant_targets(
        &self,
        target: &GrantTableTarget,
    ) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
        match target {
            GrantTableTarget::Relations { names } => {
                bind_named_table_grants(self.resolution, names)
            }
            GrantTableTarget::AllTablesInSchemas { schemas } => {
                self.resolve_all_tables_in_schemas(schemas)
            }
        }
    }
    fn resolve_all_tables_in_schemas(
        &self,
        schemas: &[String],
    ) -> Result<Vec<ResolvedTableGrantTarget>, SQLError> {
        self.registry.refresh_catalog().map_err(|error| {
            SQLError::Internal(format!("load schemas for table privileges: {error}"))
        })?;
        self.registry
            .refresh_tables()
            .map_err(|error| SQLError::Internal(format!("load tables for privileges: {error}")))?;
        let resolved_schemas = bind_grant_schemas(self.namespaces, schemas)?;
        let tables = self.tables.tables();
        let mut targets = tables
            .keys()
            .filter(|relation| resolved_schemas.contains(&relation.schema))
            .map(|relation| ResolvedTableGrantTarget {
                requested: relation.qualified_name(),
                name: relation.qualified_name(),
                relation: relation.clone(),
                kind: "table",
            })
            .collect::<Vec<_>>();
        drop(tables);
        targets.extend(
            self.registry
                .views()
                .iter()
                .filter(|(relation, _)| resolved_schemas.contains(&relation.schema))
                .map(|(relation, view)| ResolvedTableGrantTarget {
                    requested: relation.qualified_name(),
                    name: relation.qualified_name(),
                    relation: relation.clone(),
                    kind: match view.kind {
                        StoredViewKind::View => "view",
                        StoredViewKind::Materialized => "materialized view",
                    },
                }),
        );
        targets.extend(
            self.registry
                .foreign_tables()
                .keys()
                .filter(|relation| resolved_schemas.contains(&relation.schema))
                .map(|relation| ResolvedTableGrantTarget {
                    requested: relation.qualified_name(),
                    name: relation.qualified_name(),
                    relation: relation.clone(),
                    kind: "foreign table",
                }),
        );
        targets.sort_by(|left, right| left.relation.cmp(&right.relation));
        Ok(targets)
    }
}
