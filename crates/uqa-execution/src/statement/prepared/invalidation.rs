//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Preserve actual catalog publication events across transaction and savepoint completion. Restoring an older definition cannot erase the invalidation that its intervening publication caused.

use std::collections::BTreeSet;
use uqa_sql::prepared::{dependencies::PreparedAnalysisDependencies, entry::PreparedStatementPlan};

/// Changes to executable dependencies, such as domain constraints, preserve already-read input constants but require a fresh executable plan.
pub fn invalidate_execution_plans<'a>(
    entries: impl IntoIterator<Item = &'a mut PreparedStatementPlan>,
) {
    for entry in entries {
        if !entry.has_tracked_executable_dependencies() {
            entry.plan = None;
        }
    }
}

/// Registry publication distinguishes executable definitions from builtin ACL tuples. Surviving calls check their ACL at execution initialization; already-folded calls have no remaining execution permission dependency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CatalogRegistryChange {
    Definitions,
    BuiltinRoutinePrivileges,
}

impl CatalogRegistryChange {
    pub fn invalidate<'a>(self, entries: impl IntoIterator<Item = &'a mut PreparedStatementPlan>) {
        if self == Self::Definitions {
            invalidate_execution_plans(entries);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum PreparedCatalogChange {
    Relation(u32),
    Routine([u8; 16]),
    GlobalCatalog,
}

impl PreparedCatalogChange {
    pub fn invalidate_with_routines<'a>(
        self,
        entries: impl IntoIterator<Item = &'a mut PreparedStatementPlan>,
        routines: &crate::routines::invocation::bodies::SessionRoutineBodies,
    ) {
        self.invalidate(entries);
        routines.invalidate(|dependencies| self.affects(dependencies));
    }

    fn affects(self, dependencies: &PreparedAnalysisDependencies) -> bool {
        match self {
            Self::Relation(oid) => dependencies.relations.contains(&oid),
            Self::Routine(identity) => dependencies.routines.contains(&identity),
            Self::GlobalCatalog => true,
        }
    }

    pub fn invalidate<'a>(self, entries: impl IntoIterator<Item = &'a mut PreparedStatementPlan>) {
        for entry in entries {
            if self.affects(&entry.dependencies) {
                entry.invalidate();
            }
        }
    }
}

/// A savepoint boundary inside one frame's catalog change log.
#[derive(Clone, Copy, Debug)]
pub struct PreparedInvalidationMark(usize);

/// One deduplicated set per live savepoint level. Repeated mutations of an object retain one identity per level; rollback discards only the levels it undoes.
#[derive(Default)]
pub struct PreparedInvalidationLog {
    levels: Vec<BTreeSet<PreparedCatalogChange>>,
}

impl PreparedInvalidationLog {
    pub fn invalidate_with_routines<'a>(
        &self,
        entries: impl IntoIterator<Item = &'a mut PreparedStatementPlan>,
        routines: &crate::routines::invocation::bodies::SessionRoutineBodies,
    ) {
        self.invalidate(entries);
        routines.invalidate(|dependencies| self.affects(dependencies));
    }

    fn current(&mut self) -> &mut BTreeSet<PreparedCatalogChange> {
        if self.levels.is_empty() {
            self.levels.push(BTreeSet::new());
        }
        self.levels
            .last_mut()
            .expect("invalidation log has a current level")
    }

    pub fn record(&mut self, change: PreparedCatalogChange) {
        let level = self.current();
        if level.contains(&PreparedCatalogChange::GlobalCatalog) {
            return;
        }
        if change == PreparedCatalogChange::GlobalCatalog {
            level.clear();
        }
        level.insert(change);
    }

    pub fn mark(&mut self) -> PreparedInvalidationMark {
        self.current();
        let mark = PreparedInvalidationMark(self.levels.len());
        self.levels.push(BTreeSet::new());
        mark
    }

    pub fn release(&mut self, mark: PreparedInvalidationMark) {
        let released = Self {
            levels: self.levels.split_off(mark.0),
        };
        self.append(released);
    }

    /// Return events to resend after undo, retaining the named savepoint with a fresh empty level. A statement prepared after the original mutation must also see the undo invalidation.
    pub fn rollback_to(&mut self, mark: PreparedInvalidationMark) -> Self {
        let undone = Self {
            levels: self.levels.split_off(mark.0),
        };
        self.levels.push(BTreeSet::new());
        undone
    }

    pub fn append(&mut self, other: Self) {
        for change in other.levels.into_iter().flatten() {
            self.record(change);
        }
    }

    pub fn invalidate<'a>(&self, entries: impl IntoIterator<Item = &'a mut PreparedStatementPlan>) {
        for entry in entries {
            if self.affects(&entry.dependencies) {
                entry.invalidate();
            }
        }
    }

    fn affects(&self, dependencies: &PreparedAnalysisDependencies) -> bool {
        self.levels
            .iter()
            .flatten()
            .any(|change| change.affects(dependencies))
    }
}

#[cfg(test)]
mod tests;
