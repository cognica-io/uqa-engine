//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn declaration(inline: bool) -> (Vec<ColumnDef>, TableConstraintSet) {
    let sql = if inline {
        "CREATE TABLE t(v integer CONSTRAINT fk REFERENCES p(id))"
    } else {
        "CREATE TABLE t(v integer, CONSTRAINT fk FOREIGN KEY(v) REFERENCES p(id))"
    };
    let crate::Statement::CreateTable(table) = crate::compile(sql).unwrap().remove(0) else {
        panic!("table declaration")
    };
    let mut columns = table.columns;
    let mut constraints = TableConstraintSet {
        foreign_keys: table.foreign_keys,
        ..TableConstraintSet::default()
    };
    if inline {
        columns[0].references.as_mut().unwrap().object_id = Some([1; 16]);
    } else {
        constraints.foreign_keys[0].object_id = Some([1; 16]);
    }
    (columns, constraints)
}

#[test]
fn foreign_key_identity_survives_renames_and_metadata_reordering() {
    for inline in [true, false] {
        let (mut columns, mut constraints) = declaration(inline);
        assert_eq!(
            ForeignKeyTarget::by_name(&columns, &constraints, "fk")
                .unwrap()
                .unwrap()
                .object_id,
            [1; 16]
        );
        if inline {
            let reference = columns[0].references.as_mut().unwrap();
            reference.name = Some("renamed".into());
            reference.table = "original_parent".into();
            let mut other = columns[0].clone();
            other.references = None;
            columns.insert(0, other);
        } else {
            constraints.foreign_keys[0].name = Some("renamed".into());
            constraints.foreign_keys[0].ref_table = "original_parent".into();
            let mut other = constraints.foreign_keys[0].clone();
            other.name = Some("other".into());
            other.object_id = Some([2; 16]);
            constraints.foreign_keys.insert(0, other);
        }
        let target = ForeignKeyTarget::by_id(&columns, &constraints, [1; 16])
            .unwrap()
            .unwrap();
        assert_eq!(target.name, "renamed");
        assert_eq!(target.referenced_table, "original_parent");
        assert_eq!(
            target.location,
            if inline {
                ConstraintLocation::ColumnForeignKey(1)
            } else {
                ConstraintLocation::TableForeignKey(1)
            }
        );
    }
}

#[test]
fn a_reused_foreign_key_name_never_substitutes_for_the_original_identity() {
    for inline in [true, false] {
        let (mut columns, mut constraints) = declaration(inline);
        if inline {
            columns[0].references.as_mut().unwrap().object_id = Some([2; 16]);
        } else {
            constraints.foreign_keys[0].object_id = Some([2; 16]);
        }
        assert!(ForeignKeyTarget::by_id(&columns, &constraints, [1; 16])
            .unwrap()
            .is_none());
        assert_eq!(
            ForeignKeyTarget::by_name(&columns, &constraints, "fk")
                .unwrap()
                .unwrap()
                .object_id,
            [2; 16]
        );
    }
}

#[test]
fn only_materialized_foreign_keys_can_be_retained() {
    for inline in [true, false] {
        let (mut columns, mut constraints) = declaration(inline);
        if inline {
            columns[0].references.as_mut().unwrap().object_id = None;
        } else {
            constraints.foreign_keys[0].object_id = None;
        }
        assert!(ForeignKeyTarget::by_name(&columns, &constraints, "fk").is_err());
        assert!(ForeignKeyTarget::by_name(&columns, &constraints, "missing")
            .unwrap()
            .is_none());
        columns[0].not_null = true;
        columns[0].not_null_name = Some("nn".into());
        assert!(ForeignKeyTarget::by_name(&columns, &constraints, "nn")
            .unwrap()
            .is_none());
    }
}

#[test]
fn removing_a_renamed_foreign_key_retires_only_its_local_attachment_provenance() {
    let (mut columns, mut constraints) = declaration(false);
    let mut other = constraints.foreign_keys[0].clone();
    other.name = Some("other".into());
    other.object_id = Some([2; 16]);
    constraints.foreign_keys.push(other);
    constraints.hierarchy.partition_inherited_foreign_keys = constraints.foreign_keys.clone();
    constraints.foreign_keys[0].name = Some("renamed".into());
    assert!(remove_foreign_key(&mut columns, &mut constraints, [1; 16]).unwrap());
    assert_eq!(constraints.foreign_keys.len(), 1);
    assert_eq!(
        constraints.foreign_keys,
        constraints.hierarchy.partition_inherited_foreign_keys
    );
    assert_eq!(constraints.foreign_keys[0].object_id, Some([2; 16]));
    assert!(!remove_foreign_key(&mut columns, &mut constraints, [1; 16]).unwrap());
}
