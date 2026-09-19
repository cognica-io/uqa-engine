//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Relation owner and ACL identities survive private catalog refresh and restoration.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::{
    tests::relation_lock_support::{error, sessions, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::{catalog_acl::LegacyRelationSecurity, RelationIdentity, Value};
use uqa_sql::catalog::security::BoundTableSecurity;
use uqa_storage::{CatalogFacade, RelationSecurityRow};

const RELATIONS: [&str; 4] = ["secured", "secured_view", "secured_mat", "secured_foreign"];

fn setup(engine: &Engine) {
    sql(engine, "CREATE ROLE reader; CREATE ROLE other; CREATE TABLE secured(id int); INSERT INTO secured VALUES (7); CREATE VIEW secured_view AS SELECT id FROM secured; CREATE MATERIALIZED VIEW secured_mat AS SELECT id FROM secured; CREATE SERVER remote FOREIGN DATA WRAPPER memory_fdw; CREATE FOREIGN TABLE secured_foreign(id int) SERVER remote");
}

fn can_select(engine: &Engine, role: &str, relation: &str) -> bool {
    sql(
        engine,
        &format!(
            "SELECT has_column_privilege('{role}', '{relation}', 'id', 'SELECT') AS permitted"
        ),
    )
    .rows[0]["permitted"]
        == Value::Bool(true)
}

fn stored(catalog: &dyn CatalogFacade, name: &str) -> RelationSecurityRow {
    let relation = RelationIdentity::new("public", name);
    match name {
        "secured" => {
            catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation == relation)
                .unwrap()
                .security
        }
        "secured_foreign" => {
            catalog
                .load_foreign_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation == relation)
                .unwrap()
                .security
        }
        _ => {
            catalog
                .load_views()
                .unwrap()
                .into_iter()
                .find(|row| row.relation == relation)
                .unwrap()
                .security
        }
    }
}

fn replace(catalog: &dyn CatalogFacade, name: &str, security: RelationSecurityRow) {
    let relation = RelationIdentity::new("public", name);
    match name {
        "secured" => {
            let mut row = catalog
                .load_tables()
                .unwrap()
                .into_iter()
                .find(|row| row.relation == relation)
                .unwrap();
            row.security = security;
            catalog.save_table(&row).unwrap();
        }
        "secured_foreign" => {
            assert!(catalog
                .update_foreign_table_security(&relation, &security)
                .unwrap());
        }
        _ => {
            let mut row = catalog
                .load_views()
                .unwrap()
                .into_iter()
                .find(|row| row.relation == relation)
                .unwrap();
            row.security = security;
            catalog.save_view(&row).unwrap();
        }
    }
}

#[test]
fn memory_relation_acl_keeps_identity_after_role_names_are_reused() {
    let engine = Engine::new();
    setup(&engine);
    sql(&engine, "GRANT SELECT(id) ON secured, secured_view, secured_mat, secured_foreign TO reader; BEGIN; SAVEPOINT before_rename");
    {
        let mut roles = engine.durable.roles.write();
        let mut role = roles.remove("reader").unwrap();
        role.name = "renamed".into();
        roles.insert(role.name.clone(), role);
    }
    engine.note_catalog_registry_changed();
    sql(&engine, "CREATE ROLE reader");
    for relation in RELATIONS {
        assert!(can_select(&engine, "renamed", relation));
        assert!(!can_select(&engine, "reader", relation));
    }
    error(&engine, "DROP ROLE renamed", "2BP01");
    sql(&engine, "ROLLBACK TO before_rename");
    for relation in RELATIONS {
        assert!(can_select(&engine, "reader", relation));
    }
    sql(&engine, "ROLLBACK");
}

