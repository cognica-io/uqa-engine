//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use uqa_core::memory::MemoryBudget;
use uqa_execution::catalog::sequence::registry::SequenceCatalog;

fn bounded(engine: &Engine) -> MemoryBudget {
    let memory = MemoryBudget::new(1 << 20);
    let current = engine.durable.sequences.snapshot();
    let mut next = SequenceCatalog::with_budget(&memory);
    for (relation, record) in current.iter() {
        next.insert(
            relation.clone(),
            *record,
            current.security()[relation].clone(),
        )
        .unwrap();
    }
    *engine.durable.sequences.write() = next;
    memory
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn failed_native_sequence_admission_preserves_values_catalogs_and_recovery(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("native-sequence.db"));
    engine.sql("CREATE SEQUENCE ids CACHE 3; CREATE SEQUENCE other; CREATE SEQUENCE third; CREATE ROLE reader; SELECT nextval('ids')", &[]).unwrap();
    let memory = bounded(&engine);
    let stored = || {
        engine
            .storage
            .catalog
            .as_ref()
            .map(|catalog| format!("{:?}", catalog.load_sequence_rows().unwrap()))
    };
    let before = stored();
    for statement in [
        "CREATE SEQUENCE added",
        "ALTER SEQUENCE ids RESTART WITH 99",
        "ALTER SEQUENCE ids RENAME TO renamed",
        "DROP SEQUENCE ids",
        "GRANT USAGE ON SEQUENCE ids TO reader",
        "ALTER SEQUENCE ids OWNER TO reader",
        "SELECT nextval('ids')",
        "SELECT nextval('other')",
        "SELECT setval('ids', 71, true)",
        "SELECT setval('ids', 71, false)",
    ] {
        let original = engine.durable.sequences.snapshot();
        let caches = engine.session.sequence_caches.lock().clone();
        let observations = engine.session.state.read().sequence_currvals.clone();
        let last = engine.session.state.read().last_sequence.clone();
        let used = memory.used();
        let occupied = memory.reserve(memory.available()).unwrap();
        let error = engine.sql(statement, &[]).unwrap_err();
        assert_eq!(
            error.sqlstate(),
            Some("53200"),
            "{provider:?}: {statement}: {error}"
        );
        let current = engine.durable.sequences.snapshot();
        assert!(
            current.iter().eq(original.iter()),
            "{provider:?}: {statement}"
        );
        assert!(
            current.security().iter().eq(original.security().iter()),
            "{provider:?}: {statement}"
        );
        assert!(
            current
                .catalog_oids()
                .iter()
                .eq(original.catalog_oids().iter()),
            "{provider:?}: {statement}"
        );
        for (relation, record) in original.iter() {
            assert!(
                std::ptr::eq(record, current.get(relation).unwrap()),
                "{provider:?}: {statement}: {relation:?}"
            );
        }
        assert!(
            *engine.session.sequence_caches.lock() == caches,
            "{provider:?}: {statement}"
        );
        assert!(
            engine.session.state.read().sequence_currvals == observations,
            "{provider:?}: {statement}"
        );
        assert!(
            engine.session.state.read().last_sequence == last,
            "{provider:?}: {statement}"
        );
        assert_eq!(stored(), before, "{provider:?}: {statement}");
        drop((current, occupied));
        assert_eq!(memory.used(), used, "{provider:?}: {statement}");
        assert_eq!(engine.transaction_depth(), 0);
    }
    assert_eq!(engine.currval("ids").unwrap(), 1);
    assert_eq!(engine.nextval("ids").unwrap(), 2);
    engine.sql("ALTER SEQUENCE ids RESTART WITH 90; SELECT nextval('ids'); GRANT USAGE ON SEQUENCE ids TO reader", &[]).unwrap();
    assert_eq!(engine.currval("ids").unwrap(), 90);
    drop(engine);
    assert_eq!(memory.used(), 0);
}
