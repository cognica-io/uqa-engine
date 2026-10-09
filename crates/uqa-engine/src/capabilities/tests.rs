//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::StoredViewKind;
use uqa_core::Value;
use uqa_sql::{expr::EngineHook, ColumnType, SQLParam};

thread_local! {
    static CATALOG_CAPTURES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(super) fn record_catalog_capture() {
    CATALOG_CAPTURES.set(CATALOG_CAPTURES.get() + 1);
}

#[test]
fn privilege_readers_retain_registry_guards_and_table_generations() {
    use uqa_execution::catalog::security::table_inquiry::TablePrivilegeRegistry;

    let engine = Engine::new();
    engine
        .sql("CREATE TABLE inquiry_items (old_value integer)", &[])
        .unwrap();
    engine
        .sql("GRANT SELECT ON inquiry_items TO PUBLIC", &[])
        .unwrap();
    let relation = crate::RelationIdentity::new("public", "inquiry_items");
    let selected = {
        let registry = TablePrivilegeRegistry::tables(&engine);
        assert!(engine.storage.tables.try_write().is_none());
        assert!(registry.keys().any(|name| name == &relation));
        assert_eq!(
            registry.get(&relation).unwrap().column_names(),
            ["old_value"]
        );
        registry.retained(&relation).unwrap()
    };
    assert!(engine.storage.tables.try_write().is_some());
    let security = selected.security();
    assert!(security.acl.is_some());

    engine.sql("DROP TABLE inquiry_items", &[]).unwrap();
    engine
        .sql("CREATE TABLE inquiry_items (new_value text)", &[])
        .unwrap();
    let replacement = TablePrivilegeRegistry::tables(&engine)
        .retained(&relation)
        .unwrap();
    assert_eq!(replacement.column_names(), ["new_value"]);
    assert!(replacement.security().acl.is_none());
    assert_eq!(selected.column_names(), ["old_value"]);
    assert_eq!(selected.security(), security);
}

#[test]
fn catalog_views_share_schema_and_graph_allocations_until_mutation() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT)",
            &[],
        )
        .unwrap();
    engine.create_graph("shared_graph").unwrap();
    let first = engine.catalog_read_view();
    let second = engine.catalog_read_view();
    let relation = crate::RelationIdentity::new("public", "items");
    assert!(Arc::ptr_eq(
        &first.snapshot().tables[&relation].columns,
        &second.snapshot().tables[&relation].columns
    ));
    assert!(Arc::ptr_eq(
        &first.snapshot().definitions.graphs,
        &second.snapshot().definitions.graphs
    ));
    assert!(Arc::ptr_eq(
        &first.snapshot().definitions.schemas,
        &second.snapshot().definitions.schemas
    ));
    engine
        .sql("ALTER TABLE items ADD COLUMN extra INTEGER", &[])
        .unwrap();
    engine.create_graph("later_graph").unwrap();
    let current = engine.catalog_read_view();
    assert_eq!(first.snapshot().tables[&relation].columns.len(), 2);
    assert_eq!(current.snapshot().tables[&relation].columns.len(), 3);
    assert_eq!(first.graph_names(), vec!["shared_graph"]);
    assert_eq!(current.graph_names(), vec!["later_graph", "shared_graph"]);
}

#[test]
fn capability_views_expose_only_their_owned_state() {
    let engine = Engine::new();
    let catalog = engine.catalog_read_view();
    let session = engine.session_execution_view();
    let runtime = engine.query_runtime_view();
    assert!(catalog.has_schema("public"));
    assert_eq!(
        session.show_variable("search_path").unwrap(),
        "\"$user\", public"
    );
    assert_eq!(
        session
            .current_role()
            .require_name(&engine.durable.roles.read())
            .unwrap(),
        "uqa"
    );
    assert_eq!(
        session
            .session_role()
            .require_name(&engine.durable.roles.read())
            .unwrap(),
        "uqa"
    );
    assert_eq!(session.transaction_depth(), 0);
    assert_eq!(session.transaction_snapshot_identity(), None);
    assert_eq!(runtime.work_mem_bytes().unwrap(), 64 * 1024 * 1024);
    runtime.check_cancelled().unwrap();
}

