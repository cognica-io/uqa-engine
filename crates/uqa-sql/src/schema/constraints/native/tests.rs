//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnType, ForeignKeyRef, PartitionBound, PartitionSpec, PartitionStrategy},
    catalog::index::{IndexCatalogIdentity, IndexDefinition},
};
use uqa_core::catalog_identity::CatalogObjectIdentity;

mod identities;

struct Table {
    name: RelationIdentity,
    id: [u8; 16],
    columns: Vec<ColumnDef>,
    checks: Vec<TableCheck>,
    keys: Vec<TableKeyConstraint>,
    foreign_keys: Vec<ForeignKey>,
    hierarchy: Option<TableHierarchy>,
}

fn table(name: &str, id: u8) -> Table {
    Table {
        name: RelationIdentity::new("public", name),
        id: [id; 16],
        columns: vec![
            ColumnDef::nullable("id", ColumnType::Integer),
            ColumnDef::nullable("extra", ColumnType::Integer),
        ],
        checks: vec![],
        keys: vec![],
        foreign_keys: vec![],
        hierarchy: Some(TableHierarchy::default()),
    }
}

fn identity(id: u8) -> CatalogObjectIdentity {
    CatalogObjectIdentity {
        object_id: [id; 16],
        oid: 20_000 + i64::from(id),
    }
}

fn index(table: &Table, id: u8) -> CatalogIndexRow {
    CatalogIndexRow {
        relation: RelationIdentity::new("public", format!("key_{id}")),
        table_name: table.name.qualified_name(),
        index_type: "btree".into(),
        columns_json: r#"["id"]"#.into(),
        parameters_json: "{}".into(),
        definition_json: Some(
            serde_json::to_string(&IndexDefinition {
                unique: true,
                catalog: Some(IndexCatalogIdentity {
                    identity: identity(id),
                    table_object_id: table.id,
                    physical_key: "original".into(),
                }),
                ..IndexDefinition::default()
            })
            .unwrap(),
        ),
    }
}

fn reference() -> ForeignKey {
    let mut key: ForeignKey = serde_json::from_str(
        r#"{"local_columns":["id"],"ref_table":"public.parent","ref_columns":["id"]}"#,
    )
    .unwrap();
    key.referenced_index = Some([3; 16]);
    key.catalog_identity = Some(identity(4));
    key.object_id = Some([5; 16]);
    key
}

fn check(
    tables: &[Table],
    indexes: &[CatalogIndexRow],
    memory: &MemoryBudget,
    token: &CancellationToken,
) -> Result<(), SQLError> {
    validate(
        tables.iter().map(|table| NativeConstraintTable {
            name: &table.name,
            object_id: table.id,
            columns: &table.columns,
            checks: &table.checks,
            keys: &table.keys,
            foreign_keys: &table.foreign_keys,
            hierarchy: table.hierarchy.as_ref(),
        }),
        indexes.iter(),
        std::iter::empty(),
        memory,
        token,
    )
}

fn accepts(tables: &[Table], indexes: &[CatalogIndexRow], expected: bool) {
    let memory = MemoryBudget::new(256 << 10);
    let result = check(tables, indexes, &memory, &CancellationToken::new());
    assert_eq!(result.is_ok(), expected, "{result:?}");
    assert_eq!(memory.used(), 0);
}

fn fixture() -> (Vec<Table>, Vec<CatalogIndexRow>) {
    let mut tables = vec![table("parent", 1), table("child", 2)];
    tables[1].foreign_keys.push(reference());
    let indexes = vec![index(&tables[0], 3)];
    (tables, indexes)
}

#[test]
fn native_constraints_require_existing_foreign_key_tables_and_live_columns() {
    for corruption in [
        "table",
        "local",
        "target",
        "arity",
        "duplicate",
        "type",
        "foreign_target",
    ] {
        let (mut tables, indexes) = fixture();
        match corruption {
            "table" => tables[1].foreign_keys[0].ref_table = "public.absent".into(),
            "local" => tables[1].foreign_keys[0].local_columns[0] = "gone".into(),
            "target" => {
                tables[0].columns.remove(0);
            }
            "arity" => {
                tables[1].foreign_keys[0].ref_columns.push("extra".into());
            }
            "duplicate" => {
                tables[1].foreign_keys[0].local_columns.push("id".into());
                tables[1].foreign_keys[0].ref_columns.push("id".into());
            }
            "type" => tables[1].columns[0].ty = ColumnType::Boolean,
            "foreign_target" => tables[0].hierarchy = None,
            _ => unreachable!(),
        }
        accepts(&tables, &indexes, false);
    }
}

