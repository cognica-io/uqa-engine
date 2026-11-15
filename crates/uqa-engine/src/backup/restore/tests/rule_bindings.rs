//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::providers::{capture_provider, destination};
use super::*;
use crate::tests::providers::Provider;
use uqa_core::RelationIdentity;
use uqa_sql::{catalog::events::StoredRule, ScalarExpr};

mod preserved;

fn corrupt(rule: &mut StoredRule, damage: &str) {
    if damage == "type" {
        rule.condition_plan.as_mut().unwrap().scalar = ScalarExpr::Literal(Value::Int(42));
        return;
    }
    if damage == "unbound" {
        rule.condition_plan = None;
        rule.condition_binding = None;
        return;
    }
    if damage == "missing" {
        rule.condition_binding = None;
        return;
    }
    let binding = rule.condition_binding.as_ref().unwrap();
    if damage == "shared" {
        let mut encoded = serde_json::to_value(binding).unwrap();
        encoded["new_relation"] = encoded["old_relation"].clone();
        rule.condition_binding = Some(serde_json::from_value(encoded).unwrap());
        return;
    }
    let reference = if damage == "identity" {
        uqa_sql::ast::InternalRelationId::from_raw(u64::MAX).column(0)
    } else {
        binding.old_relation().unwrap().column(100)
    };
    let mut replaced = false;
    rule.condition_plan
        .as_mut()
        .unwrap()
        .scalar
        .visit_mut(&mut |expression| {
            if matches!(expression, ScalarExpr::InternalColumn(_)) {
                *expression = ScalarExpr::InternalColumn(reference);
                replaced = true;
            }
        });
    assert!(replaced);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn restored_rule_bindings_reject_invalid_original_row_references(
    #[case] provider: Provider,
    #[values("identity", "attribute", "missing", "shared", "unbound", "type")] damage: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = provider.open(&directory.path().join("source"));
    source.sql("CREATE TABLE items(id integer); CREATE RULE saved AS ON UPDATE TO items WHERE OLD.id IS DISTINCT FROM NEW.id DO NOTHING", &[]).unwrap();
    let root = if let Some(persistent) = source.storage.provider.clone() {
        drop(source);
        let session = persistent.open_session().unwrap();
        let json = session
            .catalog
            .get_metadata("sql_rules_json")
            .unwrap()
            .unwrap();
        let mut stored: uqa_sql::catalog::events::persistence::StoredRuleCatalog =
            serde_json::from_str(&json).unwrap();
        assert_eq!(stored.rules.len(), 1);
        corrupt(&mut stored.rules[0], damage);
        session
            .catalog
            .set_metadata("sql_rules_json", &serde_json::to_string(&stored).unwrap())
            .unwrap();
        drop(session);
        capture_provider(persistent.as_ref(), &repository)
    } else {
        source
            .durable
            .rules
            .write()
            .edit(&RelationIdentity::new("public", "items"), |definitions| {
                definitions
                    .edit("saved", |rule| {
                        corrupt(rule, damage);
                        Ok(())
                    })?
                    .unwrap();
                Ok(())
            })
            .unwrap()
            .unwrap();
        let root = capture(&source, &repository, None);
        drop(source);
        root
    };
    let path = directory.path().join("restored");
    let control = StorageReadControl::with_limit(32 << 20);
    let authority = Engine::new();
    let mut job = authority
        .begin_restore(
            &repository,
            root.backup(),
            destination(provider, &path),
            RestoreOptions::new(16 << 20, 8 << 20),
            &control,
        )
        .unwrap();
    let error = (0..65536)
        .find_map(|_| match job.advance(1024) {
            Err(error) => Some(error),
            Ok(RestorePhase::Reading | RestorePhase::Validating) => None,
            Ok(phase) => panic!("{provider:?}: invalid rule reached {phase:?}"),
        })
        .expect("rule validation must finish");
    let expected = if damage == "type" {
        "argument of WHERE must be type boolean"
    } else if matches!(provider, Provider::Memory) {
        "rule condition"
    } else if matches!(damage, "missing" | "unbound") {
        "rule expressions require an initial-open migration"
    } else {
        "is outside the bound row scope"
    };
    assert!(
        error.to_string().contains(expected),
        "{provider:?}: {damage}: {error}"
    );
    assert_eq!(job.phase(), RestorePhase::Failed);
    assert!(!path.exists());
    assert_eq!(control.memory().used(), 0);
    assert!(matches!(job.publish(), Err(BackupError::Finished)));
}
