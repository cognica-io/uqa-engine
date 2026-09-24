//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain ownership preserves role incarnations and prevents dangling dependencies.

use super::{identity::reopen, snapshots::refresh_catalog};
use crate::tests::relation_lock_support::sessions;
use crate::{
    tests::relation_lock_support::{error, sql},
    Engine,
};
use std::sync::Arc;
use uqa_core::Value;
use uqa_execution::catalog::domain::DOMAINS_METADATA_KEY;

#[test]
fn private_domain_authority_survives_refresh_savepoint_and_reopen() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            for finish in ["COMMIT", "ROLLBACK", "ROLLBACK TO private; COMMIT"] {
                let (directory, first, second) = sessions(provider);
                sql(
                    &first,
                    "CREATE ROLE other; CREATE DOMAIN removed_domain AS integer",
                );
                sql(&first, &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t; SAVEPOINT private; CREATE ROLE private_owner; GRANT CREATE ON SCHEMA public TO private_owner; SET ROLE private_owner; CREATE DOMAIN private_domain AS integer; RESET ROLE; REVOKE CREATE ON SCHEMA public FROM private_owner; DROP DOMAIN removed_domain"));
                let owner = first.durable.roles.read()["private_owner"].identity();
                let original = first.durable.domains.read()["public.private_domain"].clone();
                assert_eq!(original.owner, owner);
                sql(
                    &second,
                    "ALTER ROLE other LOGIN; CREATE TABLE unrelated(id int)",
                );
                refresh_catalog(&first, isolation);
                assert_eq!(
                    first.durable.domains.read()["public.private_domain"].owner,
                    owner,
                    "provider {provider}, {isolation}, {finish}"
                );
                assert!(!first
                    .durable
                    .domains
                    .read()
                    .contains_key("public.removed_domain"));
                assert_eq!(
                    sql(
                        &first,
                        "SELECT typowner FROM pg_type WHERE typname = 'private_domain'"
                    )
                    .rows[0]["typowner"],
                    Value::Int(owner.oid)
                );
                assert!(sql(
                    &second,
                    "SELECT typowner FROM pg_type WHERE typname = 'private_domain'"
                )
                .rows
                .is_empty());
                sql(&first, finish);
                assert_eq!(
                    sql(
                        &second,
                        "SELECT typowner FROM pg_type WHERE typname = 'private_domain'"
                    )
                    .rows
                    .len(),
                    usize::from(finish == "COMMIT")
                );
                drop(second);
                drop(first);
                let restored = reopen(provider, &directory.path().join("table-locks.db"));
                let registry = restored.durable.domains.read();
                assert_eq!(
                    registry.contains_key("public.removed_domain"),
                    finish != "COMMIT"
                );
                let domain = registry.get("public.private_domain");
                assert_eq!(domain.is_some(), finish == "COMMIT");
                if let Some(domain) = domain {
                    assert_eq!(domain.owner, owner);
                    assert_eq!(domain.object_id, original.object_id);
                    assert_eq!(domain.oid, original.oid);
                }
            }
        }
    }
}

#[test]
fn untouched_domain_catalog_refresh_observes_committed_definitions() {
    for provider in 0..3 {
        for isolation in ["READ COMMITTED", "REPEATABLE READ", "SERIALIZABLE"] {
            let (_directory, first, second) = sessions(provider);
            sql(
                &first,
                &format!("BEGIN ISOLATION LEVEL {isolation}; SELECT * FROM t"),
            );
            sql(&second, "CREATE DOMAIN committed_domain AS integer");
            let original = second.durable.domains.snapshot();
            refresh_catalog(&first, isolation);
            assert_eq!(
                first.durable.domains.read()["public.committed_domain"].object_id,
                original["public.committed_domain"].object_id,
                "provider {provider}, {isolation}"
            );
            sql(&first, "COMMIT");
        }
    }
}

#[test]
fn domain_conversion_is_initial_only_and_later_failure_restores_the_legacy_catalog() {
    for provider in 0..3 {
        for format in [0, 1] {
            let (_directory, first, second) = sessions(provider);
            setup(&first);
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            let raw = factory.open_session().unwrap();
            let original = raw
                .catalog
                .get_metadata(DOMAINS_METADATA_KEY)
                .unwrap()
                .unwrap();
            let records = raw.catalog.metadata_with_prefix("uqa.sql.domain").unwrap();
            let mut domains = serde_json::to_value(&*first.durable.domains.read()).unwrap();
            let legacy = if format == 0 {
                domains["public.owned_domain"]["owner"] = "domain_owner".into();
                domains.to_string()
            } else {
                serde_json::json!({"domain_catalog_format": 1, "domains": domains}).to_string()
            };
            for (key, _) in &records {
                raw.catalog.delete_metadata(key).unwrap();
            }
            raw.catalog
                .set_metadata(DOMAINS_METADATA_KEY, &legacy)
                .unwrap();
            let Err(failure) = first.new_session() else {
                panic!("secondary session migrated domain names")
            };
            assert!(
                failure.to_string().contains("initial catalog migration"),
                "{failure}"
            );
            let before = first.durable.domains.snapshot();
            assert!(first
                .restore_domains_from_catalog(raw.catalog.as_ref(), false)
                .is_err());
            assert!(Arc::ptr_eq(&before, &first.durable.domains.snapshot()));
            raw.catalog.set_metadata("sql_triggers_json", "{").unwrap();
            drop((first, second));
            let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
                panic!("invalid trigger catalog accepted")
            };
            assert!(failure.to_string().contains("EOF"), "{failure}");
            assert_eq!(
                raw.catalog
                    .get_metadata(DOMAINS_METADATA_KEY)
                    .unwrap()
                    .unwrap(),
                legacy
            );
            assert!(raw
                .catalog
                .metadata_with_prefix("uqa.sql.domain")
                .unwrap()
                .is_empty());
            raw.catalog.delete_metadata("sql_triggers_json").unwrap();
            let restored = Engine::from_persistent_provider(factory).unwrap();
            assert_eq!(
                raw.catalog
                    .get_metadata(DOMAINS_METADATA_KEY)
                    .unwrap()
                    .unwrap(),
                original
            );
            assert_eq!(
                raw.catalog.metadata_with_prefix("uqa.sql.domain").unwrap(),
                records
            );
            assert_eq!(
                sql(&restored, "SELECT 7::owned_domain AS value").rows[0]["value"],
                Value::Int(7)
            );
            error(&restored, "DROP ROLE domain_owner", "2BP01");
        }
    }
}

