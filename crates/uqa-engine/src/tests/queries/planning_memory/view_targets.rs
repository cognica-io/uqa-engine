//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_view_target_selection_avoids_full_definition_copies(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("view-target-selection.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer)", &[]).unwrap();
    let large = "x".repeat(4 << 20);
    engine
        .sql(
            &format!("CREATE VIEW large_view AS SELECT id, '{large}'::text AS payload FROM items"),
            &[],
        )
        .unwrap();
    let services = engine.view_rewrite_context();
    let observed = allocation_counter::measure(|| {
        let target =
            uqa_sql::semantics::view_mutation::resolve_view_target(services, "large_view").unwrap();
        assert_eq!(target.columns, ["id", "payload"]);
        assert_eq!(target.canonical_name, "public.large_view");
    });
    assert!(
        observed.bytes_total < (4 << 20) + (256 << 10),
        "{provider:?}: {observed:?}"
    );
    engine.close().unwrap();
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_trigger_view_targets_reject_before_copies_and_recover_all_commands(
    #[case] provider: Provider,
) {
    use uqa_core::memory::ProductionControl;
    use uqa_sql::semantics::view_mutation::resolve_view_target_with_control;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("trigger-view-targets.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer PRIMARY KEY, payload text); INSERT INTO items VALUES(1,'initial'); CREATE VIEW v AS SELECT id,payload FROM items", &[]).unwrap();
    engine.sql("CREATE FUNCTION apply_view_change() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF TG_OP = 'INSERT' THEN INSERT INTO items VALUES(NEW.id,NEW.payload); RETURN NEW; ELSIF TG_OP = 'UPDATE' THEN UPDATE items SET id = NEW.id, payload = NEW.payload WHERE id = OLD.id; RETURN NEW; ELSE DELETE FROM items WHERE id = OLD.id; RETURN OLD; END IF; END $$; CREATE TRIGGER apply_view_change INSTEAD OF INSERT OR UPDATE OR DELETE ON v FOR EACH ROW EXECUTE FUNCTION apply_view_change()", &[]).unwrap();
    let large = "x".repeat(4 << 20);
    engine
        .sql(
            &format!("CREATE VIEW large_view AS SELECT id, '{large}'::text AS payload FROM items"),
            &[],
        )
        .unwrap();
    let retained = engine.query_retention_control().unwrap();
    let memory = retained.memory();
    let cancellation = uqa_core::CancellationToken::new();
    let control = ProductionControl::new(memory, retained.cancellation(), &cancellation);
    let before = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let observed = allocation_counter::measure(|| {
        let error =
            resolve_view_target_with_control(engine.view_rewrite_context(), "large_view", &control)
                .expect_err("native target must reject before copying");
        assert_eq!(error.sqlstate(), Some("53200"), "{provider:?}: {error}");
    });
    assert!(
        observed.bytes_total < 256 << 10,
        "{provider:?}: {observed:?}"
    );
    assert_eq!(memory.used(), before + occupied.bytes());
    drop(occupied);
    let target =
        resolve_view_target_with_control(engine.view_rewrite_context(), "v", &control).unwrap();
    assert_eq!(target.columns, ["id", "payload"]);
    assert!(memory.used() > before);
    drop(target);
    assert_eq!(memory.used(), before);
    cancellation.cancel();
    let error =
        resolve_view_target_with_control(engine.view_rewrite_context(), "v", &control).unwrap_err();
    assert_eq!(error.sqlstate(), Some("57014"));
    assert_eq!(memory.used(), before);
    engine.sql("INSERT INTO v VALUES(2,'inserted'); UPDATE v SET payload = 'updated' WHERE id = 1; MERGE INTO v AS target USING (VALUES(2,'merged'),(3,'new')) AS source(id,payload) ON target.id = source.id WHEN MATCHED THEN UPDATE SET payload = source.payload WHEN NOT MATCHED THEN INSERT(id,payload) VALUES(source.id,source.payload); DELETE FROM v WHERE id = 1", &[]).unwrap();
    assert_trigger_rows(&engine);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let reopened = provider.open(&path);
        assert_trigger_rows(&reopened);
        reopened.close().unwrap();
    }
}

fn assert_trigger_rows(engine: &Engine) {
    let result = engine
        .sql("SELECT id,payload FROM items ORDER BY id", &[])
        .unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.value_at(0, 0), Some(&Value::Int(2)));
    assert_eq!(result.value_at(0, 1), Some(&Value::Str("merged".into())));
    assert_eq!(result.value_at(1, 0), Some(&Value::Int(3)));
    assert_eq!(result.value_at(1, 1), Some(&Value::Str("new".into())));
}