#[test]
fn relation_owners_and_delegated_grants_keep_identity_through_view_execution() {
    let engine = Engine::new();
    setup(&engine);
    sql(&engine, "CREATE ROLE object_owner; CREATE ROLE delegate; GRANT CREATE ON SCHEMA public TO object_owner; ALTER TABLE secured OWNER TO object_owner; ALTER VIEW secured_view OWNER TO object_owner; ALTER MATERIALIZED VIEW secured_mat OWNER TO object_owner; ALTER FOREIGN TABLE secured_foreign OWNER TO object_owner; SET ROLE object_owner; GRANT SELECT ON secured, secured_view, secured_mat, secured_foreign TO delegate WITH GRANT OPTION; SET ROLE delegate; GRANT SELECT(id) ON secured, secured_view, secured_mat, secured_foreign TO reader; RESET ROLE; BEGIN; SAVEPOINT before_rename");
    let owner = engine.durable.roles.read()["object_owner"].identity();
    // Exercise retained runtime identities independently of the pending SQL role-rename command.
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
    for name in RELATIONS {
        assert!(can_select(&engine, "renamed_reader", name));
        assert!(!can_select(&engine, "reader", name));
        assert!(!can_select(&engine, "object_owner", name));
    }
    assert_eq!(
        engine
            .view_definition("secured_view")
            .unwrap()
            .unwrap()
            .security
            .role_owner,
        owner
    );
    assert_eq!(
        sql(
            &engine,
            "SELECT viewowner FROM pg_views WHERE viewname = 'secured_view'"
        )
        .rows[0]["viewowner"],
        Value::Str("renamed_object_owner".into())
    );
    sql(&engine, "SET ROLE renamed_reader");
    for name in ["secured", "secured_view", "secured_mat"] {
        assert_eq!(
            sql(&engine, &format!("SELECT id FROM {name}")).rows[0]["id"],
            Value::Int(7)
        );
    }
    sql(&engine, "RESET ROLE; SET ROLE renamed_object_owner; REFRESH MATERIALIZED VIEW secured_mat; REVOKE SELECT ON secured, secured_view, secured_mat, secured_foreign FROM renamed_delegate CASCADE; RESET ROLE");
    for name in RELATIONS {
        assert!(!can_select(&engine, "renamed_reader", name));
    }
    sql(&engine, "ROLLBACK TO before_rename");
    for name in RELATIONS {
        assert!(can_select(&engine, "reader", name));
    }
    sql(&engine, "ROLLBACK");
}

#[test]
fn column_acl_private_changes_survive_catalog_refresh_undo_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                setup(&first);
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; GRANT SELECT(id) ON secured, secured_view, secured_mat, secured_foreign TO reader"));
                let catalog = first.storage.catalog.as_ref().unwrap();
                let encoded = RELATIONS.map(|name| stored(catalog.as_ref(), name));
                let reader = first.durable.roles.read()["reader"].identity();
                for row in &encoded {
                    let RelationSecurityRow::Bound(bound) = row else {
                        panic!("new relations must retain role identities")
                    };
                    assert!(bound.column_acls["id"]
                        .iter()
                        .any(|entry| entry.role == Some(reader)));
                }
                sql(
                    &second,
                    "ALTER ROLE other LOGIN; CREATE TABLE unrelated(id int)",
                );
                refresh_catalog(&first, isolation);
                for (name, row) in RELATIONS.into_iter().zip(&encoded) {
                    assert!(can_select(&first, "reader", name));
                    assert!(!can_select(&second, "reader", name));
                    assert_eq!(&stored(catalog.as_ref(), name), row);
                }
                sql(&first, finish);
                for name in RELATIONS {
                    assert_eq!(can_select(&second, "reader", name), finish == "COMMIT");
                }
                drop(second);
                drop(first);
                let reopened = reopen(provider, &directory.path().join("table-locks.db"));
                for name in RELATIONS {
                    assert_eq!(can_select(&reopened, "reader", name), finish == "COMMIT");
                }
            }
        }
    }
}