#[test]
fn current_domain_owner_corruption_never_rebinds_on_refresh_or_reopen() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        setup(&first);
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let original = raw
            .catalog
            .get_metadata("uqa.sql.domain.v1:public.owned_domain")
            .unwrap()
            .unwrap();
        let mut corrupt: serde_json::Value = serde_json::from_str(&original).unwrap();
        corrupt["owner"]["object_id"] = serde_json::to_value([42_u8; 16]).unwrap();
        let corrupt = corrupt.to_string();
        raw.catalog
            .set_metadata("uqa.sql.domain.v1:public.owned_domain", &corrupt)
            .unwrap();
        let before = first.durable.domains.snapshot();
        let failure = first
            .restore_domains_from_catalog(raw.catalog.as_ref(), false)
            .unwrap_err();
        assert!(
            failure.to_string().contains("missing role incarnation"),
            "{failure}"
        );
        assert!(Arc::ptr_eq(&before, &first.durable.domains.snapshot()));
        assert!(first.new_session().is_err());
        drop(second);
        drop(first);
        let Err(failure) = Engine::from_persistent_provider(Arc::clone(&factory)) else {
            panic!("corrupt domain owner rebound")
        };
        assert!(
            failure.to_string().contains("missing role incarnation"),
            "{failure}"
        );
        assert_eq!(
            raw.catalog
                .get_metadata("uqa.sql.domain.v1:public.owned_domain")
                .unwrap()
                .unwrap(),
            corrupt
        );
        raw.catalog
            .set_metadata("uqa.sql.domain.v1:public.owned_domain", &original)
            .unwrap();
        let restored = Engine::from_persistent_provider(factory).unwrap();
        error(&restored, "DROP ROLE domain_owner", "2BP01");
    }
}

fn setup(engine: &Engine) {
    sql(engine, "CREATE ROLE domain_owner; GRANT CREATE ON SCHEMA public TO domain_owner; SET ROLE domain_owner; CREATE DOMAIN owned_domain AS integer; RESET ROLE; REVOKE CREATE ON SCHEMA public FROM domain_owner");
}

#[test]
fn drop_role_requires_domain_removal_and_undo_restores_its_dependency() {
    let engine = Engine::new();
    setup(&engine);
    error(&engine, "DROP ROLE domain_owner", "2BP01");
    sql(&engine, "BEGIN; DROP DOMAIN owned_domain; ROLLBACK");
    error(&engine, "DROP ROLE domain_owner", "2BP01");
    sql(
        &engine,
        "BEGIN; SAVEPOINT owned; DROP DOMAIN owned_domain; ROLLBACK TO owned; COMMIT",
    );
    error(&engine, "DROP ROLE domain_owner", "2BP01");
    sql(&engine, "DROP DOMAIN owned_domain; DROP ROLE domain_owner");
}

#[test]
fn domain_owner_keeps_its_incarnation_when_its_name_is_reused() {
    let engine = Engine::new();
    setup(&engine);
    let owner = engine.durable.roles.read()["domain_owner"].identity();
    sql(&engine, "BEGIN; SAVEPOINT original");
    // Isolate retained authority from the separately pending SQL role-rename command.
    {
        let mut roles = engine.durable.roles.write();
        let mut role = roles.remove("domain_owner").unwrap();
        role.name = "renamed_owner".into();
        roles.insert(role.name.clone(), role);
    }
    engine.note_catalog_registry_changed();
    sql(&engine, "CREATE ROLE domain_owner");
    assert_eq!(
        sql(
            &engine,
            "SELECT typowner FROM pg_type WHERE typname = 'owned_domain'"
        )
        .rows[0]["typowner"],
        Value::Int(owner.oid)
    );
    sql(&engine, "SAVEPOINT dependency");
    error(&engine, "DROP ROLE renamed_owner", "2BP01");
    sql(&engine, "ROLLBACK TO dependency; SET ROLE domain_owner");
    error(&engine, "DROP DOMAIN owned_domain", "42501");
    sql(&engine, "ROLLBACK TO dependency; SET ROLE renamed_owner; DROP DOMAIN owned_domain; RESET ROLE; ROLLBACK TO original");
    assert_eq!(
        sql(
            &engine,
            "SELECT typowner FROM pg_type WHERE typname = 'owned_domain'"
        )
        .rows[0]["typowner"],
        Value::Int(owner.oid)
    );
    sql(&engine, "ROLLBACK");
}
