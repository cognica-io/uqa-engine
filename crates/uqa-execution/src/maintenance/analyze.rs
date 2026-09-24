//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve analysis targets, retain their logical locks and recheck identities after waits.

use uqa_sql::{maintenance::VacuumPrivileges, SQLError};
use uqa_storage::StorageBackendResult;

use crate::row_locks::{RelationLockMode, ScopedRelationLock};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnalyzeTarget {
    pub name: String,
    pub object_id: [u8; 16],
}

pub trait AnalyzeCatalog {
    fn resolve(&self, name: &str) -> Result<Option<AnalyzeTarget>, SQLError>;
    fn all_tables(&self) -> Vec<AnalyzeTarget>;
    fn current_target(&self, object_id: [u8; 16]) -> Option<AnalyzeTarget>;
    fn descendants(&self, name: &str) -> Result<Vec<String>, SQLError>;
}

pub trait AnalyzeLocks {
    fn bind_name(&self, name: &str) -> Result<ScopedRelationLock<'_>, SQLError>;
    fn acquire(&self, name: &str, mode: RelationLockMode) -> Result<(), SQLError>;
    fn refresh_after_wait(&self) -> Result<(), SQLError>;
}

pub trait AnalyzeNotices {
    fn warning(&self, message: &str);
}

pub struct AnalyzeContext<'a> {
    pub catalog: &'a dyn AnalyzeCatalog,
    pub locks: &'a dyn AnalyzeLocks,
    pub notices: &'a dyn AnalyzeNotices,
    pub privileges: Option<&'a dyn VacuumPrivileges>,
}

fn bind_requested(context: &AnalyzeContext<'_>, name: &str) -> Result<AnalyzeTarget, SQLError> {
    loop {
        let initial = context
            .catalog
            .resolve(name)?
            .ok_or_else(|| SQLError::UnknownTable(name.into()))?;
        let _binding = context.locks.bind_name(&initial.name)?;
        context.locks.refresh_after_wait()?;
        let current = context
            .catalog
            .resolve(name)?
            .ok_or_else(|| SQLError::UnknownTable(name.into()))?;
        if initial == current {
            return Ok(current);
        }
    }
}

/// Lock the selected object, following a rename but never adopting a replacement with the same name. Transaction and savepoint adapters retain the acquisitions.
fn lock_identity(
    context: &AnalyzeContext<'_>,
    mut target: AnalyzeTarget,
    mode: RelationLockMode,
    warn_missing: bool,
) -> Result<Option<AnalyzeTarget>, SQLError> {
    loop {
        context.locks.acquire(&target.name, mode)?;
        context.locks.refresh_after_wait()?;
        let Some(current) = context.catalog.current_target(target.object_id) else {
            if warn_missing {
                context.notices.warning(&format!(
                    "skipping analyze of \"{}\" --- relation no longer exists",
                    target.name
                ));
            }
            return Ok(None);
        };
        if current.name == target.name {
            return Ok(Some(current));
        }
        // Existing relation lock identities follow names. A renamed target must acquire its current name before the identity can be considered protected.
        target = current;
    }
}

pub fn prepare_targets(
    context: &AnalyzeContext<'_>,
    requested: Option<&str>,
    include_descendants: bool,
) -> Result<Vec<AnalyzeTarget>, SQLError> {
    let selected = match requested {
        Some(name) => vec![bind_requested(context, name)?],
        None => context.catalog.all_tables(),
    };
    let mut targets = Vec::with_capacity(selected.len());
    for target in selected {
        let Some(target) = lock_identity(
            context,
            target,
            RelationLockMode::ShareUpdateExclusive,
            true,
        )?
        else {
            continue;
        };
        if let Some(privileges) = context.privileges {
            privileges.ensure_maintain(&target.name)?;
        }
        if include_descendants {
            for member in context.catalog.descendants(&target.name)? {
                if member == target.name {
                    continue;
                }
                if let Some(child) = context.catalog.resolve(&member)? {
                    lock_identity(context, child, RelationLockMode::AccessShare, true)?;
                }
            }
        }
        targets.push(target);
    }
    Ok(targets)
}

/// Automatic maintenance may discover that its previously enumerated table has disappeared before collection begins.
pub fn prepare_optional_target(
    context: &AnalyzeContext<'_>,
    requested: &str,
) -> Result<Option<AnalyzeTarget>, SQLError> {
    let Some(target) = context.catalog.resolve(requested)? else {
        return Ok(None);
    };
    lock_identity(
        context,
        target,
        RelationLockMode::ShareUpdateExclusive,
        false,
    )
}

pub fn run_locked_targets(
    targets: &[AnalyzeTarget],
    mut analyze: impl FnMut(&str) -> StorageBackendResult<()>,
) -> StorageBackendResult<()> {
    for target in targets {
        analyze(&target.name)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