#[test]
fn native_constraints_preserve_selected_index_identity_and_reordered_column_keys() {
    let (mut tables, mut indexes) = fixture();
    tables[1].foreign_keys[0].local_columns.push("extra".into());
    tables[1].foreign_keys[0]
        .ref_columns
        .insert(0, "extra".into());
    tables[1].foreign_keys[0].referenced_key = Some("old_name".into());
    indexes[0].columns_json = r#"["id","extra"]"#.into();
    indexes[0].relation.name = "renamed".into();
    accepts(&tables, &indexes, true);
    for corruption in [
        "missing",
        "replaced",
        "nonunique",
        "partial",
        "wrong_table",
        "wrong_columns",
        "duplicate",
        "expression",
    ] {
        let (tables, mut indexes) = fixture();
        let mut definition =
            crate::catalog::index::stored::index_definition(indexes[0].definition_json.as_deref())
                .unwrap();
        match corruption {
            "missing" => {
                indexes.clear();
            }
            "replaced" => definition.catalog.as_mut().unwrap().identity = identity(8),
            "nonunique" => definition.unique = false,
            "partial" => {
                definition.predicate = Some(Box::new(Expr::Literal(uqa_core::Value::Bool(true))));
            }
            "wrong_table" => definition.catalog.as_mut().unwrap().table_object_id = [2; 16],
            "wrong_columns" => indexes[0].columns_json = r#"["extra"]"#.into(),
            "duplicate" => indexes[0].columns_json = r#"["id","id"]"#.into(),
            "expression" => indexes[0].columns_json = r#"[{"Column":"id"}]"#.into(),
            _ => unreachable!(),
        }
        if let Some(index) = indexes.first_mut() {
            index.definition_json = Some(serde_json::to_string(&definition).unwrap());
        }
        accepts(&tables, &indexes, false);
    }
}

