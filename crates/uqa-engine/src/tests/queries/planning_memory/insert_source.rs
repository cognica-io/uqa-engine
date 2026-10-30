//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::memory::ProductionControl;
use uqa_execution::{
    mutation::{
        identity::IdentitySource,
        insert::source::{InsertSelectConsumer, InsertSelectIdentity},
    },
    query::CteScope,
};
use uqa_sql::plan::{CommandPlan, InsertPlan, RelationalPlan};

fn insert(sql: &str) -> Box<InsertPlan> {
    let UnifiedPlan::Command(command) =
        UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
    else {
        panic!("command")
    };
    let CommandPlan::Insert(insert) = *command else {
        panic!("insert")
    };
    insert
}

fn capture_inputs(large_statement: bool) -> (Box<InsertPlan>, Vec<uqa_sql::SQLParam>) {
    let mut statement = insert("INSERT INTO public.items(payload) SELECT $1");
    let params = if large_statement {
        let RelationalPlan::QueryBlock(block) = &mut statement.source.as_mut().unwrap().root else {
            panic!("query block")
        };
        block.projections[0].expr = uqa_sql::ScalarExpr::Literal(Value::Str("x".repeat(4 << 20)));
        vec![uqa_sql::SQLParam::Scalar(Value::Str("small".into()))]
    } else {
        vec![uqa_sql::SQLParam::Scalar(Value::Str("x".repeat(4 << 20)))]
    };
    (statement, params)
}

