//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::cell::Cell;
use uqa_core::catalog_sequence::SequencePrivileges;
use uqa_sql::{
    ast::{RelationPersistence, SequenceDataType},
    catalog::{
        roles::{identity::RoleBinding, RoleReference},
        security::BoundSequenceSecurity,
    },
};

mod fixtures;
use fixtures::Fixture;

#[test]
fn relation_ownership_keeps_role_incarnations_and_uses_no_value_snapshot() {
    let mut fixture = Fixture::new();
    let relation = RelationIdentity::new("public", "ids");
    let error = fixture
        .context()
        .ensure_relation_owner(&relation, "sequence")
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42501"));
    let owner = fixture.retained.roles.roles["reader"].identity();
    Arc::make_mut(&mut fixture.retained.security)
        .get_mut(&relation)
        .unwrap()
        .role_owner = owner;
    fixture
        .context()
        .ensure_relation_owner(&relation, "sequence")
        .unwrap();
    Arc::make_mut(&mut fixture.retained.roles.roles)
        .get_mut("reader")
        .unwrap()
        .object_id = [9; 16];
    let error = fixture
        .context()
        .ensure_relation_owner(&relation, "sequence")
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("XX000"));
    assert!(error
        .to_string()
        .contains("missing or replaced incarnation"));
    assert_eq!(fixture.reads.get(), 0);
}

#[test]
fn sequence_oid_binding_and_comma_privileges_keep_one_authority_per_invocation() {
    let fixture = Fixture::new();
    let mut revoked = fixture.retained.clone();
    Arc::make_mut(&mut revoked.security)
        .values_mut()
        .next()
        .unwrap()
        .acl = Some(Vec::new());
    *fixture.after_read.borrow_mut() = Some(revoked);
    let context = fixture.context();
    let arguments = [
        Value::Int(sequence_relation_oid([7; 16])),
        Value::Str("SELECT, UPDATE".into()),
    ];
    assert_eq!(
        context.has_table_privilege_value(&arguments).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(fixture.reads.get(), 1);
    assert_eq!(
        context.has_table_privilege_value(&arguments).unwrap(),
        Value::Bool(false)
    );
    assert_eq!(fixture.reads.get(), 2);
    assert!(
        fixture
            .retained
            .security
            .values()
            .next()
            .unwrap()
            .acl
            .as_ref()
            .unwrap()[0]
            .privileges
            .update
    );
}

#[test]
fn sequence_name_inquiry_reads_roles_and_acl_after_resolution_without_rebinding_subject() {
    let fixture = Fixture::new();
    let mut replaced = fixture.retained.clone();
    Arc::make_mut(&mut replaced.roles.roles)
        .get_mut("reader")
        .unwrap()
        .object_id = [2; 16];
    Arc::make_mut(&mut replaced.security)
        .values_mut()
        .next()
        .unwrap()
        .acl
        .as_mut()
        .unwrap()[0]
        .role = Some(replaced.roles.roles["reader"].identity());
    *fixture.after_resolution.borrow_mut() = Some(replaced);
    let arguments = [
        Value::Str("reader".into()),
        Value::Str("ids".into()),
        Value::Str("last_value".into()),
        Value::Str("UPDATE".into()),
    ];
    assert_eq!(
        fixture
            .context()
            .has_column_privilege_value(&arguments)
            .unwrap(),
        Value::Bool(false)
    );
    assert_eq!(fixture.reads.get(), 1);
    assert_eq!(fixture.retained.roles.roles["reader"].object_id, [1; 16]);
}

#[test]
fn removed_sequences_do_not_reappear_from_the_statement_catalog() {
    let fixture = Fixture::new();
    {
        let mut current = fixture.current.borrow_mut();
        Arc::make_mut(&mut current.object_ids).clear();
        Arc::make_mut(&mut current.sequences).clear();
        Arc::make_mut(&mut current.security).clear();
        Arc::make_mut(&mut current.persistence).clear();
    }
    let context = fixture.context();
    assert_eq!(
        context
            .has_table_privilege_value(&[
                Value::Int(sequence_relation_oid([7; 16])),
                Value::Str("UPDATE".into())
            ])
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        context
            .has_table_privilege_value(&[Value::Str("ids".into()), Value::Str("UPDATE".into())])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
}

#[test]
fn strict_and_invalid_inquiry_arguments_do_not_refresh_or_capture_sequence_catalogs() {
    let fixture = Fixture::new();
    let context = fixture.context();
    assert_eq!(
        context
            .has_table_privilege_value(&[Value::Null, Value::Str("bad".into())])
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        context
            .has_column_privilege_value(&[Value::Null, Value::Int(0), Value::Str("bad".into())])
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        context
            .has_table_privilege_value(&[Value::Int(0), Value::Str("bad".into())])
            .unwrap_err()
            .sqlstate(),
        Some("22023")
    );
    assert_eq!(
        context
            .has_column_privilege_value(&[Value::Int(0), Value::Int(0), Value::Str("bad".into())])
            .unwrap_err()
            .sqlstate(),
        Some("22023")
    );
    assert!(matches!(
        context.has_table_privilege_value(&[]),
        Err(SQLError::BadArity { .. })
    ));
    assert_eq!(fixture.reads.get(), 0);
    assert_eq!(fixture.refreshes.get(), 0);
}
