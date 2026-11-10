//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn family(column_parent: bool) -> (Vec<Table>, Vec<CatalogIndexRow>) {
    let (mut tables, indexes) = fixture();
    tables[1].hierarchy.as_mut().unwrap().partition_spec = Some(PartitionSpec {
        strategy: PartitionStrategy::Range,
        keys: vec![Expr::Column("id".into())],
    });
    let mut child = table("partition", 6);
    let mut copied = tables[1].foreign_keys[0].clone();
    copied.catalog_identity = Some(identity(7));
    copied.name = Some("attached_name".into());
    child.foreign_keys.push(copied);
    let hierarchy = child.hierarchy.as_mut().unwrap();
    hierarchy.parents.push("public.child".into());
    hierarchy.partition_bound = Some(PartitionBound::Default);
    if column_parent {
        let original = tables[1].foreign_keys.remove(0);
        let mut reference: ForeignKeyRef =
            serde_json::from_str(r#"{"table":"public.parent","column":"id"}"#).unwrap();
        reference.catalog_identity = original.catalog_identity;
        reference.object_id = original.object_id;
        reference.referenced_index = original.referenced_index;
        tables[1].columns[0].references = Some(reference);
    }
    tables.push(child);
    (tables, indexes)
}

#[test]
fn native_constraints_require_partition_foreign_key_family_definitions() {
    for column_parent in [false, true] {
        for corruption in [
            "target",
            "local",
            "columns",
            "update",
            "delete",
            "match",
            "deferrable",
            "deferred",
            "enforced",
        ] {
            let (mut tables, mut indexes) = family(column_parent);
            accepts(&tables, &indexes, true);
            let other = table("other", 8);
            indexes.push(index(&other, 9));
            tables.push(other);
            let key = &mut tables[2].foreign_keys[0];
            match corruption {
                "target" => {
                    key.ref_table = "public.other".into();
                    key.referenced_index = Some([9; 16]);
                }
                "local" => key.local_columns[0] = "extra".into(),
                "columns" => {
                    key.ref_columns[0] = "extra".into();
                    let mut another = index(&tables[0], 10);
                    another.columns_json = r#"["extra"]"#.into();
                    indexes.push(another);
                    tables[2].foreign_keys[0].referenced_index = Some([10; 16]);
                }
                "update" => key.on_update = crate::ast::ForeignKeyAction::Cascade,
                "delete" => key.on_delete = crate::ast::ForeignKeyAction::Cascade,
                "match" => key.match_type = crate::ast::ForeignKeyMatch::Full,
                "deferrable" => key.deferrable = true,
                "deferred" => {
                    key.deferrable = true;
                    key.initially_deferred = true;
                }
                "enforced" => key.enforced = false,
                _ => unreachable!(),
            }
            accepts(&tables, &indexes, false);
        }
    }
}

#[test]
fn native_constraints_require_every_referencing_partition_family_member() {
    for column_parent in [false, true] {
        let (mut tables, indexes) = family(column_parent);
        accepts(&tables, &indexes, true);
        tables[2].foreign_keys.clear();
        accepts(&tables, &indexes, false);
    }
}

#[test]
fn native_constraints_require_one_declaring_owner_per_foreign_key_family() {
    let (mut tables, indexes) = family(false);
    let mut other = table("independent", 8);
    let mut key = tables[1].foreign_keys[0].clone();
    key.catalog_identity = Some(identity(9));
    other.foreign_keys.push(key);
    tables.push(other);
    accepts(&tables, &indexes, false);
}

#[test]
fn native_constraints_preserve_attached_foreign_key_catalog_choices() {
    let (mut tables, mut indexes) = family(false);
    for table in &mut tables[1..] {
        let key = &mut table.foreign_keys[0];
        key.local_columns.push("extra".into());
        key.ref_columns.push("extra".into());
        key.on_delete = crate::ast::ForeignKeyAction::SetNull;
    }
    tables[1].foreign_keys[0]
        .on_delete_set_columns
        .push("id".into());
    tables[2].foreign_keys[0]
        .on_delete_set_columns
        .push("extra".into());
    tables[2].foreign_keys[0].referenced_index = Some([9; 16]);
    indexes.push(index(&tables[0], 9));
    for index in &mut indexes {
        index.columns_json = r#"["id","extra"]"#.into();
    }
    accepts(&tables, &indexes, true);
}
