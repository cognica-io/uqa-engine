//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! TRUNCATE target expansion, partition rules and foreign-key dependency ordering.

use crate::SQLError;
use std::collections::BTreeSet;

/// Catalog metadata required to bind a TRUNCATE target set.
pub trait TruncateCatalog {
    fn try_resolve_visible_relation_kind(
        &self,
        name: &str,
    ) -> Result<Option<(String, &'static str)>, SQLError>;
    fn is_partitioned(&self, table: &str) -> Result<bool, String>;
    fn hierarchy_scan_tables(
        &self,
        table: &str,
        descendants: bool,
    ) -> Result<Vec<String>, SQLError>;
    fn referrers_to(&self, table: &str) -> Result<Vec<String>, String>;
    /// The relation followed by each partitioned table it is a partition of, nearest first.
    fn partition_ancestor_tables(&self, table: &str) -> Result<Vec<String>, SQLError>;
}

/// The tables whose foreign keys reference `table`'s rows: those that reference it and those that reference a partitioned table it is a partition of, as `PostgreSQL` derives a constraint on each referenced partition.
fn referencing_tables(catalog: &dyn TruncateCatalog, table: &str) -> Result<Vec<String>, SQLError> {
    let mut referencing = Vec::new();
    for ancestor in catalog.partition_ancestor_tables(table)? {
        for referrer in catalog
            .referrers_to(&ancestor)
            .map_err(|err| SQLError::Internal(format!("read foreign keys: {err}")))?
        {
            if !referencing.contains(&referrer) {
                referencing.push(referrer);
            }
        }
    }
    Ok(referencing)
}

#[cfg(test)]
mod tests;

#[derive(Debug)]
pub struct TruncateTargets {
    pub all: BTreeSet<String>,
    pub trigger_order: Vec<String>,
    pub privilege_targets: BTreeSet<String>,
    /// The tables `CASCADE` added because their foreign keys reference a target, in the order they were added.
    pub cascaded: Vec<String>,
}

pub fn resolve_truncate_targets(
    catalog: &dyn TruncateCatalog,
    tables: &[crate::ast::TruncateTarget],
    cascade: bool,
) -> Result<TruncateTargets, SQLError> {
    let mut targets = BTreeSet::new();
    let mut trigger_targets = Vec::new();
    let mut privilege_targets = BTreeSet::new();
    for requested in tables {
        let table = match catalog.try_resolve_visible_relation_kind(&requested.table)? {
            Some((table, "table")) => table,
            Some((_, "foreign table")) => {
                return Err(SQLError::Unsupported(format!(
                    "TRUNCATE TABLE: foreign table `{}` cannot be truncated",
                    requested.table
                )))
            }
            // `truncate_check_rel` accepts only tables.
            Some((relation, _)) => {
                let local = uqa_core::RelationIdentity::from_legacy_name(&relation)
                    .map_or(relation, |relation| relation.name);
                return Err(SQLError::Routine {
                    sqlstate: "42809".into(),
                    message: format!("\"{local}\" is not a table"),
                });
            }
            None => return Err(SQLError::UnknownTable(requested.table.clone())),
        };
        privilege_targets.insert(table.clone());
        let partitioned = catalog
            .is_partitioned(&table)
            .map_err(|err| SQLError::Internal(format!("read table hierarchy: {err}")))?;
        if !requested.include_descendants && partitioned {
            return Err(SQLError::Routine {
                sqlstate: "42809".into(),
                message: "cannot truncate only a partitioned table".into(),
            });
        }
        for target in catalog.hierarchy_scan_tables(&table, requested.include_descendants)? {
            if targets.insert(target.clone()) {
                trigger_targets.push(target);
            }
        }
    }
    let mut cascaded = Vec::new();
    if cascade {
        let mut cursor = 0;
        while let Some(table) = trigger_targets.get(cursor).cloned() {
            cursor += 1;
            for referrer in referencing_tables(catalog, &table)? {
                if targets.insert(referrer.clone()) {
                    privilege_targets.insert(referrer.clone());
                    cascaded.push(referrer.clone());
                    trigger_targets.push(referrer);
                }
            }
        }
    }
    Ok(TruncateTargets {
        all: targets,
        trigger_order: trigger_targets,
        privilege_targets,
        cascaded,
    })
}

/// Reject truncating a table that a table outside the targets references, naming the first such pair in target order, as `PostgreSQL`'s `heap_truncate_check_FKs` does.
pub fn validate_truncate_references(
    catalog: &dyn TruncateCatalog,
    targets: &TruncateTargets,
) -> Result<(), SQLError> {
    for table in &targets.trigger_order {
        if let Some(referrer) = referencing_tables(catalog, table)?
            .into_iter()
            .find(|referrer| !targets.all.contains(referrer))
        {
            let referrer = crate::semantics::foreign_keys::foreign_key_relation_name(&referrer);
            return Err(SQLError::Diagnostic {
                sqlstate: "0A000".into(),
                message: "cannot truncate a table referenced in a foreign key constraint".into(),
                detail: Some(format!(
                    "Table \"{referrer}\" references \"{}\".",
                    crate::semantics::foreign_keys::foreign_key_relation_name(table)
                )),
                hint: Some(format!(
                    "Truncate table \"{referrer}\" at the same time, or use TRUNCATE ... CASCADE."
                )),
            });
        }
    }
    Ok(())
}

/// Referencing relations precede their targets, with cycles visited once.
pub fn truncate_dependency_order(
    catalog: &dyn TruncateCatalog,
    targets: &TruncateTargets,
) -> Result<Vec<String>, SQLError> {
    let mut ordered = Vec::with_capacity(targets.all.len());
    let mut visiting = BTreeSet::new();
    let mut visited = BTreeSet::new();
    for table in &targets.trigger_order {
        visit_truncate_target(
            catalog,
            table,
            &targets.all,
            &mut visiting,
            &mut visited,
            &mut ordered,
        )?;
    }
    Ok(ordered)
}

fn visit_truncate_target(
    catalog: &dyn TruncateCatalog,
    table: &str,
    targets: &BTreeSet<String>,
    visiting: &mut BTreeSet<String>,
    visited: &mut BTreeSet<String>,
    ordered: &mut Vec<String>,
) -> Result<(), SQLError> {
    if visited.contains(table) || !visiting.insert(table.to_string()) {
        return Ok(());
    }
    for referrer in referencing_tables(catalog, table)? {
        if targets.contains(&referrer) {
            visit_truncate_target(catalog, &referrer, targets, visiting, visited, ordered)?;
        }
    }
    visiting.remove(table);
    if visited.insert(table.to_string()) {
        ordered.push(table.to_string());
    }
    Ok(())
}