#[test]
fn relation_acl_conversion_rolls_back_when_later_catalog_restoration_fails() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        setup(&first);
        sql(
            &first,
            "GRANT SELECT(id) ON secured, secured_view, secured_mat, secured_foreign TO reader",
        );
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let mut expected = RELATIONS.map(|name| stored(raw.catalog.as_ref(), name));
        // Rewriting a definition baseline retires the independent tuple revisions.
        for row in &mut expected {
            if let RelationSecurityRow::Bound(row) = row {
                row.acl_revisions = uqa_core::catalog_acl::RelationAclRevisions::default();
            }
        }
        let legacy = expected.clone().map(|row| {
            let RelationSecurityRow::Bound(bound) = row else {
                unreachable!()
            };
            let named = BoundTableSecurity::from_row(bound)
                .resolve(&first.durable.roles.read())
                .unwrap();
            RelationSecurityRow::Legacy(LegacyRelationSecurity {
                role_owner: named.role_owner,
                acl: named.acl,
                column_acls: named.column_acls,
            })
        });
        for (name, row) in RELATIONS.into_iter().zip(&legacy) {
            replace(raw.catalog.as_ref(), name, row.clone());
        }
        let Err(error) = first.new_session() else {
            panic!("secondary session converted legacy relation names")
        };
        assert!(
            error.to_string().contains("initial catalog migration"),
            "{error}"
        );
        raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
        drop(second);
        drop(first);
        let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("malformed trigger metadata accepted")
        };
        assert!(error.to_string().contains("EOF"), "{error}");
        assert_eq!(
            RELATIONS.map(|name| stored(raw.catalog.as_ref(), name)),
            legacy
        );
        raw.catalog.delete_metadata("sql_triggers_json").unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(
            RELATIONS.map(|name| stored(raw.catalog.as_ref(), name)),
            expected
        );
        for name in RELATIONS {
            assert!(can_select(&restored, "reader", name));
        }
    }
}

#[test]
fn relation_acl_corruption_rejects_owner_grantee_and_grantor_incarnations() {
    for provider in 0..3 {
        for name in RELATIONS {
            for endpoint in ["owner", "grantee", "grantor"] {
                let (_directory, first, second) = sessions(provider);
                setup(&first);
                sql(&first, &format!("GRANT SELECT(id) ON {name} TO reader"));
                let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
                let raw = factory.open_session().unwrap();
                let original = stored(raw.catalog.as_ref(), name);
                let RelationSecurityRow::Bound(mut bound) = original.clone() else {
                    unreachable!()
                };
                bound.acl_revisions = uqa_core::catalog_acl::RelationAclRevisions::default();
                let reference = match endpoint {
                    "owner" => &mut bound.role_owner,
                    "grantee" => bound.column_acls.get_mut("id").unwrap()[0]
                        .role
                        .as_mut()
                        .unwrap(),
                    _ => &mut bound.column_acls.get_mut("id").unwrap()[0].grantor,
                };
                reference.object_id = [42; 16];
                let corrupt = RelationSecurityRow::Bound(bound);
                replace(raw.catalog.as_ref(), name, corrupt.clone());
                let Err(error) = first.new_session() else {
                    panic!("corrupt relation endpoint accepted")
                };
                assert!(
                    error.to_string().contains("missing role incarnation"),
                    "{error}"
                );
                drop(second);
                drop(first);
                let Err(error) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
                    panic!("corrupt relation endpoint rebound")
                };
                assert!(
                    error.to_string().contains("missing role incarnation"),
                    "{error}"
                );
                assert_eq!(stored(raw.catalog.as_ref(), name), corrupt);
                replace(raw.catalog.as_ref(), name, original);
                let restored = Engine::from_persistent_provider(factory).unwrap();
                assert!(can_select(&restored, "reader", name));
            }
        }
    }
}

#[test]
fn independent_acl_records_reject_replaced_role_incarnations_without_rebinding() {
    for provider in 0..3 {
        for name in RELATIONS {
            for endpoint in ["role", "grantor"] {
                let (_directory, first, _second) = sessions(provider);
                setup(&first);
                sql(&first, &format!("GRANT SELECT(id) ON {name} TO reader"));
                let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
                let raw = factory.open_session().unwrap();
                let key = uqa_storage::catalog::relation_acl::key(
                    &RelationIdentity::new("public", name),
                    Some("id"),
                );
                let original = raw.catalog.get_metadata(&key).unwrap().unwrap();
                let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
                value["acl"][0][endpoint]["object_id"] = serde_json::json!(vec![42; 16]);
                let corrupt = value.to_string();
                raw.catalog.set_metadata(&key, &corrupt).unwrap();
                let error = first
                    .new_session()
                    .err()
                    .expect("corrupt ACL tuple accepted");
                assert!(
                    error.to_string().contains("missing role incarnation"),
                    "{error}"
                );
                assert_eq!(
                    raw.catalog.get_metadata(&key).unwrap().as_deref(),
                    Some(corrupt.as_str())
                );
                raw.catalog.set_metadata(&key, &original).unwrap();
                assert!(can_select(&first.new_session().unwrap(), "reader", name));
            }
        }
    }
}