fn verify_capture(provider: Provider, large_statement: bool) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("insert-source-capture.db"));
    engine.sql("CREATE TABLE items(payload text)", &[]).unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let context = engine.mutation_statement_context();
    let services = context.insert_source();
    let (statement, params) = capture_inputs(large_statement);
    let scope = CteScope::new();
    let identity = InsertSelectIdentity {
        auto_id_column: None,
        id_column: "id".into(),
        identity_source: IdentitySource::Generated,
    };
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let mut result = None;
    let measured = allocation_counter::measure(|| {
        result = Some(InsertSelectConsumer::new(
            services,
            &statement,
            &params,
            scope,
            &identity,
            &[],
            &control,
        ));
    });
    assert!(
        measured.bytes_total < 128 << 10,
        "{provider:?}, statement={large_statement}: {measured:?}"
    );
    assert_eq!(
        result
            .unwrap()
            .err()
            .expect("the complete private input must be admitted")
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    drop(occupied);
    assert_eq!(memory.used(), baseline);
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(
            InsertSelectConsumer::new(
                services,
                &statement,
                &params,
                CteScope::new(),
                &identity,
                &[],
                &control
            )
            .err()
            .unwrap()
            .sqlstate(),
            Some("57014"),
        );
        assert_eq!(memory.used(), baseline);
        token.reset();
    }
    let captured = std::rc::Rc::new(
        InsertSelectConsumer::new(
            services,
            &statement,
            &params,
            CteScope::new(),
            &identity,
            &[],
            &control,
        )
        .unwrap(),
    );
    let retained = {
        let state = captured.state.borrow();
        let bytes = state.stmt.reserved_bytes()
            + state.params.reserved_bytes()
            + state.id_column.reserved_bytes()
            + state.conflict_update_columns.reserved_bytes();
        assert!(bytes >= 4 << 20);
        assert_eq!(memory.used(), baseline + bytes);
        bytes
    };
    let sibling = std::rc::Rc::clone(&captured);
    drop(captured);
    assert_eq!(memory.used(), baseline + retained);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), retained);
    drop(sibling);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_insert_select_capture_admits_private_statement_before_copy(#[case] provider: Provider) {
    verify_capture(provider, true);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_insert_select_capture_admits_private_parameters_before_copy(#[case] provider: Provider) {
    verify_capture(provider, false);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_insert_select_borrows_explicit_targets_and_publishes_complete_source_shape(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("insert-source-targets.db"));
    engine
        .sql("CREATE TABLE items(payload integer[])", &[])
        .unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let context = engine.mutation_statement_context();
    let services = context.insert_source();
    let statement = insert(&format!(
        "INSERT INTO public.items(payload[coalesce(1,length('{}'))]) SELECT 7",
        "x".repeat(4 << 20)
    ));
    let identity = InsertSelectIdentity {
        auto_id_column: None,
        id_column: "id".into(),
        identity_source: IdentitySource::Generated,
    };
    let baseline = memory.used();
    let consumer = InsertSelectConsumer::new(
        services,
        &statement,
        &[],
        CteScope::new(),
        &identity,
        &[],
        &control,
    )
    .unwrap();
    let captured = memory.used();
    let schema =
        uqa_sql::RowSchema::with_types(vec!["n".into()], vec![Some(uqa_sql::ColumnType::Integer)]);
    let source_columns = vec!["n".into()];
    let occupied = memory.reserve(memory.available()).unwrap();
    assert_eq!(
        consumer
            .begin(services, &source_columns, &schema)
            .unwrap_err()
            .sqlstate(),
        Some("53200")
    );
    assert_eq!(consumer.state.borrow().result_width, None);
    assert!(consumer.state.borrow().supplied_columns.is_empty());
    assert!(consumer.state.borrow().statement_relation.is_empty());
    assert_eq!(memory.used(), captured + occupied.bytes());
    drop(occupied);
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let allocation = allocation_counter::measure(|| {
        consumer.begin(services, &source_columns, &schema).unwrap();
    });
    assert!(
        allocation.bytes_total < 128 << 10,
        "{provider:?}: {allocation:?}"
    );
    let bound = memory.used();
    assert_eq!(
        bound - captured - occupied.bytes(),
        usize::try_from(allocation.bytes_current).unwrap()
    );
    assert_eq!(consumer.state.borrow().result_width, Some(1));
    assert_eq!(&**consumer.state.borrow().supplied_columns, &["payload"]);
    assert_eq!(
        &**consumer.state.borrow().statement_relation,
        "public.items"
    );
    consumer.begin(services, &source_columns, &schema).unwrap();
    assert_eq!(memory.used(), bound);
    let error = consumer
        .begin(services, &["n".into(), "extra".into()], &schema)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("INSERT SELECT width 2 != column count 1"));
    assert_eq!(memory.used(), bound);
    assert_eq!(consumer.state.borrow().result_width, Some(1));
    for token in [resources.cancellation(), runtime.cancellation] {
        token.cancel();
        assert_eq!(
            consumer
                .begin(services, &source_columns, &schema)
                .unwrap_err()
                .sqlstate(),
            Some("57014")
        );
        assert_eq!(memory.used(), bound);
        token.reset();
    }
    consumer.begin(services, &source_columns, &schema).unwrap();
    assert_eq!(memory.used(), bound);
    drop(occupied);
    drop(consumer);
    assert_eq!(memory.used(), baseline);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_insert_source_names_precede_default_observation_and_release_on_failure(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let engine = provider.open(&directory.path().join("insert-source-names.db"));
    engine
        .sql("CREATE TABLE items(payload integer)", &[])
        .unwrap();
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    let runtime = engine.query_runtime_view();
    let control = ProductionControl::new(memory, resources.cancellation(), runtime.cancellation);
    let context = engine.mutation_statement_context();
    let services = context.insert_source();
    let identity = InsertSelectIdentity {
        auto_id_column: None,
        id_column: "id".into(),
        identity_source: IdentitySource::Generated,
    };
    let statement = insert("INSERT INTO public.items SELECT 7");
    let consumer = InsertSelectConsumer::new(
        services,
        &statement,
        &[],
        CteScope::new(),
        &identity,
        &[],
        &control,
    )
    .unwrap();
    let table =
        engine.storage.tables.read()[&uqa_core::RelationIdentity::new("public", "items")].clone();
    let uqa_sql::Statement::CreateTable(mut missing) =
        uqa_sql::compile("CREATE TABLE t(n bigint DEFAULT nextval('missing_sequence'))")
            .unwrap()
            .remove(0)
    else {
        panic!("table")
    };
    table.columns.write()[0].default = missing.columns.remove(0).default;
    let mut visit = |name: &str| {
        assert!(!table.columns.is_locked());
        assert_eq!(name, "payload");
        table.columns.write()[0].name = "changed".into();
        assert_eq!(name, "payload");
        Ok(())
    };
    uqa_sql::semantics::mutation_rows::MutationRowCatalog::visit_column_names_with_control(
        &engine,
        "public.items",
        &control,
        &mut visit,
    )
    .unwrap();
    table.columns.write()[0].name = "payload".into();
    uqa_sql::semantics::returning::ReturningCatalog::visit_column_names_with_control(
        &engine,
        "public.items",
        &control,
        &mut visit,
    )
    .unwrap();
    table.columns.write()[0].name = "x".repeat(4 << 20);
    let baseline = memory.used();
    let occupied = memory.reserve(memory.available() - (64 << 10)).unwrap();
    let schema =
        uqa_sql::RowSchema::with_types(vec!["n".into()], vec![Some(uqa_sql::ColumnType::Integer)]);
    let source_columns = vec!["n".into()];
    let allocation = allocation_counter::measure(|| {
        assert_eq!(
            consumer
                .begin(services, &source_columns, &schema)
                .unwrap_err()
                .sqlstate(),
            Some("53200")
        );
    });
    assert!(
        allocation.bytes_total < 128 << 10,
        "{provider:?}: {allocation:?}"
    );
    assert_eq!(memory.used(), baseline + occupied.bytes());
    assert_eq!(consumer.state.borrow().result_width, None);
    assert!(consumer.state.borrow().statement_relation.is_empty());
    drop(occupied);
    table.columns.write()[0].name = "payload".into();
    let ordinary = engine
        .describe_table_in_execution("public.items")
        .unwrap_err();
    let uqa_sql::SQLError::Internal(message) = consumer
        .begin(services, &source_columns, &schema)
        .unwrap_err()
    else {
        panic!("storage diagnostic")
    };
    assert_eq!(
        message,
        format!("INSERT SELECT failed in storage backend: {ordinary}")
    );
    assert_eq!(memory.used(), baseline);
    assert_eq!(consumer.state.borrow().result_width, None);
    table.columns.write()[0].default = None;
    consumer.begin(services, &source_columns, &schema).unwrap();
    assert_eq!(consumer.state.borrow().result_width, Some(1));
    assert_eq!(&**consumer.state.borrow().supplied_columns, &["payload"]);
    let bound = memory.used();
    table.columns.write()[0].name = "changed".into();
    let uqa_sql::SQLError::Internal(message) = consumer
        .begin(services, &source_columns, &schema)
        .unwrap_err()
    else {
        panic!("source shape diagnostic")
    };
    assert_eq!(
        message,
        "INSERT SELECT row consumer was rebound to a different source shape"
    );
    assert_eq!(consumer.state.borrow().result_width, Some(1));
    assert_eq!(&**consumer.state.borrow().supplied_columns, &["payload"]);
    assert_eq!(memory.used(), bound);
    table.columns.write()[0].name = "payload".into();
    consumer.begin(services, &source_columns, &schema).unwrap();
    assert_eq!(memory.used(), bound);
    drop(consumer);
    drop(table);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
}

#[rstest::rstest]
#[case::memory(Provider::Memory)]
#[case::sqlite_native(Provider::SQLiteNative)]
#[case::sqlite_kv(Provider::SQLiteKeyValue)]
#[case::redb(Provider::Redb)]
fn native_insert_source_preserves_lazy_targets_implicit_defaults_and_prepared_effects(
    #[case] provider: Provider,
) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("insert-source-effects.db");
    let engine = provider.open(&path);
    engine.sql("CREATE TABLE items(id integer GENERATED ALWAYS AS IDENTITY, payload integer[]); CREATE SEQUENCE untouched; CREATE TABLE implicit_items(payload integer,note text DEFAULT 'kept')", &[]).unwrap();
    let inserted = engine.sql("INSERT INTO items(payload[coalesce(1,nextval('untouched')::integer)]) SELECT 7 RETURNING id,payload::text", &[]).unwrap();
    assert_eq!(inserted.value_at(0, 0), Some(&Value::Int(1)));
    assert_eq!(inserted.value_at(0, 1), Some(&Value::Str("{7}".into())));
    assert_eq!(
        engine
            .sql("SELECT currval('untouched')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("55000")
    );
    engine.sql("INSERT INTO implicit_items SELECT 8; INSERT INTO implicit_items VALUES(9); MERGE INTO implicit_items USING(VALUES(10)) source(n) ON false WHEN NOT MATCHED THEN INSERT VALUES(source.n)", &[]).unwrap();
    engine.sql("PREPARE saved(integer) AS INSERT INTO implicit_items SELECT $1 RETURNING payload,note; SET plan_cache_mode=force_generic_plan; EXECUTE saved(11); SET plan_cache_mode=force_custom_plan; EXECUTE saved(12)", &[]).unwrap();
    engine.sql("BEGIN; SAVEPOINT before_change; INSERT INTO implicit_items SELECT 99; ROLLBACK TO before_change; COMMIT; EXPLAIN(MEMORY,FORMAT JSON) INSERT INTO implicit_items SELECT 100; DEALLOCATE saved", &[]).unwrap();
    let expected = (8..=12)
        .map(|payload| {
            std::collections::BTreeMap::from([
                ("payload".into(), Value::Int(payload)),
                ("note".into(), Value::Str("kept".into())),
            ])
        })
        .collect::<Vec<_>>();
    assert_eq!(
        engine
            .sql(
                "SELECT payload,note FROM implicit_items ORDER BY payload",
                &[]
            )
            .unwrap()
            .rows,
        expected
    );
    drop(inserted);
    let resources = engine.query_retention_control().unwrap();
    let memory = resources.memory();
    engine.close().unwrap();
    drop(engine);
    assert_eq!(memory.used(), 0);
    if !matches!(provider, Provider::Memory) {
        let engine = provider.open(&path);
        assert_eq!(
            engine
                .sql(
                    "SELECT payload,note FROM implicit_items ORDER BY payload",
                    &[]
                )
                .unwrap()
                .rows,
            expected
        );
        assert_eq!(
            engine
                .sql("SELECT payload::text FROM items", &[])
                .unwrap()
                .value_at(0, 0),
            Some(&Value::Str("{7}".into()))
        );
        engine.close().unwrap();
    }
}
