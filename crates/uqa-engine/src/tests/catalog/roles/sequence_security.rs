//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence authority retains role identities through snapshots, publication and initial migration.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::{catalog_sequence::LegacySequenceSecurity, Value};
use uqa_sql::catalog::security::BoundSequenceSecurity;
use uqa_storage::{CatalogFacade, SequenceSecurityRow};

fn setup(engine: &Engine) {
    sql(
        engine,
        "CREATE ROLE reader; CREATE ROLE other; CREATE SEQUENCE secured; CREATE SEQUENCE another",
    );
}

fn permitted(engine: &Engine, role: &str) -> bool {
    sql(
        engine,
        &format!("SELECT has_sequence_privilege('{role}', 'secured', 'USAGE') AS permitted"),
    )
    .rows[0]["permitted"]
        == Value::Bool(true)
}

fn stored(catalog: &dyn CatalogFacade, name: &str) -> uqa_storage::SequenceRow {
    catalog
        .load_sequence_rows()
        .unwrap()
        .into_iter()
        .find(|row| row.relation.name == name)
        .unwrap()
}

fn replace(catalog: &dyn CatalogFacade, name: &str, security: SequenceSecurityRow) {
    let mut row = stored(catalog, name);
    row.security = security;
    assert!(catalog.replace_sequence_row(&row).unwrap());
}

#[test]
fn sequence_owner_and_delegated_grants_keep_identity_after_every_name_is_reused() {
    let engine = Engine::new();
    setup(&engine);
    sql(&engine, "CREATE ROLE object_owner; CREATE ROLE delegate; GRANT CREATE ON SCHEMA public TO object_owner; ALTER SEQUENCE secured OWNER TO object_owner; SET ROLE object_owner; GRANT USAGE ON SEQUENCE secured TO delegate WITH GRANT OPTION; SET ROLE delegate; GRANT USAGE ON SEQUENCE secured TO reader; RESET ROLE; BEGIN; SAVEPOINT original");
    let owner = engine.durable.roles.read()["object_owner"].identity();
    // Test stored incarnations independently of SQL's pending role-rename command.
    {
        let mut roles = engine.durable.roles.write();
        for name in ["object_owner", "delegate", "reader"] {
            let mut role = roles.remove(name).unwrap();
            role.name = format!("renamed_{name}");
            roles.insert(role.name.clone(), role);
        }
    }
    engine.note_catalog_registry_changed();
    sql(
        &engine,
        "CREATE ROLE object_owner; CREATE ROLE delegate; CREATE ROLE reader",
    );
    assert!(permitted(&engine, "renamed_reader"));
    assert!(!permitted(&engine, "reader"));
    assert!(!permitted(&engine, "object_owner"));
    let projected = sql(
        &engine,
        "SELECT relowner, relacl FROM pg_class WHERE relname = 'secured'",
    );
    assert_eq!(projected.rows[0]["relowner"], Value::Int(owner.oid));
    assert!(format!("{:?}", projected.rows[0]["relacl"]).contains("renamed_delegate"));
    assert_eq!(
        sql(
            &engine,
            "SELECT sequenceowner FROM pg_sequences WHERE sequencename = 'secured'"
        )
        .rows[0]["sequenceowner"],
        Value::Str("renamed_object_owner".into())
    );
    sql(&engine, "SAVEPOINT dependency_check");
    error(&engine, "DROP ROLE renamed_reader", "2BP01");
    sql(&engine, "ROLLBACK TO dependency_check");
    sql(&engine, "SET ROLE renamed_reader; SELECT nextval('secured'); RESET ROLE; SET ROLE renamed_object_owner; REVOKE USAGE ON SEQUENCE secured FROM renamed_delegate CASCADE; RESET ROLE");
    assert!(!permitted(&engine, "renamed_reader"));
    sql(&engine, "ROLLBACK TO original");
    assert!(permitted(&engine, "reader"));
    sql(&engine, "ROLLBACK");
}

