//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn partitioned() -> (Vec<Table>, Vec<CatalogIndexRow>) {
    let (mut tables, indexes) = fixture();
    let specification = || PartitionSpec {
        strategy: PartitionStrategy::Range,
        keys: vec![Expr::Column("id".into())],
    };
    tables[0].hierarchy.as_mut().unwrap().partition_spec = Some(specification());
    let mut branch = table("branch", 6);
    let hierarchy = branch.hierarchy.as_mut().unwrap();
    hierarchy.parents.push("public.parent".into());
    hierarchy.partition_bound = Some(PartitionBound::Default);
    hierarchy.partition_spec = Some(specification());
    let mut leaf = table("leaf", 7);
    let hierarchy = leaf.hierarchy.as_mut().unwrap();
    hierarchy.parents.push("public.branch".into());
    hierarchy.partition_bound = Some(PartitionBound::Default);
    tables.extend([branch, leaf]);
    tables[1].foreign_keys[0].referenced_partitions = vec![
        crate::ast::ReferencedPartitionConstraint {
            partition: [6; 16],
            parent: None,
            name: "branch_reference".into(),
            catalog_identity: identity(10),
            validated: true,
        },
        crate::ast::ReferencedPartitionConstraint {
            partition: [7; 16],
            parent: Some([6; 16]),
            name: "leaf_reference".into(),
            catalog_identity: identity(11),
            validated: true,
        },
    ];
    (tables, indexes)
}

#[test]
fn native_constraints_require_complete_referenced_partition_membership() {
    for all in [false, true] {
        let (mut tables, indexes) = partitioned();
        accepts(&tables, &indexes, true);
        let derived = &mut tables[1].foreign_keys[0].referenced_partitions;
        if all {
            derived.clear();
        } else {
            derived.pop();
        }
        accepts(&tables, &indexes, false);
    }
}

#[test]
fn native_constraints_derive_partition_rows_only_for_the_declaring_foreign_key() {
    for column_parent in [false, true] {
        let (mut tables, indexes) = partitioned();
        let mut child = table("referencing_partition", 8);
        let mut copied = tables[1].foreign_keys[0].clone();
        copied.catalog_identity = Some(identity(12));
        copied.referenced_partitions.clear();
        child.foreign_keys.push(copied.clone());
        let hierarchy = child.hierarchy.as_mut().unwrap();
        hierarchy.parents.push("public.child".into());
        hierarchy.partition_bound = Some(PartitionBound::Default);
        hierarchy.partition_inherited_foreign_keys.push(copied);
        tables[1].hierarchy.as_mut().unwrap().partition_spec = Some(PartitionSpec {
            strategy: PartitionStrategy::Range,
            keys: vec![Expr::Column("id".into())],
        });
        if column_parent {
            let mut key: ForeignKeyRef =
                serde_json::from_str(r#"{"table":"public.parent","column":"id"}"#).unwrap();
            let original = tables[1].foreign_keys.remove(0);
            key.catalog_identity = original.catalog_identity;
            key.object_id = original.object_id;
            key.referenced_index = original.referenced_index;
            key.referenced_partitions = original.referenced_partitions;
            tables[1].columns[0].references = Some(key);
        }
        tables.push(child);
        accepts(&tables, &indexes, true);
        let mut derived = if column_parent {
            tables[1].columns[0]
                .references
                .as_ref()
                .unwrap()
                .referenced_partitions
                .clone()
        } else {
            tables[1].foreign_keys[0].referenced_partitions.clone()
        };
        for (id, constraint) in [13, 14].into_iter().zip(&mut derived) {
            constraint.catalog_identity = identity(id);
        }
        tables.last_mut().unwrap().foreign_keys[0].referenced_partitions = derived;
        accepts(&tables, &indexes, false);
    }
}

#[test]
fn native_constraints_select_only_the_original_referenced_partition_tree() {
    let (mut tables, indexes) = partitioned();
    let mut parent = table("other_parent", 20);
    parent.hierarchy.as_mut().unwrap().partition_spec = Some(PartitionSpec {
        strategy: PartitionStrategy::Range,
        keys: vec![Expr::Column("id".into())],
    });
    let mut child = table("other_leaf", 21);
    let hierarchy = child.hierarchy.as_mut().unwrap();
    hierarchy.parents.push("public.other_parent".into());
    hierarchy.partition_bound = Some(PartitionBound::Default);
    tables.extend([parent, child]);
    accepts(&tables, &indexes, true);
    tables[1].foreign_keys[0].referenced_partitions.push(
        crate::ast::ReferencedPartitionConstraint {
            partition: [21; 16],
            parent: None,
            name: "unrelated_reference".into(),
            catalog_identity: identity(22),
            validated: true,
        },
    );
    accepts(&tables, &indexes, false);
}
