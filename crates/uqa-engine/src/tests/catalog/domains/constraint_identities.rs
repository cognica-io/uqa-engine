//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::{collections::BTreeSet, sync::Arc};
use uqa_execution::catalog::domain::DOMAINS_METADATA_KEY;
use uqa_sql::schema::domains::constraints;

fn identities(
    engine: &crate::Engine,
) -> Vec<(String, Vec<uqa_sql::ast::ConstraintCatalogIdentity>)> {
    engine
        .durable
        .domains
        .read()
        .iter()
        .map(|(name, domain)| {
            (
                name.clone(),
                constraints::identities(&domain.definition).collect(),
            )
        })
        .collect()
}

#[test]
fn domain_constraint_identities_survive_peer_commits_savepoint_undo_and_reopen() {
    for provider in 0..3 {
        let (directory, first, second) = sessions(provider);
        sql(
            &first,
            "BEGIN; CREATE DOMAIN first_domain AS int NOT NULL CONSTRAINT shared CHECK(VALUE>0)",
        );
        let second = before_commit(
            &first,
            second,
            "CREATE DOMAIN second_domain AS int NOT NULL CONSTRAINT shared CHECK(VALUE<100)",
        );
        sql(&first, "SELECT 1::first_domain, 1::second_domain");
        let before = identities(&first);
        assert_eq!(before.iter().flat_map(|(_, rows)| rows).count(), 4);
        let mut objects = BTreeSet::new();
        let mut oids = BTreeSet::new();
        for (_, rows) in &before {
            for identity in rows {
                assert!(identity.is_valid());
                assert!(objects.insert(identity.object_id));
                assert!(oids.insert(identity.oid));
            }
        }
        sql(&first, "BEGIN; SAVEPOINT kept; DROP DOMAIN first_domain; CREATE DOMAIN first_domain AS int NOT NULL CHECK(VALUE>10)");
        assert_ne!(identities(&first), before);
        sql(&first, "ROLLBACK TO kept; COMMIT");
        assert_eq!(identities(&first), before);
        drop((first, second));
        let engine = reopen(provider, &directory.path().join("table-locks.db"));
        assert_eq!(identities(&engine), before);
        error(&engine, "SELECT NULL::first_domain", "23502");
        error(&engine, "SELECT 100::second_domain", "23514");
    }
}

#[test]
fn current_domain_constraint_corruption_is_rejected_without_repair() {
    for provider in 0..3 {
        for corruption in 0..4 {
            let (_directory, first, second) = sessions(provider);
            sql(&first, "CREATE DOMAIN a AS int NOT NULL CHECK(VALUE>0); CREATE DOMAIN b AS int CHECK(VALUE<100)");
            let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
            let raw = factory.open_session().unwrap();
            let key = "uqa.sql.domain.v1:public.a";
            let original = raw.catalog.get_metadata(key).unwrap().unwrap();
            let mut value: serde_json::Value = serde_json::from_str(&original).unwrap();
            match corruption {
                0 => value["definition"]["checks"][0]["catalog_identity"] = serde_json::Value::Null,
                1 => {
                    value["definition"]["checks"][0]["catalog_identity"]["object_id"] =
                        serde_json::to_value([0_u8; 16]).unwrap();
                }
                2 => {
                    value["definition"]["checks"][0]["catalog_identity"] =
                        value["definition"]["not_null"]["catalog_identity"].clone();
                }
                _ => {
                    value["definition"]["checks"][0]["catalog_identity"] = serde_json::to_value(
                        first.durable.domains.read()["public.b"].definition.checks[0]
                            .catalog_identity,
                    )
                    .unwrap();
                }
            }
            let corrupt = value.to_string();
            raw.catalog.set_metadata(key, &corrupt).unwrap();
            assert!(first.new_session().is_err());
            drop((first, second));
            let Err(failure) = crate::Engine::from_persistent_provider(Arc::clone(&factory)) else {
                panic!("current domain constraint corruption was repaired")
            };
            assert!(failure.to_string().contains("constraint"), "{failure}");
            assert_eq!(raw.catalog.get_metadata(key).unwrap().unwrap(), corrupt);
            raw.catalog.set_metadata(key, &original).unwrap();
            crate::Engine::from_persistent_provider(factory).unwrap();
        }
    }
}

#[test]
fn domain_constraint_conversion_rolls_back_if_later_path_restoration_fails() {
    for provider in 0..3 {
        let (_directory, first, second) = sessions(provider);
        sql(&first, "CREATE DOMAIN a AS int NOT NULL CHECK(VALUE>0)");
        let factory = Arc::clone(first.storage.provider.as_ref().unwrap());
        let raw = factory.open_session().unwrap();
        let key = "uqa.sql.domain.v1:public.a";
        let mut legacy: serde_json::Value =
            serde_json::from_str(&raw.catalog.get_metadata(key).unwrap().unwrap()).unwrap();
        legacy["definition"]["not_null"]
            .as_object_mut()
            .unwrap()
            .remove("catalog_identity");
        for check in legacy["definition"]["checks"].as_array_mut().unwrap() {
            check.as_object_mut().unwrap().remove("catalog_identity");
        }
        let legacy = legacy.to_string();
        raw.catalog.set_metadata(key, &legacy).unwrap();
        raw.catalog
            .set_metadata(DOMAINS_METADATA_KEY, r#"{"domain_catalog_format":2}"#)
            .unwrap();
        raw.catalog.save_path_index("invalid", "{").unwrap();
        assert!(first.new_session().is_err());
        drop((first, second));
        assert!(crate::Engine::from_persistent_provider(Arc::clone(&factory)).is_err());
        assert_eq!(raw.catalog.get_metadata(key).unwrap().unwrap(), legacy);
        assert_eq!(
            raw.catalog
                .get_metadata(DOMAINS_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some(r#"{"domain_catalog_format":2}"#)
        );
        raw.catalog.drop_path_index("invalid").unwrap();
        let engine = crate::Engine::from_persistent_provider(Arc::clone(&factory)).unwrap();
        let before = identities(&engine);
        assert_eq!(before[0].1.len(), 2);
        assert_eq!(
            raw.catalog
                .get_metadata(DOMAINS_METADATA_KEY)
                .unwrap()
                .as_deref(),
            Some(r#"{"domain_catalog_format":3}"#)
        );
        drop(engine);
        let engine = crate::Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(identities(&engine), before);
    }
}
