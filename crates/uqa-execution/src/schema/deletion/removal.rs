//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `doDeletion`: remove each object of a plan without checking what depends on it, since its dependents were removed before it.

use super::plan::DeletionPlan;
use super::CatalogRemovalContext;
use crate::catalog::projection::{CatalogObject, RelationKind};
use std::collections::BTreeSet;
use uqa_core::RelationIdentity;
use uqa_sql::ast::{DropRule, DropTrigger, FunctionBinding};
use uqa_sql::routines::lifecycle::RoutineDropTarget;
use uqa_sql::SQLError;
use uqa_storage::StorageBackendError;

impl DeletionPlan {
    /// Remove each object in order, as `deleteObjectsInList` does, so nothing that remains names an object already removed. Surviving routine bodies that name removed columns in source aliases are rewritten, and stored `MERGE` plans are refreshed.
    pub(super) fn execute(self, context: &CatalogRemovalContext<'_>) -> Result<(), SQLError> {
        // `heap_drop_with_catalog`'s `CheckTableNotInUse`: removing a table's constraints first must not discard the pending trigger events that forbid dropping it.
        for object in &self.steps {
            if let CatalogObject::Relation {
                identity,
                kind: RelationKind::Table,
                ..
            } = object
            {
                context
                    .events
                    .ensure_no_pending_trigger_events(&identity.qualified_name(), "DROP TABLE")?;
            }
        }
        let routines = routine_targets(context, &self.steps)?;
        let mut rewritten = Some(routine_alias_rewrites(context, &self.steps, &routines)?);
        // Surviving routines stop naming removed columns as soon as the last column is gone, before a later step, such as removing a schema, reloads the catalog and validates them.
        let last_column = self
            .steps
            .iter()
            .rposition(|object| matches!(object, CatalogObject::Column { .. }));
        let mut tables_removed = false;
        let mut routines = routines.into_iter();
        for (position, object) in self.steps.iter().enumerate() {
            match object {
                CatalogObject::Relation { identity, kind, .. } => {
                    tables_removed |= *kind == RelationKind::Table;
                    remove_relation(context, identity, *kind)?;
                }
                CatalogObject::Column {
                    relation,
                    kind,
                    name,
                } => remove_column(context, relation, *kind, name)?,
                CatalogObject::Type(oid) => {
                    crate::schema::domains::dependencies::remove_types(
                        &context.domains,
                        &BTreeSet::from([*oid]),
                    )?;
                }
                CatalogObject::RelationConstraint {
                    relation,
                    kind,
                    name,
                    ..
                } => remove_relation_constraint(context, relation, *kind, name)?,
                CatalogObject::DomainConstraint { domain, name, .. } => {
                    crate::schema::domains::dependencies::remove_domain_constraint(
                        &context.domains,
                        *domain,
                        name,
                    )?;
                }
                CatalogObject::ColumnDefault {
                    relation,
                    kind,
                    column,
                } => remove_column_default(context, relation, *kind, column)?,
                CatalogObject::Routine { .. } => {
                    let routine = routines.next().ok_or_else(|| {
                        SQLError::Internal("deletion plan lost a routine target".into())
                    })?;
                    crate::routines::removal::commit_routine_registry_drop(
                        &context.tables.routines,
                        std::slice::from_ref(&routine),
                    )?;
                }
                CatalogObject::Rule { relation, name, .. } => {
                    context.tables.events.drop_rule(&DropRule {
                        name: name.clone(),
                        table: relation.qualified_name(),
                        if_exists: false,
                        cascade: false,
                    })?;
                }
                CatalogObject::Trigger { relation, name, .. } => {
                    context.tables.events.drop_trigger(&DropTrigger {
                        name: name.clone(),
                        table: relation.qualified_name(),
                        if_exists: false,
                        cascade: false,
                    })?;
                }
                CatalogObject::Schema(name) => remove_schema(context, name)?,
                CatalogObject::ArrayType { .. } | CatalogObject::RowType { .. } => {}
            }
            if Some(position) == last_column {
                publish_routine_rewrites(context, rewritten.take())?;
            }
        }
        if tables_removed {
            context.tables.publication.prune_constraint_modes()?;
        }
        publish_routine_rewrites(context, rewritten.take())
    }
}

