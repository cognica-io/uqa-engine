//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{PartitionBound, PartitionSpec, PartitionStrategy, TableKeyConstraintKind},
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
};
use uqa_core::catalog_identity::CatalogObjectIdentity;

struct Table {
    name: RelationIdentity,
    id: [u8; 16],
    columns: Option<Vec<ColumnDef>>,
    keys: Vec<TableKeyConstraint>,
    hierarchy: TableHierarchy,
}

fn identity(id: u8) -> CatalogObjectIdentity {
    CatalogObjectIdentity {
        object_id: [id; 16],
        oid: 20_000 + i64::from(id),
    }
}

fn table(name: &str, id: u8) -> Table {
    Table {
        name: RelationIdentity::new("public", name),
        id: [id; 16],
        columns: Some(
            ["value", "extra"]
                .into_iter()
                .map(|name| ColumnDef::nullable(name, crate::ColumnType::Integer))
                .collect(),
        ),
        keys: vec![],
        hierarchy: TableHierarchy::default(),
    }
}

fn row(table: &Table, id: u8) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new(&table.name.schema, format!("index_{id}")),
        table_name: table.name.qualified_name(),
        index_type: "btree".into(),
        columns_json: r#"["value"]"#.into(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                catalog: Some(IndexCatalogIdentity {
                    identity: identity(id),
                    table_object_id: table.id,
                    physical_key: format!("original:{id}"),
                }),
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    }
}

fn alter(row: &mut CatalogIndexRow, change: impl FnOnce(&mut IndexDefinition)) {
    let mut definition =
        super::super::stored::index_definition(row.definition_json.as_deref()).unwrap();
    change(&mut definition);
    row.definition_json = Some(serde_json::to_string(&definition).unwrap());
}