#[test]
fn native_constraints_validate_column_references_without_rebinding_missing_indexes() {
    let (mut tables, indexes) = fixture();
    tables[1].foreign_keys.clear();
    let mut reference: ForeignKeyRef =
        serde_json::from_str(r#"{"table":"public.parent","column":"id"}"#).unwrap();
    reference.referenced_index = Some([3; 16]);
    tables[1].columns[0].references = Some(reference);
    accepts(&tables, &indexes, true);
    tables[1].columns[0]
        .references
        .as_mut()
        .unwrap()
        .referenced_index = None;
    accepts(&tables, &indexes, false);
}

#[test]
fn native_constraints_preserve_repeated_local_and_action_columns() {
    // PostgreSQL 18.4 accepts both lists; only the referenced columns must be distinct.
    let (mut tables, mut indexes) = fixture();
    let key = &mut tables[1].foreign_keys[0];
    key.local_columns.push("id".into());
    key.ref_columns.push("extra".into());
    key.on_delete = crate::ast::ForeignKeyAction::SetNull;
    key.on_delete_set_columns = vec!["id".into(), "id".into()];
    indexes[0].columns_json = r#"["id","extra"]"#.into();
    accepts(&tables, &indexes, true);
    tables[1].foreign_keys[0]
        .on_delete_set_columns
        .push("extra".into());
    accepts(&tables, &indexes, false);
}

#[test]
fn native_constraints_require_original_period_key_ownership() {
    let (mut tables, mut indexes) = fixture();
    for table in &mut tables {
        table.columns[1].ty = ColumnType::Range(crate::ast::RangeSubtype::Integer);
    }
    let key = &mut tables[1].foreign_keys[0];
    key.local_columns.push("extra".into());
    key.ref_columns.push("extra".into());
    key.period = true;
    let mut constraint: TableKeyConstraint = serde_json::from_str(
        r#"{"kind":"Unique","columns":["id","extra"],"without_overlaps":true}"#,
    )
    .unwrap();
    constraint.catalog_identity = Some(identity(9));
    constraint.index_identity = Some(identity(3));
    tables[0].keys.push(constraint);
    indexes[0].columns_json = r#"["id","extra"]"#.into();
    let mut definition =
        crate::catalog::index::stored::index_definition(indexes[0].definition_json.as_deref())
            .unwrap();
    definition.relationships.owning_constraint = Some([9; 16]);
    indexes[0].definition_json = Some(serde_json::to_string(&definition).unwrap());
    accepts(&tables, &indexes, true);
    definition.relationships.owning_constraint = None;
    tables[0].keys[0].catalog_identity = None;
    indexes[0].definition_json = Some(serde_json::to_string(&definition).unwrap());
    accepts(&tables, &indexes, false);
}

#[test]
fn native_constraints_check_key_membership_and_borrowed_check_columns_without_evaluation() {
    let mut tables = vec![table("items", 1)];
    let key: TableKeyConstraint = serde_json::from_str(
        r#"{"kind":"Unique","columns":["id"],"included_columns":["id","extra","extra"]}"#,
    )
    .unwrap();
    tables[0].keys.push(key);
    tables[0].columns[0].check = Some(Expr::Column("extra".into()));
    tables[0].columns[0].check_validated = false;
    tables[0].columns[0].check_enforced = false;
    accepts(&tables, &[], true);
    tables[0].columns[0].check = Some(Expr::Column("missing".into()));
    accepts(&tables, &[], false);
    tables[0].columns[0].check = Some(Expr::Column("tableoid".into()));
    accepts(&tables, &[], true);
    tables[0].columns[0].check = Some(Expr::Literal(uqa_core::Value::Bool(false)));
    accepts(&tables, &[], true);
    tables[0].keys[0].included_columns.push("missing".into());
    accepts(&tables, &[], false);
    tables[0].keys.clear();
    tables[0].hierarchy = None;
    accepts(&tables, &[], true);
}

#[test]
fn native_constraints_require_original_referenced_partition_edges() {
    let (mut tables, indexes) = fixture();
    tables[0].hierarchy.as_mut().unwrap().partition_spec = Some(PartitionSpec {
        strategy: PartitionStrategy::Range,
        keys: vec![Expr::Column("id".into())],
    });
    let mut child = table("part", 6);
    child
        .hierarchy
        .as_mut()
        .unwrap()
        .parents
        .push("public.parent".into());
    child.hierarchy.as_mut().unwrap().partition_bound = Some(PartitionBound::Default);
    tables.push(child);
    tables[1].foreign_keys[0].referenced_partitions.push(
        crate::ast::ReferencedPartitionConstraint {
            partition: [6; 16],
            parent: None,
            name: "derived".into(),
            catalog_identity: identity(7),
            validated: false,
        },
    );
    accepts(&tables, &indexes, true);
    tables[2].columns[0].not_null = true;
    tables[2].columns[0].not_null_identity = Some(identity(7));
    accepts(&tables, &indexes, false);
    tables[2].columns[0].not_null = false;
    tables[2].columns[0].not_null_identity = None;
    tables[1].foreign_keys[0].referenced_partitions[0].parent = Some([6; 16]);
    accepts(&tables, &indexes, false);
    tables[1].foreign_keys[0].referenced_partitions[0].parent = None;
    tables[2].hierarchy.as_mut().unwrap().parents[0] = "public.child".into();
    accepts(&tables, &indexes, false);
    tables.pop();
    accepts(&tables, &indexes, false);
}

#[test]
fn native_constraints_release_all_workspace_on_quota_failure_and_cancellation() {
    let (tables, indexes) = fixture();
    let memory = MemoryBudget::new(1);
    assert_eq!(
        check(&tables, &indexes, &memory, &CancellationToken::new())
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
    let memory = MemoryBudget::new(256 << 10);
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(
        check(&tables, &indexes, &memory, &token)
            .unwrap_err()
            .sqlstate(),
        Some("57014")
    );
    assert_eq!(memory.used(), 0);
}

#[test]
fn native_foreign_key_type_checks_preserve_directional_coercion_and_admission() {
    let memory = MemoryBudget::new(4096);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let result = crate::type_resolution::foreign_key_operand_type_with_control(
        &ColumnType::SmallInteger,
        &ColumnType::BigInteger,
        &control,
    )
    .unwrap();
    assert_eq!(
        *result,
        crate::type_resolution::foreign_key_operand_type(
            &ColumnType::SmallInteger,
            &ColumnType::BigInteger
        )
        .unwrap()
    );
    drop(result);
    assert_eq!(memory.used(), 0);
    let memory = MemoryBudget::new(0);
    let control = ProductionControl::new(&memory, &token, &token);
    assert_eq!(
        crate::type_resolution::foreign_key_operand_type_with_control(
            &ColumnType::Integer,
            &ColumnType::Integer,
            &control
        )
        .unwrap_err()
        .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), 0);
}
