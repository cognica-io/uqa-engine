//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table-hierarchy lookup and mutation.

use super::{Engine, SQLError, StorageBackendError, StorageBackendResult};
use std::collections::BTreeSet;

impl Engine {
    pub(crate) fn try_table_hierarchy(
        &self,
        table: &str,
    ) -> StorageBackendResult<uqa_sql::ast::TableHierarchy> {
        let table = self
            .try_table(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        let hierarchy = table.hierarchy.read().clone();
        Ok(hierarchy)
    }

    pub(crate) fn install_table_hierarchy(
        &self,
        table: &str,
        hierarchy: uqa_sql::ast::TableHierarchy,
    ) -> StorageBackendResult<()> {
        let table = self
            .try_table(table)?
            .ok_or_else(|| StorageBackendError::Other(format!("table `{table}` does not exist")))?;
        *table.hierarchy.write() = hierarchy;
        Ok(())
    }

    /// Return the canonical table followed by every descendant in stable
    /// catalog order. A cycle indicates corrupt durable metadata and is never
    /// silently truncated.
    pub(crate) fn hierarchy_scan_tables(
        &self,
        table: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        let root = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        if !include_descendants {
            return Ok(vec![root]);
        }
        let mut output = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        self.collect_hierarchy_descendants(&root, &mut visiting, &mut visited, &mut output)?;
        Ok(output)
    }

    fn collect_hierarchy_descendants(
        &self,
        parent: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        output: &mut Vec<String>,
    ) -> Result<(), SQLError> {
        if visiting.contains(parent) {
            return Err(SQLError::Internal(format!(
                "table inheritance cycle reaches `{parent}`"
            )));
        }
        if !visited.insert(parent.to_string()) {
            return Ok(());
        }
        visiting.insert(parent.to_string());
        output.push(parent.to_string());
        let tables = self.storage.tables.read();
        let children = tables
            .iter()
            .filter(|(_, state)| {
                state
                    .hierarchy
                    .read()
                    .parents
                    .iter()
                    .any(|candidate| candidate == parent)
            })
            .map(|(identity, _)| identity.qualified_name())
            .collect::<Vec<_>>();
        drop(tables);
        for child in children {
            self.collect_hierarchy_descendants(&child, visiting, visited, output)?;
        }
        visiting.remove(parent);
        visited.insert(parent.to_string());
        Ok(())
    }

    pub(crate) fn direct_hierarchy_children(&self, parent: &str) -> Result<Vec<String>, SQLError> {
        let parent = self
            .try_resolve_table_name(parent)
            .map_err(|error| SQLError::Internal(format!("resolve table `{parent}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(parent.to_string()))?;
        let tables = self.storage.tables.read();
        Ok(tables
            .iter()
            .filter(|(_, state)| {
                state
                    .hierarchy
                    .read()
                    .parents
                    .iter()
                    .any(|candidate| candidate == &parent)
            })
            .map(|(identity, _)| identity.qualified_name())
            .collect())
    }

    /// Return the canonical relation followed by each direct ancestor in breadth-first declaration order. Physical row identity stays attached to the relation that stores the row; this list is only for discovering constraints declared against a logical ancestor.
    pub(crate) fn hierarchy_ancestor_tables(&self, table: &str) -> Result<Vec<String>, SQLError> {
        let table = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let mut output = Vec::new();
        let mut visited = BTreeSet::new();
        let mut pending = std::collections::VecDeque::from([table]);
        while let Some(candidate) = pending.pop_front() {
            if !visited.insert(candidate.clone()) {
                continue;
            }
            let hierarchy = self
                .try_table_hierarchy(&candidate)
                .map_err(|error| SQLError::Internal(format!("read table hierarchy: {error}")))?;
            output.push(candidate);
            pending.extend(hierarchy.parents);
        }
        Ok(output)
    }

    /// Return the top declarative-partitioning root that owns `table`, or `None` when the relation is not a partitioned table or partition.
    pub(crate) fn partition_hierarchy_root(&self, table: &str) -> Result<Option<String>, SQLError> {
        let mut current = self
            .try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        let mut visited = BTreeSet::new();
        let mut participates = false;
        loop {
            if !visited.insert(current.clone()) {
                return Err(SQLError::Internal(format!(
                    "partition hierarchy cycle reaches `{current}`"
                )));
            }
            let hierarchy = self.try_table_hierarchy(&current).map_err(|error| {
                SQLError::Internal(format!("read partition hierarchy: {error}"))
            })?;
            participates |= hierarchy.partition_spec.is_some() || hierarchy.is_partition();
            if !hierarchy.is_partition() {
                return Ok(participates.then_some(current));
            }
            current = hierarchy
                .parents
                .first()
                .cloned()
                .ok_or_else(|| SQLError::Internal("partition has no parent relation".into()))?;
        }
    }

    /// Expand a `DROP TABLE` target set through table hierarchy dependencies.
    /// Declarative partitions are owned by their parent and are always dropped
    /// with it, while ordinary inheritance children require `CASCADE`.
    pub(crate) fn hierarchy_drop_targets(
        &self,
        roots: &[String],
        cascade: bool,
    ) -> (Vec<String>, Vec<String>) {
        let mut targets = roots.iter().cloned().collect::<BTreeSet<_>>();
        let mut blockers = BTreeSet::new();
        loop {
            let mut added = false;
            let tables = self.storage.tables.read();
            for (identity, table) in tables.iter() {
                let candidate = identity.qualified_name();
                if targets.contains(&candidate) {
                    continue;
                }
                let hierarchy = table.hierarchy.read();
                if !hierarchy
                    .parents
                    .iter()
                    .any(|parent| targets.contains(parent))
                {
                    continue;
                }
                if hierarchy.is_partition() || cascade {
                    added |= targets.insert(candidate);
                } else {
                    blockers.insert(candidate);
                }
            }
            if !added {
                break;
            }
        }
        (
            targets.into_iter().collect(),
            blockers.into_iter().collect(),
        )
    }
}