fn check(
    tables: &[Table],
    rows: &[CatalogIndexRow],
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<(), SQLError> {
    validate(
        tables.iter().map(|table| NativeIndexTable {
            name: &table.name,
            object_id: table.id,
            columns: table.columns.as_deref(),
            keys: &table.keys,
            hierarchy: &table.hierarchy,
        }),
        rows.iter(),
        memory,
        token,
    )
}

fn accepts(tables: &[Table], rows: &[CatalogIndexRow], expected: bool) {
    let memory = MemoryBudget::new(256 << 10);
    let token = CancellationToken::new();
    let result = check(tables, rows, &memory, &token);
    assert_eq!(result.is_ok(), expected, "{result:?}");
    assert_eq!(memory.used(), 0);
}

fn owned_key() -> (Table, CatalogIndexRow) {
    let mut table = table("items", 1);
    let mut row = row(&table, 2);
    table.keys.push(TableKeyConstraint {
        catalog_identity: Some(identity(3)),
        index_identity: Some(identity(2)),
        name: Some("constraint".into()),
        kind: TableKeyConstraintKind::Unique,
        columns: vec!["value".into()],
        included_columns: vec!["extra".into(), "value".into(), "extra".into()],
        nulls_not_distinct: true,
        without_overlaps: false,
    });
    alter(&mut row, |definition| {
        definition.relationships.owning_constraint = Some([3; 16]);
        definition.unique = true;
        definition.nulls_not_distinct = true;
        definition
            .included_columns
            .clone_from(&table.keys[0].included_columns);
    });
    (table, row)
}

#[test]
fn native_index_columns_close_over_declared_layouts_before_optional_addresses() {
    for addressed in [false, true] {
        for included in [false, true] {
            let mut table = table("items", 1);
            let mut row = row(&table, 2);
            alter(&mut row, |definition| {
                if !addressed {
                    definition.catalog = None;
                }
                if included {
                    definition.included_columns = vec!["missing".into()];
                }
            });
            if !included {
                row.columns_json = r#"["missing"]"#.into();
            }
            accepts(
                std::slice::from_ref(&table),
                std::slice::from_ref(&row),
                false,
            );
            table
                .columns
                .as_mut()
                .unwrap()
                .push(ColumnDef::nullable("missing", crate::ColumnType::Text));
            accepts(
                std::slice::from_ref(&table),
                std::slice::from_ref(&row),
                true,
            );
            table.columns = Some(vec![]);
            accepts(
                std::slice::from_ref(&table),
                std::slice::from_ref(&row),
                false,
            );
            table.columns = None;
            accepts(&[table], &[row], true);
        }
    }
}

#[test]
fn native_index_column_admission_preserves_quoted_names_and_repeated_include_fields() {
    let mut table = table("items", 1);
    let mut row = row(&table, 2);
    let names = vec!["quoted name".to_owned(), "한글\\\"".repeat(2000)];
    table.columns = Some(
        names
            .iter()
            .map(|name| ColumnDef::nullable(name, crate::ColumnType::Integer))
            .collect(),
    );
    row.columns_json = serde_json::to_string(&names).unwrap();
    alter(&mut row, |definition| {
        definition.included_columns = vec![names[1].clone(), names[0].clone(), names[1].clone()];
    });
    accepts(
        std::slice::from_ref(&table),
        std::slice::from_ref(&row),
        true,
    );
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    assert_eq!(
        check(
            std::slice::from_ref(&table),
            std::slice::from_ref(&row),
            &memory,
            &token
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    table
        .columns
        .as_mut()
        .unwrap()
        .push(ColumnDef::nullable(&names[0], crate::ColumnType::Integer));
    accepts(&[table], &[row], false);
}

#[test]
fn native_index_references_preserve_renames_legacy_addresses_and_included_duplicates() {
    let (mut table, mut row) = owned_key();
    accepts(
        std::slice::from_ref(&table),
        std::slice::from_ref(&row),
        true,
    );
    table.name = RelationIdentity::new("quoted schema", "renamed table");
    row.table_name = table.name.qualified_name();
    row.relation = RelationIdentity::new("quoted schema", "renamed index");
    accepts(
        std::slice::from_ref(&table),
        std::slice::from_ref(&row),
        true,
    );
    table.keys.clear();
    row.definition_json = None;
    accepts(&[table], &[row], true);
}

#[test]
fn native_index_references_reject_missing_replaced_and_duplicate_targets() {
    let table = table("items", 1);
    let original = row(&table, 2);
    accepts(&[], std::slice::from_ref(&original), false);
    accepts(
        std::slice::from_ref(&table),
        &[original.clone(), original.clone()],
        false,
    );
    for shape in 0..5 {
        let mut row = original.clone();
        alter(&mut row, |definition| {
            let catalog = definition.catalog.as_mut().unwrap();
            match shape {
                0 => catalog.table_object_id = [8; 16],
                1 => catalog.identity.object_id = [0; 16],
                2 => catalog.identity.oid = i64::from(u32::MAX) + 1,
                3 => catalog.physical_key = String::new(),
                _ => catalog.physical_key = "bad\0key".into(),
            }
        });
        accepts(std::slice::from_ref(&table), &[row], false);
    }
    for same_oid in [false, true] {
        let mut second = row(&table, 4);
        alter(&mut second, |definition| {
            let catalog = definition.catalog.as_mut().unwrap();
            if same_oid {
                catalog.identity.oid = identity(2).oid;
            } else {
                catalog.physical_key = "original:2".into();
            }
        });
        accepts(
            std::slice::from_ref(&table),
            &[original.clone(), second],
            false,
        );
    }
}

#[test]
fn native_index_references_require_bidirectional_constraint_ownership() {
    for shape in 0..10 {
        let (mut table, mut row) = owned_key();
        match shape {
            0 => table.keys.clear(),
            1 => table.keys[0].index_identity = Some(identity(9)),
            2 => table.keys[0].columns.push("missing".into()),
            3 => table.keys[0].included_columns.pop().map(|_| ()).unwrap(),
            4 => alter(&mut row, |definition| definition.unique = false),
            5 => alter(&mut row, |definition| definition.nulls_not_distinct = false),
            6 => alter(&mut row, |definition| {
                definition.relationships.owning_constraint = None;
            }),
            7 => alter(&mut row, |definition| definition.catalog = None),
            8 => table.keys[0].catalog_identity = None,
            _ => table.keys.push(table.keys[0].clone()),
        }
        accepts(&[table], &[row], false);
    }
    let (table, _) = owned_key();
    accepts(&[table], &[], false);
    let (mut table, row) = owned_key();
    table.keys[0].index_identity = None;
    let mut second = row.clone();
    second.relation.name = "another".into();
    alter(&mut second, |definition| {
        definition.catalog.as_mut().unwrap().identity = identity(4);
        definition.catalog.as_mut().unwrap().physical_key = "another".into();
    });
    accepts(&[table], &[row, second], false);
}

fn partition_tables() -> Vec<Table> {
    let mut parent = table("parent", 1);
    parent.hierarchy.partition_spec = Some(PartitionSpec {
        strategy: PartitionStrategy::List,
        keys: vec![],
    });
    let mut child = table("child", 4);
    child.hierarchy.parents.push(parent.name.qualified_name());
    child.hierarchy.partition_bound = Some(PartitionBound::Default);
    vec![parent, child]
}

#[test]
fn native_index_references_follow_original_partition_parent_edges() {
    for shape in 0..5 {
        let mut tables = partition_tables();
        let parent = row(&tables[0], 2);
        let mut child = row(&tables[1], 5);
        alter(&mut child, |definition| {
            definition.relationships.parent_index = Some([2; 16]);
        });
        match shape {
            0 => {}
            1 => alter(&mut child, |definition| {
                definition.relationships.parent_index = Some([8; 16]);
            }),
            2 => tables[1].hierarchy.partition_bound = None,
            3 => tables[1].hierarchy.parents[0] = "public.other".into(),
            _ => tables[0].hierarchy.partition_spec = None,
        }
        accepts(&tables, &[parent, child], shape == 0);
    }
}

#[test]
fn native_index_references_reject_cycles_without_recursive_traversal() {
    let mut tables = partition_tables();
    let child_name = tables[1].name.qualified_name();
    tables[0].hierarchy.parents.push(child_name);
    tables[0].hierarchy.partition_bound = Some(PartitionBound::Default);
    tables[1].hierarchy.partition_spec = tables[0].hierarchy.partition_spec.clone();
    let mut parent = row(&tables[0], 2);
    let mut child = row(&tables[1], 5);
    alter(&mut parent, |definition| {
        definition.relationships.parent_index = Some([5; 16]);
    });
    alter(&mut child, |definition| {
        definition.relationships.parent_index = Some([2; 16]);
    });
    accepts(&tables, &[parent, child], false);
}

#[test]
fn native_index_references_release_workspace_on_quota_failure_and_cancellation() {
    let (table, row) = owned_key();
    for limit in [0, 1, 128, 256, 1024, 4096, 64 << 10] {
        let memory = MemoryBudget::new(limit);
        let token = CancellationToken::new();
        let result = check(
            std::slice::from_ref(&table),
            std::slice::from_ref(&row),
            &memory,
            &token,
        );
        if limit == 0 {
            assert!(result.is_err());
        }
        if limit == 64 << 10 {
            result.unwrap();
        }
        assert_eq!(memory.used(), 0);
    }
    let memory = MemoryBudget::new(64 << 10);
    let token = CancellationToken::new();
    token.cancel();
    assert!(check(&[table], &[row], &memory, &token).is_err());
    assert_eq!(memory.used(), 0);
}
