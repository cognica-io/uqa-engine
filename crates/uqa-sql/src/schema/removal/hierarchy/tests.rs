//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::PartitionBound;
use std::{
    cell::{Cell, Ref, RefCell},
    collections::BTreeMap,
    rc::Rc,
};
struct Table {
    hierarchy: RefCell<TableHierarchy>,
    registry_held: Rc<Cell<bool>>,
}
impl HierarchyDropTable for Table {
    fn hierarchy(&self) -> HierarchyDropRead<'_> {
        assert!(self.registry_held.get());
        Box::new(self.hierarchy.borrow())
    }
}
struct Catalog {
    tables: RefCell<BTreeMap<RelationIdentity, Table>>,
    held: Rc<Cell<bool>>,
    reads: Cell<usize>,
}
struct Tables<'a> {
    tables: Ref<'a, BTreeMap<RelationIdentity, Table>>,
    held: &'a Cell<bool>,
}
impl Drop for Tables<'_> {
    fn drop(&mut self) {
        self.held.set(false);
    }
}
impl HierarchyDropTables for Tables<'_> {
    fn iter(&self) -> HierarchyDropEntries<'_> {
        Box::new(
            self.tables
                .iter()
                .map(|(name, table)| (name, table as &dyn HierarchyDropTable)),
        )
    }
}
impl HierarchyDropCatalog for Catalog {
    fn tables(&self) -> Box<dyn HierarchyDropTables + '_> {
        assert!(!self.held.replace(true));
        self.reads.set(self.reads.get() + 1);
        Box::new(Tables {
            tables: self.tables.borrow(),
            held: &self.held,
        })
    }
}
fn catalog(entries: &[(&str, &[&str], bool)]) -> Catalog {
    let held = Rc::new(Cell::new(false));
    let tables = entries
        .iter()
        .map(|(name, parents, partition)| {
            let hierarchy = TableHierarchy {
                parents: parents
                    .iter()
                    .map(|name| format!("public.{name}"))
                    .collect(),
                partition_bound: partition.then_some(PartitionBound::Default),
                ..TableHierarchy::default()
            };
            (
                RelationIdentity::new("public", *name),
                Table {
                    hierarchy: RefCell::new(hierarchy),
                    registry_held: held.clone(),
                },
            )
        })
        .collect();
    Catalog {
        tables: RefCell::new(tables),
        held,
        reads: Cell::new(0),
    }
}
#[test]
fn restrict_expands_nested_partitions_before_reporting_ordinary_inheritance_children() {
    let catalog = catalog(&[
        ("a_leaf", &["b_partition"], true),
        ("b_partition", &["z_root"], true),
        ("c_inherited", &["b_partition"], false),
        ("z_root", &[], false),
    ]);
    let (targets, blockers) = hierarchy_drop_targets(&catalog, &["public.z_root".into()], false);
    assert_eq!(
        targets,
        ["public.a_leaf", "public.b_partition", "public.z_root"]
    );
    assert_eq!(blockers, ["public.c_inherited"]);
    assert!(catalog.reads.get() >= 3);
    assert!(!catalog.held.get());
    assert!(catalog.tables.try_borrow_mut().is_ok());
    for table in catalog.tables.borrow().values() {
        assert!(table.hierarchy.try_borrow_mut().is_ok());
    }
}
#[test]
fn cascade_deduplicates_multiple_parent_paths_and_preserves_requested_roots() {
    let catalog = catalog(&[
        ("a_child", &["b_left", "c_right"], false),
        ("b_left", &["z_root"], false),
        ("c_right", &["z_root"], true),
        ("z_root", &[], false),
    ]);
    let roots = vec!["public.z_root".to_string(), "public.z_root".to_string()];
    let (targets, blockers) = hierarchy_drop_targets(&catalog, &roots, true);
    assert_eq!(
        targets,
        [
            "public.a_child",
            "public.b_left",
            "public.c_right",
            "public.z_root"
        ]
    );
    assert!(blockers.is_empty());
    assert_eq!(roots, ["public.z_root", "public.z_root"]);
    assert!(!catalog.held.get());
}
#[test]
fn cyclic_loaded_metadata_terminates_at_a_sorted_fixed_point() {
    let catalog = catalog(&[
        ("a", &["b"], false),
        ("b", &["a"], false),
        ("unrelated", &[], false),
    ]);
    let (targets, blockers) = hierarchy_drop_targets(&catalog, &["public.b".into()], true);
    assert_eq!(targets, ["public.a", "public.b"]);
    assert!(blockers.is_empty());
    assert_eq!(catalog.reads.get(), 2);
    assert!(!catalog.held.get());
}