#[test]
fn mutation_coordinator_publishes_schema_changes_without_engine_recovery() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();
    let before = engine.catalog_epochs();
    assert!(engine
        .mutation_coordinator()
        .register_schema(
            "capability_test",
            false,
            uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
            uqa_execution::schema::namespaces::identity::new_tuple(42_001).unwrap()
        )
        .unwrap());
    assert!(!snapshot.has_schema("capability_test"));
    assert!(engine.catalog_read_view().has_schema("capability_test"));
    let after = engine.catalog_epochs();
    assert_eq!(after.catalog_registry, before.catalog_registry);
    assert!(
        engine
            .epochs
            .catalog_registry
            .published
            .load(Ordering::Acquire)
            > before.catalog_registry
    );
}

#[test]
fn catalog_read_view_keeps_statement_relation_snapshot() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();
    engine
        .create_table(
            "snapshot_table",
            uqa_analysis::standard_analyzer("english"),
            Vec::new(),
        )
        .unwrap();
    let resolution = engine.session_execution_view().relation_name_resolution();
    assert_eq!(
        snapshot.table_name(&resolution, "snapshot_table").unwrap(),
        None
    );
    assert_eq!(
        engine
            .catalog_read_view()
            .table_name(&resolution, "snapshot_table")
            .unwrap()
            .as_deref(),
        Some("public.snapshot_table")
    );
}

#[test]
fn catalog_read_view_keeps_all_live_projection_families_on_one_snapshot() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();

    engine
        .sql("CREATE SEQUENCE snapshot_sequence", &[])
        .unwrap();
    engine
        .sql("CREATE VIEW snapshot_view AS SELECT 1 AS id", &[])
        .unwrap();
    engine.sql("CREATE ROLE snapshot_role", &[]).unwrap();
    engine.create_graph("snapshot_graph").unwrap();

    assert_eq!(snapshot.sequences().unwrap().len(), 0);
    assert!(snapshot.views_of_kind(StoredViewKind::View).is_empty());
    assert!(!snapshot.roles().any(|role| role.name == "snapshot_role"));
    assert_eq!(snapshot.graph_names().len(), 0);

    let current = engine.catalog_read_view();
    assert!(current
        .sequences()
        .unwrap()
        .iter()
        .any(|(name, _, _, _)| name == "public.snapshot_sequence"));
    assert!(current
        .views_of_kind(StoredViewKind::View)
        .iter()
        .any(|(name, _)| name == "public.snapshot_view"));
    assert!(current.roles().any(|role| role.name == "snapshot_role"));
    assert_eq!(current.graph_names(), vec!["snapshot_graph"]);
}

#[test]
fn catalog_read_view_keeps_routines_triggers_and_rules_on_one_snapshot() {
    let engine = Engine::new();
    let snapshot = engine.catalog_read_view();

    engine
        .sql(
            "CREATE TABLE snapshot_events (id INTEGER); CREATE FUNCTION snapshot_trigger_function() RETURNS trigger LANGUAGE plpgsql AS 'BEGIN RETURN NEW; END'; CREATE TRIGGER snapshot_trigger BEFORE INSERT ON snapshot_events FOR EACH ROW EXECUTE FUNCTION snapshot_trigger_function(); CREATE RULE snapshot_rule AS ON UPDATE TO snapshot_events DO ALSO NOTHING",
            &[],
        )
        .unwrap();

    assert!(snapshot.all_sql_functions().is_empty());
    assert!(snapshot.triggers().is_empty());
    assert!(snapshot.rules().is_empty());

    let current = engine.catalog_read_view();
    assert_eq!(current.all_sql_functions().len(), 1);
    assert!(current
        .triggers()
        .iter()
        .any(|trigger| trigger.definition.name == "snapshot_trigger"));
    assert!(current
        .rules()
        .iter()
        .any(|rule| rule.definition.name == "snapshot_rule"));
}