/// Publish the rewritten bodies of surviving routines once, and refresh the stored `MERGE` plans that name removed targets.
fn publish_routine_rewrites(
    context: &CatalogRemovalContext<'_>,
    rewritten: Option<Vec<uqa_sql::ast::CreateFunction>>,
) -> Result<(), SQLError> {
    let Some(rewritten) = rewritten else {
        return Ok(());
    };
    crate::routines::rewrites::publish_stored_routine_body_rewrites(
        &context.tables.routines.bodies,
        rewritten,
    )?;
    crate::routines::rewrites::refresh_stored_merge_target_plans(&context.tables.routines.bodies)
}

fn remove_relation(
    context: &CatalogRemovalContext<'_>,
    identity: &RelationIdentity,
    kind: RelationKind,
) -> Result<(), SQLError> {
    let name = identity.qualified_name();
    match kind {
        RelationKind::Table => context
            .tables
            .drop_table_state_inner(&name)
            .map_err(|error| storage_error("DROP TABLE", &error)),
        RelationKind::View | RelationKind::MaterializedView => {
            crate::schema::view_removal::drop_view_state_inner(&context.tables.views, &name)
        }
        RelationKind::ForeignTable => {
            if context
                .foreign_tables
                .drop_foreign_table_inner(&name)
                .map_err(SQLError::Internal)?
            {
                Ok(())
            } else {
                Err(disappeared("foreign table", &name))
            }
        }
        RelationKind::Sequence => {
            context
                .tables
                .sequences
                .dependencies
                .detach_sequence_provenance(&name)
                .map_err(|error| storage_error("DROP SEQUENCE", &error))?;
            if context
                .tables
                .sequences
                .publication
                .remove_state(&name)
                .map_err(SQLError::Internal)?
            {
                Ok(())
            } else {
                Err(disappeared("sequence", &name))
            }
        }
        RelationKind::Index => {
            crate::schema::indexes::removal::drop_index_dependency(&context.indexes, identity)
        }
    }
}

fn remove_column(
    context: &CatalogRemovalContext<'_>,
    relation: &RelationIdentity,
    kind: RelationKind,
    column: &str,
) -> Result<(), SQLError> {
    let table = relation.qualified_name();
    let removed = if kind == RelationKind::ForeignTable {
        context
            .tables
            .sequences
            .dependencies
            .foreign
            .drop_foreign_table_column_dependency(&table, column)
            .map_err(|error| storage_error("ALTER FOREIGN TABLE DROP COLUMN", &error))?
            == Some(true)
    } else {
        crate::schema::publication::removal::drop_column(
            &context.tables.sequences.dependencies.columns,
            &table,
            column,
        )
        .map_err(|error| storage_error("ALTER TABLE DROP COLUMN", &error))?
    };
    if removed {
        Ok(())
    } else {
        Err(disappeared("column", &format!("{table}.{column}")))
    }
}

fn remove_relation_constraint(
    context: &CatalogRemovalContext<'_>,
    relation: &RelationIdentity,
    kind: RelationKind,
    name: &str,
) -> Result<(), SQLError> {
    let table = relation.qualified_name();
    if kind == RelationKind::ForeignTable {
        return match context
            .tables
            .sequences
            .dependencies
            .foreign
            .drop_foreign_table_check_dependency(&table, name)
            .map_err(|error| storage_error("ALTER FOREIGN TABLE DROP CONSTRAINT", &error))?
        {
            Some(true) => Ok(()),
            _ => Err(disappeared("constraint", &format!("{name} on {table}"))),
        };
    }
    crate::schema::constraints::drop::drop_constraint_dependency(
        &context.tables.sequences.dependencies.constraints,
        &table,
        name,
    )
}

