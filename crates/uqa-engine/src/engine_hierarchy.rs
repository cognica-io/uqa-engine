//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Durable table-hierarchy lookup and mutation.

use super::{Engine, RelationIdentity, SQLError, StorageBackendError, StorageBackendResult};
use std::collections::BTreeSet;

impl Engine {
    pub(crate) fn try_table_hierarchy(
        &self,
        table: &str,
    ) -> StorageBackendResult<uqa_sql::ast::TableHierarchy> {
        let table = self
            .try_query_table(table)?
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

    /// Return the canonical table followed by every descendant in stable catalog order. A cycle indicates corrupt durable metadata and is never silently truncated.
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

    /// Resolve a SELECT hierarchy through the relation metadata captured for a cursor snapshot. DDL performed after DECLARE may rename a table or change its inheritance edges, but `PostgreSQL` keeps the cursor's already-bound relation identities and descendant set.
    pub(crate) fn query_hierarchy_scan_tables(
        &self,
        table: &str,
        include_descendants: bool,
    ) -> Result<Vec<String>, SQLError> {
        let Some(tables) = self.query_table_snapshots.as_ref() else {
            return self.hierarchy_scan_tables(table, include_descendants);
        };
        let root = self
            .relation_lookup_candidates(table)
            .map_err(|error| SQLError::Internal(format!("resolve query table `{table}`: {error}")))?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
        if !include_descendants {
            return Ok(vec![root.qualified_name()]);
        }
        let mut output = Vec::new();
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        Self::collect_snapshot_hierarchy_descendants(
            tables,
            &root,
            &mut visiting,
            &mut visited,
            &mut output,
        )?;
        Ok(output)
    }

    fn collect_snapshot_hierarchy_descendants(
        tables: &std::collections::BTreeMap<RelationIdentity, std::sync::Arc<super::TableState>>,
        parent: &RelationIdentity,
        visiting: &mut BTreeSet<RelationIdentity>,
        visited: &mut BTreeSet<RelationIdentity>,
        output: &mut Vec<String>,
    ) -> Result<(), SQLError> {
        if visiting.contains(parent) {
            return Err(SQLError::Internal(format!(
                "table inheritance cycle reaches `{}`",
                parent.qualified_name()
            )));
        }
        if !visited.insert(parent.clone()) {
            return Ok(());
        }
        visiting.insert(parent.clone());
        output.push(parent.qualified_name());
        let parent_name = parent.qualified_name();
        let children = tables
            .iter()
            .filter(|(_, state)| {
                state
                    .hierarchy
                    .read()
                    .parents
                    .iter()
                    .any(|candidate| candidate == &parent_name)
            })
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        for child in children {
            Self::collect_snapshot_hierarchy_descendants(
                tables, &child, visiting, visited, output,
            )?;
        }
        visiting.remove(parent);
        Ok(())
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

    pub(crate) fn query_direct_hierarchy_children(
        &self,
        parent: &str,
    ) -> Result<Vec<String>, SQLError> {
        let Some(tables) = self.query_table_snapshots.as_ref() else {
            return self.direct_hierarchy_children(parent);
        };
        let parent = self
            .relation_lookup_candidates(parent)
            .map_err(|error| {
                SQLError::Internal(format!("resolve query table `{parent}`: {error}"))
            })?
            .into_iter()
            .find(|candidate| tables.contains_key(candidate))
            .ok_or_else(|| SQLError::UnknownTable(parent.to_string()))?
            .qualified_name();
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

    /// Return the physical counter owner for legacy auto-increment metadata. Declarative partitions share the top partitioned parent's counter; newly created SERIAL and identity columns use their durable sequence binding instead.
    pub(crate) fn partition_identity_owner(&self, table: &str) -> Result<String, SQLError> {
        if let Some(root) = self.partition_hierarchy_root(table)? {
            return Ok(root);
        }
        self.try_resolve_table_name(table)
            .map_err(|error| SQLError::Internal(format!("resolve table `{table}`: {error}")))?
            .ok_or_else(|| SQLError::UnknownTable(table.to_string()))
    }

    /// Rebuild a partitioned parent's shared legacy auto-increment watermark from every physical partition. Persistent table counters are reconstructed from document ids, so the logical owner must observe the maximum restored descendant watermark before it can allocate another value.
    pub(crate) fn synchronize_partition_identity_watermarks(&self) -> StorageBackendResult<()> {
        let entries = self
            .storage
            .tables
            .read()
            .iter()
            .map(|(relation, table)| (relation.qualified_name(), table.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut root_watermarks = std::collections::BTreeMap::<String, u128>::new();
        for (name, table) in &entries {
            if !table.columns.read().iter().any(|column| {
                column
                    .auto_increment
                    .as_ref()
                    .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
            }) {
                continue;
            }
            let mut current = name.clone();
            let mut visited = BTreeSet::new();
            let mut participates = false;
            loop {
                if !visited.insert(current.clone()) {
                    return Err(StorageBackendError::Other(format!(
                        "partition hierarchy cycle reaches `{current}`"
                    )));
                }
                let state = entries.get(&current).ok_or_else(|| {
                    StorageBackendError::Other(format!(
                        "partition hierarchy references missing table `{current}`"
                    ))
                })?;
                let hierarchy = state.hierarchy.read();
                participates |= hierarchy.partition_spec.is_some() || hierarchy.is_partition();
                if !hierarchy.is_partition() {
                    break;
                }
                current = hierarchy.parents.first().cloned().ok_or_else(|| {
                    StorageBackendError::Other("partition has no parent relation".into())
                })?;
            }
            if participates {
                let watermark = *table.next_id.lock();
                root_watermarks
                    .entry(current)
                    .and_modify(|current| *current = (*current).max(watermark))
                    .or_insert(watermark);
            }
        }
        for (root, watermark) in root_watermarks {
            let table = entries.get(&root).ok_or_else(|| {
                StorageBackendError::Other(format!(
                    "partition hierarchy references missing root `{root}`"
                ))
            })?;
            let mut current = table.next_id.lock();
            *current = (*current).max(watermark);
        }
        Ok(())
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
