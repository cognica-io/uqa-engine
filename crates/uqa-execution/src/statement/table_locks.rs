//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered explicit table locks, binding rechecks, inheritance and view authorization.

use crate::row_locks::{
    binding::{
        bind_relation, lock_descendants, RelationBinding, RelationLockCatalog, RelationLockSession,
    },
    RelationLockMode,
};
use uqa_sql::{
    ast::{LockTableStmt, TableLockMode},
    catalog::{
        resolution::RelationResolution, roles::guards::RoleCatalogGuards, security::TableSecurity,
        stored_view::StoredView,
    },
    semantics::table_locks::{ensure_lock_privilege, view_lock_targets},
    SQLError, SQLResult,
};

pub struct TableLockMetadata {
    pub object_id: [u8; 16],
    pub security: TableSecurity,
}

pub trait TableLockCatalog: RelationLockCatalog {
    fn resolve(&self, name: &str, bound: bool) -> Result<RelationResolution, SQLError>;
    fn table(&self, name: &str) -> Result<Option<TableLockMetadata>, SQLError>;
    fn view(&self, name: &str) -> Result<Option<StoredView>, SQLError>;
    fn descendants(&self, name: &str) -> Result<Vec<String>, SQLError>;
}

pub trait TableLockSession: RelationLockSession {
    fn in_transaction_block(&self) -> bool;
    fn current_user(&self) -> String;
}

#[derive(Clone, Copy)]
pub struct TableLockContext<'a> {
    pub catalog: &'a dyn TableLockCatalog,
    pub roles: &'a dyn RoleCatalogGuards,
    pub session: &'a dyn TableLockSession,
}

struct BoundRelation {
    name: String,
    kind: &'static str,
    metadata: TableLockMetadata,
    view: Option<StoredView>,
}

impl TableLockContext<'_> {
    fn resolve(
        &self,
        name: &str,
        bound: bool,
        view_source: bool,
    ) -> Result<Option<BoundRelation>, SQLError> {
        let (canonical, kind) = match self.catalog.resolve(name, bound)? {
            RelationResolution::Found(canonical, kind) => (canonical, kind),
            RelationResolution::MissingSchema(schema) => {
                return Err(SQLError::Routine {
                    sqlstate: "3F000".into(),
                    message: format!("schema \"{schema}\" does not exist"),
                })
            }
            RelationResolution::MissingRelation => return Err(SQLError::UnknownTable(name.into())),
        };
        match kind {
            "table" => self
                .catalog
                .table(&canonical)?
                .map(|metadata| BoundRelation {
                    name: canonical,
                    kind,
                    metadata,
                    view: None,
                }),
            "view" => self.catalog.view(&canonical)?.map(|view| BoundRelation {
                name: canonical,
                kind,
                metadata: TableLockMetadata {
                    object_id: view.object_id,
                    security: view.security(),
                },
                view: Some(view),
            }),
            _ if view_source => return Ok(None),
            _ => {
                let (_, local) = uqa_core::RelationIdentity::parse_reference(&canonical)
                    .map_err(SQLError::Internal)?;
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("cannot lock relation \"{local}\""),
                });
            }
        }
        .map(Some)
        .ok_or_else(|| SQLError::UnknownTable(name.into()))
    }

    fn lock_named(
        &self,
        name: &str,
        bound: bool,
        subject: &str,
        statement: &LockTableStmt,
        view_source: bool,
    ) -> Result<Option<BoundRelation>, SQLError> {
        bind_relation(
            self.session,
            statement.mode.into(),
            statement.nowait,
            || {
                self.resolve(name, bound, view_source).map(|relation| {
                    relation.map(|value| RelationBinding {
                        name: value.name.clone(),
                        object_id: Some(value.metadata.object_id),
                        value,
                    })
                })
            },
            |relation| {
                let target = &relation.value;
                ensure_lock_privilege(
                    self.roles,
                    &target.metadata.security,
                    subject,
                    statement.mode,
                    &relation.name,
                    target.kind,
                )
            },
        )
        .map(|relation| relation.map(|relation| relation.value))
    }

    fn lock_descendants(&self, name: &str, statement: &LockTableStmt) -> Result<(), SQLError> {
        lock_descendants(
            self.catalog,
            self.session,
            self.catalog
                .descendants(name)?
                .into_iter()
                .filter(|child| child != name),
            statement.mode.into(),
            statement.nowait,
        )
    }

    fn lock_references(
        &self,
        relation: BoundRelation,
        descendants: bool,
        statement: &LockTableStmt,
        ancestors: &mut Vec<[u8; 16]>,
    ) -> Result<(), SQLError> {
        let Some(view) = relation.view else {
            return if descendants {
                self.lock_descendants(&relation.name, statement)
            } else {
                Ok(())
            };
        };
        if ancestors.contains(&view.object_id) {
            return Ok(());
        }
        ancestors.push(view.object_id);
        let subject = if view.security_invoker() {
            self.session.current_user()
        } else {
            view.role_owner.clone()
        };
        for target in view_lock_targets(&view.query)? {
            if self
                .resolve(&target.name, true, true)?
                .is_some_and(|source| ancestors.contains(&source.metadata.object_id))
            {
                continue;
            }
            if let Some(source) = self.lock_named(&target.name, true, &subject, statement, true)? {
                self.lock_references(source, target.include_descendants, statement, ancestors)?;
            }
        }
        ancestors.pop();
        Ok(())
    }
}

pub fn execute(
    context: TableLockContext<'_>,
    statement: &LockTableStmt,
    nested: bool,
) -> Result<SQLResult, SQLError> {
    if !nested && !context.session.in_transaction_block() {
        return Err(SQLError::Routine {
            sqlstate: "25P01".into(),
            message: "LOCK TABLE can only be used in transaction blocks".into(),
        });
    }
    let subject = context.session.current_user();
    for target in &statement.targets {
        if let Some(relation) =
            context.lock_named(&target.name, false, &subject, statement, false)?
        {
            context.lock_references(
                relation,
                target.include_descendants,
                statement,
                &mut Vec::new(),
            )?;
        }
    }
    Ok(SQLResult::empty())
}

impl From<TableLockMode> for RelationLockMode {
    fn from(mode: TableLockMode) -> Self {
        match mode {
            TableLockMode::AccessShare => Self::AccessShare,
            TableLockMode::RowShare => Self::RowShare,
            TableLockMode::RowExclusive => Self::RowExclusive,
            TableLockMode::ShareUpdateExclusive => Self::ShareUpdateExclusive,
            TableLockMode::Share => Self::Share,
            TableLockMode::ShareRowExclusive => Self::ShareRowExclusive,
            TableLockMode::Exclusive => Self::Exclusive,
            TableLockMode::AccessExclusive => Self::AccessExclusive,
        }
    }
}

#[cfg(test)]
mod tests;
