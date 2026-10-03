//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ForeignKeyAction, ForeignKeyMatch};

fn foreign_key(name: &str) -> ForeignKey {
    ForeignKey {
        referenced_index: None,
        referenced_key: None,
        name: Some(name.into()),
        object_id: Some([name.len() as u8; 16]),
        catalog_identity: None,
        local_columns: vec!["a".into()],
        ref_table: "public.pk".into(),
        ref_columns: vec!["a".into()],
        on_update: ForeignKeyAction::NoAction,
        on_delete: ForeignKeyAction::NoAction,
        on_delete_set_columns: Vec::new(),
        match_type: ForeignKeyMatch::Simple,
        enforced: true,
        validated: true,
        deferrable: false,
        initially_deferred: false,
        period: false,
    }
}

#[test]
fn the_first_equivalent_foreign_key_by_name_attaches() {
    let parent = foreign_key("fk_a_fkey");
    let mut deferrable = foreign_key("a_deferrable");
    deferrable.deferrable = true;
    let mut other_columns = foreign_key("b_other");
    other_columns.local_columns = vec!["b".into()];
    let candidates = vec![
        foreign_key("z_late"),
        deferrable,
        other_columns,
        foreign_key("c_early"),
    ];
    let attached = attachable_foreign_key("public.fk1", &parent, &candidates)
        .unwrap()
        .unwrap();
    assert_eq!(attached.name.as_deref(), Some("c_early"));
    let mut cascade = foreign_key("c_cascade");
    cascade.on_delete = ForeignKeyAction::Cascade;
    assert!(attachable_foreign_key("public.fk1", &parent, &[cascade])
        .unwrap()
        .is_none());
}

#[test]
fn a_foreign_key_that_differs_only_in_enforceability_is_an_error() {
    let parent = foreign_key("fk_a_fkey");
    let mut candidate = foreign_key("fk1_own");
    candidate.enforced = false;
    candidate.validated = false;
    let error = attachable_foreign_key("public.fk1", &parent, &[candidate]).unwrap_err();
    match error {
        SQLError::Diagnostic {
            sqlstate, message, ..
        } => {
            assert_eq!(sqlstate, "42P16");
            assert_eq!(
                message,
                "constraint \"fk_a_fkey\" enforceability conflicts with constraint \"fk1_own\" on relation \"fk1\""
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[test]
fn a_copy_keeps_the_parent_name_unless_the_partition_uses_it() {
    let parent = foreign_key("fk_a_fkey");
    let mut schema = BTreeSet::from(["fk_a_fkey".to_string(), "fk_a_fkey_1".to_string()]);
    let copy = partition_foreign_key_copy(&parent, &BTreeSet::new(), &mut schema).unwrap();
    assert_eq!(copy.name.as_deref(), Some("fk_a_fkey"));
    assert_eq!(copy.object_id, parent.object_id);
    let copy = partition_foreign_key_copy(
        &parent,
        &BTreeSet::from(["fk_a_fkey".to_string()]),
        &mut schema,
    )
    .unwrap();
    assert_eq!(copy.name.as_deref(), Some("fk_a_fkey_2"));
    assert!(schema.contains("fk_a_fkey_2"));
}