#[test]
fn relation_name_resolution_keeps_the_statement_search_path() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA first_path; CREATE SCHEMA second_path; CREATE TABLE first_path.items (id integer); CREATE TABLE second_path.items (id integer)",
            &[],
        )
        .unwrap();
    engine.set_variable("search_path", "first_path").unwrap();
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();

    engine.set_variable("search_path", "second_path").unwrap();
    let current_resolution = engine.session_execution_view().relation_name_resolution();

    assert_eq!(
        catalog.table_name(&resolution, "items").unwrap().as_deref(),
        Some("first_path.items")
    );
    assert_eq!(
        catalog
            .table_name(&current_resolution, "items")
            .unwrap()
            .as_deref(),
        Some("second_path.items")
    );
}

#[test]
fn relation_resolution_preserves_the_missing_namespace_outcome() {
    let engine = Engine::new();
    let catalog = engine.catalog_read_view();
    let resolution = engine.session_execution_view().relation_name_resolution();

    assert_eq!(
        catalog
            .relation_kind_resolution(&resolution, "missing_schema.items")
            .unwrap(),
        RelationResolution::MissingSchema("missing_schema".into())
    );
    assert_eq!(
        catalog
            .relation_kind_resolution(&resolution, "missing_items")
            .unwrap(),
        RelationResolution::MissingRelation
    );
    assert_eq!(
        catalog
            .relation_kind_resolution(&resolution, "pg_temp.missing_items")
            .unwrap(),
        RelationResolution::MissingSchema("pg_temp".into())
    );

    engine
        .sql(
            "CREATE TEMP TABLE allocate_temp_namespace (id integer)",
            &[],
        )
        .unwrap();
    let allocated = engine.session_execution_view().relation_name_resolution();
    assert_eq!(
        engine
            .catalog_read_view()
            .relation_kind_resolution(&allocated, "pg_temp.missing_items")
            .unwrap(),
        RelationResolution::MissingRelation
    );

    let mut bound = resolution;
    bound.set_lookup_mode(RelationLookupMode::Bound);
    assert_eq!(
        catalog
            .relation_kind_resolution(&bound, "missing_schema.items")
            .unwrap(),
        RelationResolution::MissingRelation
    );
}

#[rstest::rstest]
#[case::memory(false)]
#[case::sqlite(true)]
fn information_schema_filter_catalog_captures_do_not_scale_with_rows(#[case] persistent: bool) {
    let captures = [1, 32].map(|table_count| {
        let directory = tempfile::tempdir().unwrap();
        let engine = if persistent {
            Engine::open(&directory.path().join("catalog.db")).unwrap()
        } else {
            Engine::new()
        };
        for index in 0..table_count {
            engine
                .sql(
                    &format!("CREATE TABLE item_{index} (id integer, title text, active boolean)"),
                    &[],
                )
                .unwrap();
        }
        CATALOG_CAPTURES.set(0);
        let result = engine
            .sql(
                "SELECT column_name, data_type FROM information_schema.columns WHERE table_name = $1 ORDER BY ordinal_position",
                &[SQLParam::scalar(Value::Str("item_0".into()))],
            )
            .unwrap();
        let count = CATALOG_CAPTURES.get();
        assert_eq!(result.rows.len(), 3);
        for (row, (name, ty)) in result.rows.iter().zip([
            ("id", "integer"),
            ("title", "text"),
            ("active", "boolean"),
        ]) {
            assert_eq!(row["column_name"], Value::Str(name.into()));
            assert_eq!(row["data_type"], Value::Str(ty.into()));
        }
        count
    });
    assert!(captures[0] > 0, "the query must capture its catalog inputs");
    assert!(
        captures[1] <= captures[0],
        "catalog captures grew with unrelated rows: {captures:?}"
    );
}

