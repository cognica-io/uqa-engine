//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::{memory::ProductionControl, CancellationToken, RelationIdentity};
use uqa_sql::{
    ast::{EventEnableMode, Expr, RuleEvent, Statement},
    semantics::rules::RuleCatalog,
};

fn enlarge_action(engine: &Engine, name: &str) {
    let mut rules = engine.durable.rules.write();
    let entries = std::sync::Arc::make_mut(
        rules
            .get_mut(&RelationIdentity::new("public", "items"))
            .unwrap(),
    );
    let rule = entries.get_mut(name).unwrap();
    let Statement::Select(query) = &mut rule.definition.actions[0] else {
        panic!("action query")
    };
    query.projections[0].expr = Expr::Literal(Value::Str("x".repeat(4 * 1024 * 1024)));
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_rule_selection_borrows_the_live_generation_and_retains_its_owner(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let mut engine = provider.open(&directory.path().join("rule-selection.db"));
    engine.sql("CREATE TABLE items (id integer); CREATE RULE r AS ON INSERT TO items DO ALSO SELECT 'payload'", &[]).unwrap();
    enlarge_action(&engine, "r");
    let old = allocation_counter::measure(|| {
        drop(RuleCatalog::rules_for(&engine, "public.items", RuleEvent::Insert).unwrap());
    });
    assert!(old.bytes_total >= 4 * 1024 * 1024);
    engine.query_catalog_snapshot = Some(std::sync::Arc::new(
        crate::session::RetainedCatalogSnapshot {
            durable: engine.durable.snapshot(),
            read_view: engine.catalog_read_view(),
        },
    ));
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, retained.cancellation(), &invoking);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = Some(
            RuleCatalog::capture_rules_with_control(
                &engine,
                "public.items",
                RuleEvent::Insert,
                &control,
            )
            .unwrap(),
        );
    });
    assert!(
        allocation.bytes_total < 16 * 1024,
        "{provider:?}: {allocation:?}"
    );
    let selected = selected.unwrap();
    assert_eq!(
        selected.iter().next().unwrap().unwrap().definition.name,
        "r"
    );
    {
        let mut rules = engine.durable.rules.write();
        std::sync::Arc::make_mut(
            rules
                .get_mut(&RelationIdentity::new("public", "items"))
                .unwrap(),
        )
        .get_mut("r")
        .unwrap()
        .enabled = EventEnableMode::Disabled;
    }
    assert!(
        RuleCatalog::rules_for(&engine, "public.items", RuleEvent::Insert)
            .unwrap()
            .is_empty()
    );
    assert!(RuleCatalog::capture_rules_with_control(
        &engine,
        "public.items",
        RuleEvent::Insert,
        &control
    )
    .unwrap()
    .is_empty()
    .unwrap());
    assert_eq!(
        selected.iter().next().unwrap().unwrap().enabled,
        EventEnableMode::Origin
    );
    let selected_bytes = memory.used();
    for token in [retained.cancellation(), &invoking] {
        token.cancel();
        assert_eq!(
            selected.iter().next().unwrap().unwrap_err().sqlstate(),
            Some("57014")
        );
        let error = RuleCatalog::capture_rules_with_control(
            &engine,
            "public.items",
            RuleEvent::Insert,
            &control,
        )
        .err()
        .unwrap();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(memory.used(), selected_bytes);
        token.reset();
    }
    drop(selected);
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    assert!(
        RuleCatalog::capture_rules(&engine, "public.items", RuleEvent::Insert)
            .unwrap()
            .is_empty()
            .unwrap()
    );
    assert_eq!(memory.used(), before);
    engine.query_catalog_snapshot = None;
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_rule_execution_does_not_copy_later_actions_before_an_earlier_error(
    #[case] provider: Provider,
    #[values(false, true)] temporary: bool,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rule-action-order.db"));
    // Compare the same catalog view. Independent statistics commits can legitimately require decoding the durable rule before action selection.
    pause_automatic_statistics(&engine);
    let persistence = if temporary { "TEMP " } else { "" };
    engine.sql(&format!("CREATE {persistence}TABLE items (id integer); CREATE RULE a_error AS ON INSERT TO items DO INSTEAD SELECT 1 / 0; CREATE RULE z_payload AS ON INSERT TO items DO ALSO SELECT 'payload'"), &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let occupied = memory.reserve(memory.available() - 1024 * 1024).unwrap();
    let execute = || {
        let error = engine.sql("INSERT INTO items SELECT 1", &[]).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22012"), "{provider:?}: {error}");
    };
    // The first failed execution can retain its parsed SQL in the session cache.
    execute();
    let before = memory.used();
    // Temporary definitions isolate statement allocation from provider pages holding durable catalog records.
    let baseline = temporary.then(|| allocation_counter::measure(execute));
    assert_eq!(memory.used(), before);
    drop(occupied);
    // Publish through the definition boundary so durable refresh sees the same large action.
    engine
        .sql(
            &format!(
                "CREATE OR REPLACE RULE z_payload AS ON INSERT TO items DO ALSO SELECT '{}'",
                "x".repeat(4 * 1024 * 1024)
            ),
            &[],
        )
        .unwrap();
    execute();
    let occupied = memory.reserve(memory.available() - 1024 * 1024).unwrap();
    let before = memory.used();
    if let Some(baseline) = baseline {
        let allocation = allocation_counter::measure(execute);
        assert!(
            allocation.bytes_total <= baseline.bytes_total + 64 * 1024,
            "{provider:?}: small {baseline:?}, large {allocation:?}"
        );
    } else {
        execute();
    }
    assert_eq!(memory.used(), before);
    drop(occupied);
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(0))
    );
    engine
        .sql(
            "DROP RULE a_error ON items; DROP RULE z_payload ON items; INSERT INTO items SELECT 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
    );
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

