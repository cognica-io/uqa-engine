//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retain relation locks only after refreshing and revalidating the selected identity.

use super::{RelationLockMode, ScopedRelationLock};
use uqa_sql::SQLError;

pub trait RelationLockSession {
    fn acquire(
        &self,
        name: &str,
        mode: RelationLockMode,
        nowait: bool,
    ) -> Result<Option<ScopedRelationLock<'_>>, SQLError>;
    fn refresh_after_wait(&self) -> Result<(), SQLError>;
}

pub trait RelationLockCatalog {
    fn relation_object_id(&self, name: &str) -> Result<Option<[u8; 16]>, SQLError>;
    fn table_name(&self, object_id: [u8; 16]) -> Option<String>;
}

pub struct RelationBinding<T> {
    pub name: String,
    pub object_id: Option<[u8; 16]>,
    pub value: T,
}

pub fn acquire_relation<'a>(
    session: &'a dyn RelationLockSession,
    name: &str,
    mode: RelationLockMode,
    nowait: bool,
) -> Result<ScopedRelationLock<'a>, SQLError> {
    session.acquire(name, mode, nowait)?.ok_or_else(|| {
        let local = uqa_core::RelationIdentity::parse_reference(name)
            .map_or_else(|_| name.to_string(), |(_, local)| local);
        SQLError::Routine {
            sqlstate: "55P03".into(),
            message: format!("could not obtain lock on relation \"{local}\""),
        }
    })
}

/// A wait can change both the name's identity and the same object's definition or privileges. Resolve and validate again before retaining the provisional lock; a changed identity releases that acquisition before retrying.
pub fn bind_relation<T>(
    session: &dyn RelationLockSession,
    mode: RelationLockMode,
    nowait: bool,
    mut resolve: impl FnMut() -> Result<Option<RelationBinding<T>>, SQLError>,
    mut validate: impl FnMut(&RelationBinding<T>) -> Result<(), SQLError>,
) -> Result<Option<RelationBinding<T>>, SQLError> {
    loop {
        let Some(initial) = resolve()? else {
            return Ok(None);
        };
        validate(&initial)?;
        let guard = acquire_relation(session, &initial.name, mode, nowait)?;
        session.refresh_after_wait()?;
        let Some(current) = resolve()? else {
            return Ok(None);
        };
        if initial.name == current.name && initial.object_id == current.object_id {
            validate(&current)?;
            guard.retain();
            return Ok(Some(current));
        }
    }
}

/// Inherited members retain their original identity through a concurrent rename. A dropped member is skipped; a reused old name never substitutes for that member.
pub fn lock_descendants(
    catalog: &dyn RelationLockCatalog,
    session: &dyn RelationLockSession,
    names: impl Iterator<Item = String>,
    mode: RelationLockMode,
    nowait: bool,
) -> Result<(), SQLError> {
    let targets = names
        .map(|name| {
            catalog
                .relation_object_id(&name)
                .map(|id| id.map(|id| (name, id)))
        })
        .collect::<Result<Vec<_>, _>>()?;
    for (mut name, object_id) in targets.into_iter().flatten() {
        loop {
            let acquired = match acquire_relation(session, &name, mode, nowait) {
                Err(error) if error.sqlstate() != Some("55P03") => return Err(error),
                other => other,
            };
            session.refresh_after_wait()?;
            let Some(current) = catalog.table_name(object_id) else {
                break;
            };
            if current == name {
                acquired?.retain();
                break;
            }
            name = current;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