#[test]
fn bound_sequence_acl_survives_private_refresh_savepoint_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                setup(&first);
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; GRANT USAGE ON SEQUENCE secured TO reader"));
                let catalog = first.storage.catalog.as_ref().unwrap();
                let encoded = stored(catalog.as_ref(), "secured");
                let SequenceSecurityRow::Bound(bound) = &encoded.security else {
                    panic!("new sequence stores names")
                };
                assert!(bound.acl.as_ref().unwrap().iter().any(
                    |entry| entry.role == Some(first.durable.roles.read()["reader"].identity())
                ));
                sql(
                    &second,
                    "ALTER ROLE other LOGIN; CREATE TABLE unrelated(id int)",
                );
                refresh_catalog(&first, isolation);
                assert!(permitted(&first, "reader"));
                assert!(!permitted(&second, "reader"));
                assert_eq!(stored(catalog.as_ref(), "secured"), encoded);
                sql(&first, finish);
                assert_eq!(permitted(&second, "reader"), finish == "COMMIT");
                drop(second);
                drop(first);
                let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                assert_eq!(permitted(&reopened, "reader"), finish == "COMMIT");
            }
        }
    }
}

#[test]
fn sequence_security_conversion_is_initial_only_and_rolls_back_with_later_catalog_errors() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        setup(&first);
        sql(
            &first,
            "GRANT USAGE ON SEQUENCE secured TO reader; SELECT setval('secured', 77)",
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let original = stored(raw.catalog.as_ref(), "secured");
        let SequenceSecurityRow::Bound(bound) = original.security.clone() else {
            unreachable!()
        };
        let named = BoundSequenceSecurity::from_row(bound)
            .resolve(&first.durable.roles.read())
            .unwrap();
        let legacy = SequenceSecurityRow::Legacy(LegacySequenceSecurity {
            role_owner: named.role_owner,
            acl: named.acl,
        });
        replace(raw.catalog.as_ref(), "secured", legacy.clone());
        let Err(error) = first.new_session() else {
            panic!("secondary session converted old names")
        };
        assert!(
            error.to_string().contains("initial catalog migration"),
            "{error}"
        );
        let before = first.durable.sequence_security.snapshot();
        let error = first.refresh_sequences_from_catalog().unwrap_err();
        assert!(
            error.to_string().contains("initial catalog migration"),
            "{error}"
        );
        assert!(Arc::ptr_eq(
            &before,
            &first.durable.sequence_security.snapshot()
        ));
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("invalid triggers accepted")
        };
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(stored(raw.catalog.as_ref(), "secured").security, legacy);
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(stored(raw.catalog.as_ref(), "secured"), original);
        assert!(permitted(&restored, "reader"));
        assert_eq!(
            sql(&restored, "SELECT nextval('secured') AS value").rows[0]["value"],
            Value::Int(78)
        );
    }
}

#[test]
fn sequence_authority_rejects_replaced_owner_grantee_and_grantor_on_refresh_and_reopen() {
    for provider in 0..3 {
        for endpoint in ["owner", "grantee", "grantor"] {
            let (_directory, first, second) = sessions(provider);
            setup(&first);
            sql(&first, "GRANT USAGE ON SEQUENCE secured TO reader");
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            let raw = factory.open_session().unwrap();
            let original = stored(raw.catalog.as_ref(), "secured");
            let SequenceSecurityRow::Bound(mut bound) = original.security.clone() else {
                unreachable!()
            };
            let reader = first.durable.roles.read()["reader"].identity();
            let entry = bound
                .acl
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|entry| entry.role == Some(reader))
                .unwrap();
            let identity = match endpoint {
                "owner" => &mut bound.role_owner,
                "grantee" => entry.role.as_mut().unwrap(),
                _ => &mut entry.grantor,
            };
            identity.object_id = [42; 16];
            let corrupt = SequenceSecurityRow::Bound(bound);
            replace(raw.catalog.as_ref(), "secured", corrupt.clone());
            let before = first.durable.sequence_security.snapshot();
            let error = first.refresh_sequences_from_catalog().unwrap_err();
            assert!(
                error.to_string().contains("missing role incarnation"),
                "{error}"
            );
            assert!(Arc::ptr_eq(
                &before,
                &first.durable.sequence_security.snapshot()
            ));
            let Err(error) = first.new_session() else {
                panic!("invalid endpoint accepted")
            };
            assert!(
                error.to_string().contains("missing role incarnation"),
                "{error}"
            );
            drop(second);
            drop(first);
            let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
                panic!("invalid endpoint rebound")
            };
            assert!(
                error.to_string().contains("missing role incarnation"),
                "{error}"
            );
            assert_eq!(stored(raw.catalog.as_ref(), "secured").security, corrupt);
            replace(raw.catalog.as_ref(), "secured", original.security);
            let restored = Engine::from_persistent_provider(factory).unwrap();
            assert!(permitted(&restored, "reader"));
        }
    }
}