fn pause_automatic_statistics(engine: &Engine) {
    engine.release_automatic_statistics_client();
    engine
        .session
        .statistics_worker
        .store(true, std::sync::atomic::Ordering::Release);
}

#[rstest::rstest]
#[case::sqlite_native(Provider::SQLiteNative, "22012")]
#[case::sqlite_kv(Provider::SQLiteKeyValue, "53200")]
fn a_new_catalog_view_admits_its_restore_before_selecting_rule_actions(
    #[case] provider: Provider,
    #[case] limited_error: &str,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("rule-catalog-restore.db"));
    pause_automatic_statistics(&engine);
    engine.sql("CREATE TABLE items (id integer); CREATE RULE a_error AS ON INSERT TO items DO INSTEAD SELECT 1 / 0", &[]).unwrap();
    engine
        .sql(
            &format!(
                "CREATE RULE z_payload AS ON INSERT TO items DO ALSO SELECT '{}'",
                "x".repeat(4 * 1024 * 1024)
            ),
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("INSERT INTO items SELECT 1", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    let peer = engine.new_session().unwrap();
    pause_automatic_statistics(&peer);
    // SQLite Key/Value admits a durable record copy, while native SQLite can identify this as a statistics-only change. This comparison targets those two physical reads; the rule-binding cases cover all four providers.
    // Deliver the independent maintenance commit deterministically, before reserving the invoking session's remaining allowance.
    peer.sql("ANALYZE items", &[]).unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let occupied = memory.reserve(memory.available() - 1024 * 1024).unwrap();
    let error = engine.sql("INSERT INTO items SELECT 1", &[]).unwrap_err();
    assert_eq!(
        error.sqlstate(),
        Some(limited_error),
        "{provider:?}: {error}"
    );
    drop(occupied);
    assert_eq!(
        engine
            .sql("INSERT INTO items SELECT 1", &[])
            .unwrap_err()
            .sqlstate(),
        Some("22012")
    );
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(0))
    );
    engine
        .sql(
            "DROP RULE a_error ON items; DROP RULE z_payload ON items; INSERT INTO items SELECT 1",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT count(*) FROM items", &[])
            .unwrap()
            .value_at(0, 0),
        Some(&Value::Int(1))
    );
    peer.close().unwrap();
    drop(peer);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_view_rule_selection_keeps_its_catalog_and_original_control(#[case] provider: Provider) {
    use uqa_sql::semantics::view_rewrite::context::ViewRewriteCatalog;
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("view-rule-selection.db"));
    engine.sql("CREATE TABLE items (id integer); CREATE RULE r AS ON INSERT TO items DO ALSO SELECT 'payload'", &[]).unwrap();
    enlarge_action(&engine, "r");
    let catalog = engine.catalog_read_view();
    let resolution =
        uqa_execution::catalog::services::CatalogSession::relation_name_resolution(&engine);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(memory, resources.cancellation(), &invoking);
    let retained = uqa_execution::catalog::view::metadata::RetainedViewCatalog {
        catalog: &catalog,
        resolution: &resolution,
        fallback: &engine,
        replica: false,
        control: control.capture(),
    };
    std::sync::Arc::make_mut(
        engine
            .durable
            .rules
            .write()
            .get_mut(&RelationIdentity::new("public", "items"))
            .unwrap(),
    )
    .get_mut("r")
    .unwrap()
    .enabled = EventEnableMode::Disabled;
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - 16 * 1024).unwrap();
    let mut selected = None;
    let allocation = allocation_counter::measure(|| {
        selected = Some(
            retained
                .capture_rules("public.items", RuleEvent::Insert)
                .unwrap(),
        );
    });
    assert!(
        allocation.bytes_total < 16 * 1024,
        "{provider:?}: {allocation:?}"
    );
    let selected = selected.unwrap();
    assert_eq!(
        selected.iter().next().unwrap().unwrap().enabled,
        EventEnableMode::Origin
    );
    assert!(
        ViewRewriteCatalog::capture_rules(&engine, "public.items", RuleEvent::Insert)
            .unwrap()
            .is_empty()
            .unwrap()
    );
    let selected_bytes = memory.used();
    for token in [resources.cancellation(), &invoking] {
        token.cancel();
        assert_eq!(
            selected.iter().next().unwrap().unwrap_err().sqlstate(),
            Some("57014")
        );
        assert_eq!(
            retained
                .capture_rules("public.items", RuleEvent::Insert)
                .err()
                .unwrap()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), selected_bytes);
        token.reset();
    }
    drop(selected);
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    drop(retained);
    drop(catalog);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}
