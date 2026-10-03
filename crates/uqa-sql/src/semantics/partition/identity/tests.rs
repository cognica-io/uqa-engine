//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{foreign_key_scan_tables, partition_ancestor_tables};
use crate::ast::{ColumnDef, PartitionBound, PartitionSpec, PartitionStrategy, TableHierarchy};
use crate::semantics::partition::PartitionCatalog;
use crate::SQLError;
use std::collections::BTreeMap;

/// A partitioned `root` with the partitions `a` and a partitioned `b` holding `b1`, beside a plain `parent` with an inheritance child.
struct Catalog(BTreeMap<String, TableHierarchy>);

impl Catalog {
    fn new() -> Self {
        let partitioned = || TableHierarchy {
            partition_spec: Some(PartitionSpec {
                strategy: PartitionStrategy::Range,
                keys: Vec::new(),
            }),
            ..TableHierarchy::default()
        };
        let partition = |parent: &str| TableHierarchy {
            parents: vec![parent.into()],
            partition_bound: Some(PartitionBound::Default),
            ..TableHierarchy::default()
        };
        let mut b = partition("public.root");
        b.partition_spec = partitioned().partition_spec;
        Self(BTreeMap::from([
            ("public.root".into(), partitioned()),
            ("public.a".into(), partition("public.root")),
            ("public.b".into(), b),
            ("public.b1".into(), partition("public.b")),
            ("public.parent".into(), TableHierarchy::default()),
            (
                "public.child".into(),
                TableHierarchy {
                    parents: vec!["public.parent".into()],
                    ..TableHierarchy::default()
                },
            ),
        ]))
    }
}

impl PartitionCatalog for Catalog {
    fn try_table_hierarchy(&self, table: &str) -> Result<TableHierarchy, String> {
        self.0
            .get(table)
            .cloned()
            .ok_or_else(|| format!("no table `{table}`"))
    }

    fn direct_hierarchy_children(&self, parent: &str) -> Result<Vec<String>, SQLError> {
        Ok(self
            .0
            .iter()
            .filter(|(_, hierarchy)| hierarchy.parents.iter().any(|edge| edge == parent))
            .map(|(name, _)| name.clone())
            .collect())
    }

    fn try_resolve_table_name(&self, name: &str) -> Result<Option<String>, String> {
        Ok(self.0.contains_key(name).then(|| name.to_string()))
    }

    fn try_describe_table(&self, _table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
}

#[test]
fn foreign_keys_read_partitions_but_not_plain_inheritance_children() {
    let catalog = Catalog::new();
    assert_eq!(
        foreign_key_scan_tables(&catalog, "public.root").unwrap(),
        ["public.root", "public.a", "public.b", "public.b1"]
    );
    assert_eq!(
        foreign_key_scan_tables(&catalog, "public.b").unwrap(),
        ["public.b", "public.b1"]
    );
    assert_eq!(
        foreign_key_scan_tables(&catalog, "public.parent").unwrap(),
        ["public.parent"]
    );
    assert!(matches!(
        foreign_key_scan_tables(&catalog, "public.absent"),
        Err(SQLError::UnknownTable(_))
    ));
}

#[test]
fn partition_ancestors_stop_at_a_plain_inheritance_parent() {
    let catalog = Catalog::new();
    assert_eq!(
        partition_ancestor_tables(&catalog, "public.b1").unwrap(),
        ["public.b1", "public.b", "public.root"]
    );
    assert_eq!(
        partition_ancestor_tables(&catalog, "public.child").unwrap(),
        ["public.child"]
    );
}