fn remove_column_default(
    context: &CatalogRemovalContext<'_>,
    relation: &RelationIdentity,
    kind: RelationKind,
    column: &str,
) -> Result<(), SQLError> {
    let table = relation.qualified_name();
    let removed = if kind == RelationKind::ForeignTable {
        context
            .tables
            .sequences
            .dependencies
            .foreign
            .clear_foreign_table_default_dependency(&table, column)
            .map_err(|error| storage_error("ALTER FOREIGN TABLE DROP DEFAULT", &error))?
            == Some(true)
    } else {
        crate::schema::publication::columns::set_column_default(
            &context.tables.sequences.dependencies.schema,
            &table,
            column,
            None,
        )
        .map_err(|error| storage_error("ALTER TABLE DROP DEFAULT", &error))?
    };
    if removed {
        Ok(())
    } else {
        Err(disappeared("default", &format!("{table}.{column}")))
    }
}

fn remove_schema(context: &CatalogRemovalContext<'_>, name: &str) -> Result<(), SQLError> {
    if crate::schema::namespaces::removal::drop_empty_schema(&context.schemas, name)
        .map_err(|error| storage_error("DROP SCHEMA", &error))?
    {
        Ok(())
    } else {
        Err(disappeared("schema", name))
    }
}

/// The routines of the plan as the routine registry names them.
fn routine_targets(
    context: &CatalogRemovalContext<'_>,
    steps: &[CatalogObject],
) -> Result<Vec<RoutineDropTarget>, SQLError> {
    let registry = context.tables.routines.registry.routine_snapshot();
    steps
        .iter()
        .filter_map(|object| match object {
            CatalogObject::Routine { object_id } => Some(object_id),
            _ => None,
        })
        .map(|object_id| {
            registry
                .iter()
                .flat_map(|(name, overloads)| {
                    overloads.iter().map(move |function| (name, function))
                })
                .find(|(_, function)| function.def.object_id == Some(*object_id))
                .map(|(name, function)| RoutineDropTarget {
                    object_id: function.def.object_id,
                    name: name.clone(),
                    argument_types: uqa_sql::routines::routine_signature_types(&function.def),
                    is_procedure: function.def.is_procedure,
                })
                .ok_or_else(|| SQLError::Internal("routine disappeared before DROP".into()))
        })
        .collect()
}

/// Surviving routine bodies that name the removed columns in source column aliases, rewritten without them.
fn routine_alias_rewrites(
    context: &CatalogRemovalContext<'_>,
    steps: &[CatalogObject],
    routines: &[RoutineDropTarget],
) -> Result<Vec<uqa_sql::ast::CreateFunction>, SQLError> {
    let columns = steps
        .iter()
        .filter_map(|object| match object {
            CatalogObject::Column { relation, name, .. } => {
                Some((relation.qualified_name(), name.clone()))
            }
            _ => None,
        })
        .collect::<BTreeSet<_>>();
    if columns.is_empty() {
        return Ok(Vec::new());
    }
    let removed = routines
        .iter()
        .map(RoutineDropTarget::binding)
        .collect::<Vec<FunctionBinding>>();
    let dependencies =
        uqa_sql::routines::lifecycle::rewrites::routine_column_drop_dependencies(columns)?;
    let bodies = &context.tables.routines.bodies;
    let registry = bodies.registry.routine_snapshot();
    uqa_sql::routines::lifecycle::rewrites::routine_column_alias_drop_candidates(
        bodies.columns,
        &registry,
        &dependencies,
        &removed,
    )
}

fn storage_error(action: &str, error: &StorageBackendError) -> SQLError {
    uqa_sql::catalog::errors::storage_error(action, error)
}

fn disappeared(kind: &str, name: &str) -> SQLError {
    SQLError::Internal(format!("{kind} {name} disappeared before its removal"))
}