#[test]
fn scoped_type_resolution_reuses_retained_catalog() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE DOMAIN positive AS integer CHECK (VALUE > 0); CREATE TYPE mood AS ENUM ('calm', 'busy'); CREATE TABLE items (id integer)",
            &[],
        )
        .unwrap();
    let scope = query_scope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &scope);
    let names = [
        "information_schema.sql_identifier",
        "information_schema.character_data",
        "positive",
        "positive[]",
        "mood",
        "items",
        "integer",
    ];
    CATALOG_CAPTURES.set(0);
    for _ in 0..32 {
        for name in names {
            assert!(EngineHook::resolve_type_name(&hook, name)
                .unwrap()
                .is_some());
            assert!(
                uqa_execution::FunctionTypeResolver::resolve_type_name(&hook, name)
                    .unwrap()
                    .is_some()
            );
        }
    }
    assert_eq!(CATALOG_CAPTURES.get(), 0);
}

#[test]
fn scoped_type_resolution_keeps_domain_identity_until_next_scope() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA first_path; CREATE SCHEMA second_path; CREATE DOMAIN first_path.item AS integer; CREATE DOMAIN second_path.item AS text; SET search_path = first_path, second_path",
            &[],
        )
        .unwrap();
    // PostgreSQL 18.4 independently returns 2 and 'value' for these domain casts.
    let values = engine
        .sql("SELECT (2::item)::integer AS numeric_value, ('value'::second_path.item)::text AS text_value", &[])
        .unwrap();
    assert_eq!(values.rows[0]["numeric_value"], Value::Int(2));
    assert_eq!(values.rows[0]["text_value"], Value::Str("value".into()));
    let scope = query_scope::new_for_current_routine(&engine);
    let hook = ScopedEngineHook::new(&engine, &scope);
    let original = EngineHook::resolve_type_name(&hook, "item")
        .unwrap()
        .unwrap();
    let qualified = EngineHook::resolve_type_name(&hook, "second_path.item")
        .unwrap()
        .unwrap();
    assert_ne!(original, qualified);
    engine
        .sql(
            "DROP DOMAIN first_path.item; CREATE DOMAIN first_path.item AS boolean",
            &[],
        )
        .unwrap();
    assert_eq!(
        EngineHook::resolve_type_name(&hook, "item").unwrap(),
        Some(original.clone())
    );
    assert_eq!(
        EngineHook::resolve_type_name(&hook, "item[]").unwrap(),
        Some(ColumnType::Array(Box::new(original.clone())))
    );
    assert_eq!(
        uqa_execution::FunctionTypeResolver::resolve_type_name(&hook, "item").unwrap(),
        Some(original.clone())
    );
    let current_scope = query_scope::new_for_current_routine(&engine);
    let current_hook = ScopedEngineHook::new(&engine, &current_scope);
    assert_ne!(
        EngineHook::resolve_type_name(&current_hook, "item").unwrap(),
        Some(original)
    );
    engine
        .sql("SET search_path = second_path, first_path", &[])
        .unwrap();
    let reordered_scope = query_scope::new_for_current_routine(&engine);
    let reordered_hook = ScopedEngineHook::new(&engine, &reordered_scope);
    assert_eq!(
        EngineHook::resolve_type_name(&reordered_hook, "item").unwrap(),
        Some(qualified)
    );
}

#[test]
fn domain_validation_casts_have_a_retained_catalog() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE DOMAIN positive AS integer; CREATE TABLE items (id positive); INSERT INTO items VALUES (1), (2); ALTER DOMAIN positive ADD CONSTRAINT valid CHECK ((VALUE::text)::integer > 0)",
            &[],
        )
        .unwrap();
    let error = engine
        .sql("INSERT INTO items VALUES (-1)", &[])
        .unwrap_err();
    assert_eq!(error.sqlstate(), Some("23514"));
    let result = engine.sql("SELECT id FROM items ORDER BY id", &[]).unwrap();
    assert_eq!(result.rows.len(), 2);
    assert_eq!(result.rows[0]["id"], Value::Int(1));
    assert_eq!(result.rows[1]["id"], Value::Int(2));
}
