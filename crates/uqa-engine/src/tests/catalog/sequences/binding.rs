//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::{tests::providers::Provider, Engine};
use std::{collections::BTreeSet, sync::Arc};
use uqa_core::{
    image_io::ImageDecoder,
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};
use uqa_sql::{
    binding::stored_relations::{
        bind_stored_query_relations, StoredQueryBindingContext, StoredQuerySequences,
    },
    catalog::resolution::RelationLookupMode,
    plan::{PlanRef, RetainedExecutablePlan, UnifiedPlan},
    SQLError,
};

fn retained(
    engine: Arc<Engine>,
    sql: &str,
    loaded: bool,
    memory: &MemoryBudget,
) -> RetainedExecutablePlan {
    let syntax = uqa_sql::compile(sql).unwrap().remove(0);
    let memory = memory.clone();
    let work = MemoryBudget::new(1 << 20);
    let invoking = CancellationToken::new();
    let result = ImageDecoder::<_, SQLError>::new(&work, &invoking, |mut reader| async move {
        let original = CancellationToken::new();
        let mut input = reader.input();
        let control = ProductionControl::new(&memory, &original, input.cancellation);
        let plan = UnifiedPlan::lower_executable_from_input(
            &syntax,
            engine.as_ref(),
            &control,
            &mut input,
        )
        .await?;
        let temporary = engine.temporary_schema_name();
        let transitions = BTreeSet::new();
        let context = StoredQueryBindingContext {
            relations: engine.as_ref(),
            lookup_mode: RelationLookupMode::Dynamic,
            sequences: engine.as_ref(),
            temporary_schema: &temporary,
            transition_relations: &transitions,
        };
        let (plan, uses_temporary) = context
            .bind_query_names_from_input(
                plan,
                "SQL routine body",
                false,
                loaded,
                &control,
                &mut input,
            )
            .await?;
        assert!(!uses_temporary);
        Ok(plan)
    })
    .unwrap()
    .read(&mut std::io::empty())
    .unwrap();
    assert_eq!(work.used(), 0);
    result
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_catalog_names_share_live_sequence_selection_without_consuming_values(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = Arc::new(provider.open(&directory.path().join("query-names.db")));
    engine.sql("CREATE SCHEMA first; CREATE SCHEMA second; CREATE TABLE first.items(n integer); CREATE TABLE second.items(n integer); CREATE SEQUENCE first.counter START 41; CREATE SEQUENCE second.counter START 81", &[]).unwrap();
    let memory = MemoryBudget::new(32 << 20);
    let token = CancellationToken::new();
    let control = ProductionControl::new(&memory, &token, &token);
    let sql = "WITH c AS (SELECT nextval('counter') n FROM items) SELECT n, (SELECT currval('first.counter')) FROM c";
    for path in ["first, second", "second, first"] {
        engine
            .sql(&format!("SET search_path = {path}"), &[])
            .unwrap();
        let expected_name = if path.starts_with("first") {
            "first.counter"
        } else {
            "second.counter"
        };
        for loaded in [false, true] {
            let name = if loaded {
                engine.loaded_query_sequence_with_control("counter", &control)
            } else {
                engine.query_sequence_with_control("counter", &control)
            }
            .unwrap();
            assert_eq!(name.as_str(), expected_name);
            assert!(memory.used() >= name.capacity());
            drop(name);
            assert_eq!(memory.used(), 0);
            let UnifiedPlan::Query(mut expected) =
                UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
            else {
                panic!("query")
            };
            let temporary = engine.temporary_schema_name();
            let transitions = BTreeSet::new();
            let context = StoredQueryBindingContext {
                relations: engine.as_ref(),
                lookup_mode: RelationLookupMode::Dynamic,
                sequences: engine.as_ref(),
                temporary_schema: &temporary,
                transition_relations: &transitions,
            };
            bind_stored_query_relations(&context, &mut expected, "SQL routine body", false, loaded)
                .unwrap();
            let actual = retained(engine.clone(), sql, loaded, &memory);
            let PlanRef::Query(query) = actual.as_plan_ref() else {
                panic!("query")
            };
            assert_eq!(
                serde_json::to_value(query).unwrap(),
                serde_json::to_value(&*expected).unwrap()
            );
            assert_eq!(allocation_counter::measure(|| drop(actual)).count_total, 0);
            assert_eq!(memory.used(), 0);
        }
    }
    assert_eq!(engine.nextval("first.counter").unwrap(), 41);
    assert_eq!(engine.nextval("second.counter").unwrap(), 81);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn retained_sequence_lookup_keeps_resource_failures_and_catalog_state(#[case] provider: Provider) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("query-name-failures.db"));
    engine.sql("CREATE SEQUENCE counter START 41", &[]).unwrap();
    let memory = MemoryBudget::new(1 << 20);
    let sibling = memory.reserve(37).unwrap();
    for loaded in [false, true] {
        let temporary = engine.temporary_schema_name();
        let transitions = BTreeSet::new();
        let context = StoredQueryBindingContext {
            relations: &engine,
            lookup_mode: RelationLookupMode::Dynamic,
            sequences: &engine,
            temporary_schema: &temporary,
            transition_relations: &transitions,
        };
        let token = CancellationToken::new();
        let control = ProductionControl::new(&memory, &token, &token);
        for reference in ["missing_sequence", "invalid.", "\"unfinished"] {
            let ordinary = context
                .canonical_sequence_with_control(
                    reference,
                    "SQL routine body",
                    loaded,
                    &ProductionControl::uncontrolled(),
                )
                .unwrap_err();
            let retained = context
                .canonical_sequence_with_control(reference, "SQL routine body", loaded, &control)
                .unwrap_err();
            assert_eq!(retained.sqlstate(), ordinary.sqlstate());
            assert_eq!(retained.to_string(), ordinary.to_string());
            assert_eq!(memory.used(), 37);
        }
        for failure in 0..3 {
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let blocker = if failure == 0 {
                Some(memory.reserve(memory.available()).unwrap())
            } else {
                None
            };
            if failure == 1 {
                original.cancel();
            }
            if failure == 2 {
                invoking.cancel();
            }
            let control = ProductionControl::new(&memory, &original, &invoking);
            let error = if loaded {
                engine.loaded_query_sequence_with_control("counter", &control)
            } else {
                engine.query_sequence_with_control("counter", &control)
            }
            .unwrap_err();
            assert_eq!(
                error.sqlstate(),
                Some(if failure == 0 { "53200" } else { "57014" })
            );
            drop(blocker);
            assert_eq!(memory.used(), 37);
        }
    }
    assert_eq!(engine.nextval("counter").unwrap(), 41);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}
