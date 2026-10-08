//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain dependent definitions before replacing any attribute descriptor, then reparse and rebuild through the index and constraint owners.

use super::CompositeAlterationContext;
use crate::catalog::projection::{CatalogObject, RelationKind};
use crate::schema::deletion::{catalog_dependencies, perform_deletion};
use std::collections::{BTreeMap, BTreeSet};
use uqa_core::{RelationIdentity, Value};
use uqa_sql::{
    ast::{AlterTableAction, CreateIndex, Statement, TableCheck},
    catalog::{composite_type::StoredCompositeAttribute, dependencies::ObjectAddress},
    SQLError,
};

pub(super) struct Dependents {
    definitions: BTreeMap<ObjectAddress, Definition>,
}

enum Definition {
    Index {
        relation: RelationIdentity,
        statement: CreateIndex,
    },
    Check {
        table: RelationIdentity,
        constraint: TableCheck,
    },
}

impl Dependents {
    pub(super) fn capture(
        context: &CompositeAlterationContext<'_>,
        relation_oid: u32,
        changes: &[StoredCompositeAttribute],
    ) -> Result<Self, SQLError> {
        let removal = context.removal.catalog_removal_context();
        let dependencies = catalog_dependencies(&removal.catalog)?;
        let mut objects = BTreeMap::new();
        let mut tables = BTreeSet::new();
        for change in changes {
            for dependency in dependencies.graph().dependents_of(ObjectAddress::column(
                relation_oid,
                i32::from(change.number),
            )) {
                let address = dependency.dependent;
                match dependencies.catalog_object(address) {
                    Some(
                        object @ CatalogObject::Relation {
                            kind: RelationKind::Index,
                            table: Some(_),
                            ..
                        },
                    ) => {
                        if let CatalogObject::Relation {
                            table: Some(table), ..
                        } = &object
                        {
                            tables.insert(table.clone());
                        }
                        objects.insert(address, object);
                    }
                    Some(object @ CatalogObject::RelationConstraint { .. }) => {
                        if let CatalogObject::RelationConstraint { relation, .. } = &object {
                            tables.insert(relation.clone());
                        }
                        objects.insert(address, object);
                    }
                    _ => {}
                }
            }
        }
        for table in tables {
            crate::row_locks::binding::acquire_relation(
                context.binding.locks,
                &table.qualified_name(),
                crate::row_locks::RelationLockMode::AccessExclusive,
                false,
            )?
            .retain();
        }
        context.binding.locks.refresh_after_wait()?;
        let current = catalog_dependencies(&removal.catalog)?;
        let snapshot = removal.catalog.catalog_read_view();
        let mut definitions = BTreeMap::new();
        for (address, object) in objects {
            if current.catalog_object(address).as_ref() != Some(&object) {
                return Err(SQLError::Internal(
                    "dependent definition changed while acquiring its table lock".into(),
                ));
            }
            let definition = Definition::capture(&removal.catalog, &snapshot, address, object)?;
            definitions.insert(address, definition);
        }
        Ok(Self { definitions })
    }

    pub(super) fn rebuild(self, context: &CompositeAlterationContext<'_>) -> Result<(), SQLError> {
        if self.definitions.is_empty() {
            return Ok(());
        }
        let removal = context.removal.catalog_removal_context();
        perform_deletion(
            &removal,
            |_| Ok(self.definitions.keys().copied().collect()),
            false,
        )?;
        let (checks, indexes): (Vec<_>, Vec<_>) = self
            .definitions
            .into_values()
            .partition(|definition| matches!(definition, Definition::Check { .. }));
        for definition in checks.into_iter().chain(indexes) {
            match definition {
                Definition::Index {
                    relation,
                    statement,
                } => {
                    crate::schema::indexes::creation::rebuild_index(
                        &context.indexes,
                        statement,
                        &relation,
                    )?;
                }
                Definition::Check { table, constraint } => {
                    crate::schema::constraints::add_check_constraint(
                        &removal.indexes.constraints,
                        &table.qualified_name(),
                        &table.name,
                        constraint,
                    )?;
                }
            }
        }
        Ok(())
    }
}

impl Definition {
    fn capture(
        catalog: &crate::catalog::context::CatalogContext<'_>,
        snapshot: &crate::catalog::CatalogReadView,
        address: ObjectAddress,
        object: CatalogObject,
    ) -> Result<Self, SQLError> {
        match object {
            CatalogObject::Relation {
                identity,
                table: Some(table),
                ..
            } => {
                let sql = text(crate::catalog::projection::pg_get_indexdef_value(
                    catalog,
                    &[Value::Int(i64::from(address.object_id))],
                )?)?;
                let Statement::CreateIndex(mut statement) = uqa_sql::compile(&sql)?.remove(0)
                else {
                    return Err(SQLError::Internal(
                        "index definition did not reparse as CREATE INDEX".into(),
                    ));
                };
                let row = snapshot
                    .snapshot()
                    .definitions
                    .catalog_indexes
                    .get(&identity)
                    .ok_or_else(|| SQLError::Internal("dependent index disappeared".into()))?;
                statement.options = uqa_sql::catalog::index::stored::declaration(row)
                    .map_err(|error| {
                        SQLError::Internal(format!("decode dependent index: {error}"))
                    })?
                    .options;
                statement.table = table.qualified_name();
                Ok(Definition::Index {
                    relation: identity,
                    statement,
                })
            }
            CatalogObject::RelationConstraint { relation, name, .. } => {
                let sql = text(crate::catalog::projection::pg_get_constraintdef_value(
                    catalog,
                    &[Value::Int(i64::from(address.object_id))],
                )?)?;
                let sql = format!(
                    "ALTER TABLE {}.{} ADD CONSTRAINT {} {sql}",
                    uqa_sql::expr::quote_ident(&relation.schema),
                    uqa_sql::expr::quote_ident(&relation.name),
                    uqa_sql::expr::quote_ident(&name)
                );
                let Statement::AlterTable(mut statement) = uqa_sql::compile(&sql)?.remove(0) else {
                    return Err(SQLError::Internal(
                        "constraint definition did not reparse as ALTER TABLE".into(),
                    ));
                };
                let AlterTableAction::AddCheckConstraint { mut constraint } =
                    statement.actions.remove(0)
                else {
                    return Err(SQLError::Internal(
                        "composite field dependency is not a CHECK".into(),
                    ));
                };
                let table = snapshot.snapshot().tables.get(&relation).ok_or_else(|| {
                    SQLError::Internal("dependent CHECK table disappeared".into())
                })?;
                constraint.is_local = table
                    .checks
                    .iter()
                    .find(|check| check.name.as_deref() == Some(&name))
                    .map(|check| check.is_local)
                    .or_else(|| {
                        table
                            .columns
                            .iter()
                            .find(|column| column.check_name.as_deref() == Some(&name))
                            .map(|column| column.check_is_local)
                    })
                    .ok_or_else(|| SQLError::Internal("dependent CHECK disappeared".into()))?;
                Ok(Definition::Check {
                    table: relation,
                    constraint,
                })
            }
            _ => unreachable!("selected index or relation constraint"),
        }
    }
}

fn text(value: Value) -> Result<String, SQLError> {
    match value {
        Value::Str(text) => Ok(text),
        _ => Err(SQLError::Internal(
            "dependent definition has no SQL text".into(),
        )),
    }
}
