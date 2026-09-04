//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn sqlstate(engine: &Engine, sql: &str) -> String {
    engine
        .sql(sql, &[])
        .expect_err("statement must fail")
        .sqlstate()
        .expect("SQLSTATE")
        .to_string()
}

fn remove_routine_identity_fields(value: &mut serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(fields) => {
            let mut removed = usize::from(fields.remove("object_id").is_some());
            removed += usize::from(fields.remove("function_object_id").is_some());
            for value in fields.values_mut() {
                removed += remove_routine_identity_fields(value);
            }
            removed
        }
        serde_json::Value::Array(values) => {
            values.iter_mut().map(remove_routine_identity_fields).sum()
        }
        _ => 0,
    }
}

#[test]
fn routine_rename_preserves_oid_and_replace_identity() {
    let engine = Engine::new();
    for ddl in [
        "CREATE SCHEMA rename_identity",
        "CREATE FUNCTION rename_identity.oid_target(value integer) RETURNS integer RETURN value",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    let oid = scalar(
        &engine,
        "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'oid_target'",
    );
    engine
        .sql(
            "ALTER FUNCTION rename_identity.oid_target(integer) RENAME TO oid_renamed",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'oid_renamed'",
        ),
        oid
    );
    let Value::Int(oid_number) = oid else {
        panic!("routine OID must be an integer");
    };
    assert_eq!(
        scalar(
            &engine,
            "SELECT specific_name AS v FROM information_schema.routines WHERE specific_schema = 'rename_identity' AND routine_name = 'oid_renamed'",
        ),
        Value::Str(format!("oid_renamed_{oid_number}"))
    );
    engine
        .sql(
            "CREATE OR REPLACE FUNCTION rename_identity.oid_renamed(value integer) RETURNS integer RETURN value + 10",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            &engine,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'oid_renamed'",
        ),
        Value::Int(oid_number)
    );
    assert_eq!(
        scalar(&engine, "SELECT rename_identity.oid_renamed(2) AS v"),
        Value::Int(12)
    );
}

#[test]
fn routine_rename_resolves_overloads_kinds_ambiguity_and_transactions() {
    let engine = Engine::new();
    for ddl in [
        "CREATE SCHEMA rename_identity",
        "CREATE FUNCTION rename_identity.pick(value integer) RETURNS integer RETURN value + 1",
        "CREATE FUNCTION rename_identity.pick(value text) RETURNS text RETURN value || '!'",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    engine
        .sql(
            "ALTER FUNCTION rename_identity.pick(integer) RENAME TO chosen",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(&engine, "SELECT rename_identity.chosen(4) AS v"),
        Value::Int(5)
    );
    assert_eq!(
        scalar(&engine, "SELECT rename_identity.pick('x') AS v"),
        Value::Str("x!".into())
    );
    assert_eq!(sqlstate(&engine, "SELECT rename_identity.pick(4)"), "42883");

    engine
        .sql(
            "CREATE PROCEDURE rename_identity.proc(value integer) LANGUAGE plpgsql AS $$ BEGIN NULL; END $$",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER ROUTINE rename_identity.proc(integer) RENAME TO moved_proc",
            &[],
        )
        .unwrap();
    engine
        .sql("CALL rename_identity.moved_proc(1)", &[])
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER PROCEDURE rename_identity.chosen(integer) RENAME TO wrong_kind"
        ),
        "42809"
    );

    for ddl in [
        "CREATE FUNCTION rename_identity.ambiguous(value integer) RETURNS integer RETURN value",
        "CREATE FUNCTION rename_identity.ambiguous(value text) RETURNS text RETURN value",
        "CREATE FUNCTION rename_identity.unique_name(value integer) RETURNS integer RETURN value",
    ] {
        engine.sql(ddl, &[]).unwrap();
    }
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FUNCTION rename_identity.ambiguous RENAME TO impossible"
        ),
        "42725"
    );
    engine
        .sql(
            "ALTER FUNCTION rename_identity.unique_name RENAME TO unique_renamed",
            &[],
        )
        .unwrap();

    engine
        .sql(
            "CREATE FUNCTION rename_identity.collision(value integer) RETURNS integer RETURN value",
            &[],
        )
        .unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FUNCTION rename_identity.chosen(integer) RENAME TO collision"
        ),
        "42723"
    );
    assert_eq!(
        scalar(&engine, "SELECT rename_identity.chosen(8) AS v"),
        Value::Int(9)
    );

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER FUNCTION rename_identity.chosen(integer) RENAME TO rolled_back",
            &[],
        )
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    assert_eq!(
        scalar(&engine, "SELECT rename_identity.chosen(9) AS v"),
        Value::Int(10)
    );
    assert_eq!(
        sqlstate(&engine, "SELECT rename_identity.rolled_back(9)"),
        "42883"
    );
}

#[test]
fn routine_rename_requires_schema_create_before_collision_checks() {
    let engine = Engine::new();
    for ddl in [
        "CREATE ROLE routine_rename_owner",
        "CREATE SCHEMA routine_rename_acl",
        "GRANT USAGE, CREATE ON SCHEMA routine_rename_acl TO routine_rename_owner",
        "SET ROLE routine_rename_owner",
        "CREATE FUNCTION routine_rename_acl.source(value integer) RETURNS integer RETURN value",
        "CREATE FUNCTION routine_rename_acl.collision(value integer) RETURNS integer RETURN value",
        "RESET ROLE",
        "REVOKE CREATE ON SCHEMA routine_rename_acl FROM routine_rename_owner",
        "SET ROLE routine_rename_owner",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FUNCTION routine_rename_acl.source(integer) RENAME TO collision"
        ),
        "42501"
    );
    engine.sql("RESET ROLE", &[]).unwrap();
    engine
        .sql(
            "GRANT CREATE ON SCHEMA routine_rename_acl TO routine_rename_owner",
            &[],
        )
        .unwrap();
    engine.sql("SET ROLE routine_rename_owner", &[]).unwrap();
    assert_eq!(
        sqlstate(
            &engine,
            "ALTER FUNCTION routine_rename_acl.source(integer) RENAME TO collision"
        ),
        "42723"
    );
    engine
        .sql(
            "ALTER FUNCTION routine_rename_acl.source(integer) RENAME TO renamed",
            &[],
        )
        .unwrap();
}

fn assert_bound_dependents(engine: &Engine, dynamic_result: Option<i64>) {
    assert_eq!(
        scalar(engine, "SELECT rename_dep.standard_caller(4) AS v"),
        Value::Int(5)
    );
    assert_eq!(
        scalar(engine, "SELECT value AS v FROM rename_dep.bound_view"),
        Value::Int(7)
    );
    assert_eq!(
        scalar(
            engine,
            "SELECT output AS v FROM rename_dep.bound_table_view"
        ),
        Value::Int(7)
    );
    engine
        .sql(
            "INSERT INTO rename_dep.generated_source(id) VALUES (8)",
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT derived AS v FROM rename_dep.generated_source WHERE id = 8",
        ),
        Value::Int(9)
    );
    engine
        .sql("INSERT INTO rename_dep.rule_source(id) VALUES (10)", &[])
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT value AS v FROM rename_dep.rule_log ORDER BY value DESC LIMIT 1",
        ),
        Value::Int(11)
    );
    engine
        .sql("INSERT INTO rename_dep.trigger_source(id) VALUES (12)", &[])
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT id AS v FROM rename_dep.trigger_source ORDER BY id DESC LIMIT 1",
        ),
        Value::Int(13)
    );
    match dynamic_result {
        Some(expected) => assert_eq!(
            scalar(engine, "SELECT rename_dep.dynamic_caller(4) AS v"),
            Value::Int(expected)
        ),
        None => assert_eq!(
            sqlstate(engine, "SELECT rename_dep.dynamic_caller(4)"),
            "42883"
        ),
    }
    let generated = scalar(
        engine,
        "SELECT generation_expression AS v FROM information_schema.columns WHERE table_schema = 'rename_dep' AND table_name = 'generated_source' AND column_name = 'derived'",
    );
    assert!(matches!(generated, Value::Str(value) if value.contains("rename_dep.renamed_base")));
    let rule = scalar(
        engine,
        "SELECT pg_get_ruledef(oid, true) AS v FROM pg_catalog.pg_rewrite WHERE rulename = 'copy_value'",
    );
    assert!(matches!(rule, Value::Str(value) if value.contains("rename_dep.renamed_base")));
    let trigger = scalar(
        engine,
        "SELECT pg_get_triggerdef(oid, true) AS v FROM pg_catalog.pg_trigger WHERE tgname = 'increment_before'",
    );
    assert!(matches!(trigger, Value::Str(value) if value.contains("rename_dep.renamed_trigger")));
    let table_view = scalar(
        engine,
        "SELECT view_definition AS v FROM information_schema.views WHERE table_schema = 'rename_dep' AND table_name = 'bound_table_view'",
    );
    assert!(
        matches!(table_view, Value::Str(value) if value.contains("rename_dep.renamed_table_base"))
    );
}

struct DependentRoutineOids {
    base: Value,
    table_base: Value,
    trigger: Value,
}

fn create_bound_dependent_fixture(engine: &Engine) {
    for ddl in [
        "CREATE SCHEMA rename_dep",
        "CREATE FUNCTION rename_dep.base(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE FUNCTION rename_dep.standard_caller(value integer) RETURNS integer RETURN rename_dep.base(value)",
        "CREATE FUNCTION rename_dep.dynamic_caller(value integer) RETURNS integer LANGUAGE SQL AS 'SELECT rename_dep.base($1)'",
        "CREATE VIEW rename_dep.bound_view AS SELECT rename_dep.base(6) AS value",
        "CREATE FUNCTION rename_dep.table_base(value integer) RETURNS TABLE(output integer) LANGUAGE SQL AS 'SELECT $1 + 2'",
        "CREATE VIEW rename_dep.bound_table_view AS SELECT output FROM rename_dep.table_base(5)",
        "CREATE TABLE rename_dep.generated_source(id integer, derived integer GENERATED ALWAYS AS (rename_dep.base(id)) STORED)",
        "CREATE TABLE rename_dep.rule_source(id integer)",
        "CREATE TABLE rename_dep.rule_log(value integer)",
        "CREATE RULE copy_value AS ON INSERT TO rename_dep.rule_source DO ALSO INSERT INTO rename_dep.rule_log VALUES (rename_dep.base(NEW.id))",
        "CREATE TABLE rename_dep.trigger_source(id integer)",
        "CREATE FUNCTION rename_dep.increment_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.id := NEW.id + 1; RETURN NEW; END $$",
        "CREATE TRIGGER increment_before BEFORE INSERT ON rename_dep.trigger_source FOR EACH ROW EXECUTE FUNCTION rename_dep.increment_trigger()",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
}

fn dependent_routine_oids(engine: &Engine) -> DependentRoutineOids {
    DependentRoutineOids {
        base: scalar(
            engine,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'base'",
        ),
        table_base: scalar(
            engine,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'table_base'",
        ),
        trigger: scalar(
            engine,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'increment_trigger'",
        ),
    }
}

fn rename_bound_dependent_routines(engine: &Engine) {
    for ddl in [
        "ALTER FUNCTION rename_dep.base(integer) RENAME TO renamed_base",
        "ALTER FUNCTION rename_dep.table_base(integer) RENAME TO renamed_table_base",
        "ALTER FUNCTION rename_dep.increment_trigger() RENAME TO renamed_trigger",
    ] {
        engine.sql(ddl, &[]).unwrap();
    }
}

fn assert_renamed_routine_oids(engine: &Engine, expected: &DependentRoutineOids) {
    for (name, oid) in [
        ("renamed_base", &expected.base),
        ("renamed_table_base", &expected.table_base),
        ("renamed_trigger", &expected.trigger),
    ] {
        assert_eq!(
            scalar(
                engine,
                &format!("SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = '{name}'"),
            ),
            *oid
        );
    }
}

fn create_isolated_replacement(engine: &Engine, original_oid: &Value) {
    engine
        .sql(
            "CREATE FUNCTION rename_dep.base(value integer) RETURNS integer IMMUTABLE RETURN value + 100",
            &[],
        )
        .unwrap();
    assert_ne!(
        scalar(
            engine,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'base'",
        ),
        *original_oid
    );
}

#[test]
fn bound_dependents_follow_routine_identity_across_rename_and_reopen() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-rename.sqlite");
    let routine_oids;
    {
        let engine = Engine::open(&database).unwrap();
        create_bound_dependent_fixture(&engine);
        routine_oids = dependent_routine_oids(&engine);
        rename_bound_dependent_routines(&engine);
        assert_bound_dependents(&engine, None);
        assert_eq!(
            sqlstate(
                &engine,
                "DROP FUNCTION rename_dep.renamed_base(integer) RESTRICT"
            ),
            "2BP01"
        );
        create_isolated_replacement(&engine, &routine_oids.base);
        assert_bound_dependents(&engine, Some(104));
        assert_renamed_routine_oids(&engine, &routine_oids);
    }

    let reopened = Engine::open(&database).unwrap();
    assert_bound_dependents(&reopened, Some(104));
    assert_renamed_routine_oids(&reopened, &routine_oids);
}

fn create_legacy_migration_fixture(engine: &Engine) {
    for ddl in [
        "CREATE FUNCTION migration_base(value integer) RETURNS integer IMMUTABLE RETURN value + 1",
        "CREATE FUNCTION migration_caller(value integer) RETURNS integer RETURN migration_base(value)",
        "CREATE VIEW migration_view AS SELECT migration_base(2) AS value",
        "CREATE TABLE migration_generated(id integer, derived integer GENERATED ALWAYS AS (migration_base(id)) STORED)",
        "CREATE TABLE migration_rule_source(id integer)",
        "CREATE TABLE migration_rule_log(value integer)",
        "CREATE RULE migration_copy AS ON INSERT TO migration_rule_source DO ALSO INSERT INTO migration_rule_log VALUES (migration_base(NEW.id))",
        "CREATE TABLE migration_trigger_source(id integer)",
        "CREATE FUNCTION migration_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.id := NEW.id + 1; RETURN NEW; END $$",
        "CREATE TRIGGER migration_before BEFORE INSERT ON migration_trigger_source FOR EACH ROW EXECUTE FUNCTION migration_trigger()",
    ] {
        engine
            .sql(ddl, &[])
            .unwrap_or_else(|error| panic!("{ddl}: {error}"));
    }
}

fn remove_legacy_function_identities(catalog: &uqa_storage::Catalog) {
    let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
    let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert!(remove_routine_identity_fields(&mut definitions) >= 3);
    catalog
        .set_metadata(
            "sql_functions_json",
            &serde_json::to_string(&definitions).unwrap(),
        )
        .unwrap();
}

fn remove_legacy_view_identities(catalog: &uqa_storage::Catalog) {
    let mut views = catalog.load_views().unwrap();
    let view = views
        .iter_mut()
        .find(|view| view.relation.name == "migration_view")
        .unwrap();
    let mut definition: serde_json::Value = serde_json::from_str(&view.definition_json).unwrap();
    assert!(remove_routine_identity_fields(&mut definition) >= 1);
    view.definition_json = serde_json::to_string(&definition).unwrap();
    catalog.save_view(view).unwrap();
}

fn remove_legacy_generated_column_identities(catalog: &uqa_storage::Catalog) {
    let mut tables = catalog.load_tables().unwrap();
    let table = tables
        .iter_mut()
        .find(|table| table.relation.name == "migration_generated")
        .unwrap();
    let mut columns: serde_json::Value = serde_json::from_str(&table.columns_json).unwrap();
    assert!(remove_routine_identity_fields(&mut columns) >= 2);
    table.columns_json = serde_json::to_string(&columns).unwrap();
    catalog.save_table(table).unwrap();
}

fn remove_legacy_rule_and_trigger_identities(catalog: &uqa_storage::Catalog) {
    for metadata_key in ["sql_rules_json", "sql_triggers_json"] {
        let encoded = catalog.get_metadata(metadata_key).unwrap().unwrap();
        let mut metadata: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert!(remove_routine_identity_fields(&mut metadata) >= 1);
        if metadata_key == "sql_rules_json" {
            metadata["format_version"] = serde_json::json!(2);
        }
        catalog
            .set_metadata(metadata_key, &serde_json::to_string(&metadata).unwrap())
            .unwrap();
    }
}

fn make_catalog_legacy(database: &std::path::Path) {
    use uqa_storage::{Catalog, ManagedConnection};

    let catalog = Catalog::open(ManagedConnection::open(database).unwrap()).unwrap();
    remove_legacy_function_identities(&catalog);
    remove_legacy_view_identities(&catalog);
    remove_legacy_generated_column_identities(&catalog);
    remove_legacy_rule_and_trigger_identities(&catalog);
}

fn assert_migrated_dependents(engine: &Engine, generated: i64, rule: i64, trigger: i64) {
    assert_eq!(
        scalar(engine, "SELECT migration_caller(5) AS v"),
        Value::Int(6)
    );
    assert_eq!(
        scalar(engine, "SELECT value AS v FROM migration_view"),
        Value::Int(3)
    );
    engine
        .sql(
            &format!("INSERT INTO migration_generated(id) VALUES ({generated})"),
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            &format!("SELECT derived AS v FROM migration_generated WHERE id = {generated}"),
        ),
        Value::Int(generated + 1)
    );
    engine
        .sql(
            &format!("INSERT INTO migration_rule_source(id) VALUES ({rule})"),
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT value AS v FROM migration_rule_log ORDER BY value DESC LIMIT 1",
        ),
        Value::Int(rule + 1)
    );
    engine
        .sql(
            &format!("INSERT INTO migration_trigger_source(id) VALUES ({trigger})"),
            &[],
        )
        .unwrap();
    assert_eq!(
        scalar(
            engine,
            "SELECT id AS v FROM migration_trigger_source ORDER BY id DESC LIMIT 1",
        ),
        Value::Int(trigger + 1)
    );
}

fn rename_migrated_routines(engine: &Engine) -> Value {
    let migrated_oid = scalar(
        engine,
        "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'migration_base'",
    );
    assert_eq!(
        sqlstate(engine, "DROP FUNCTION migration_base(integer) RESTRICT"),
        "2BP01"
    );
    for ddl in [
        "ALTER FUNCTION migration_base(integer) RENAME TO migration_renamed",
        "ALTER FUNCTION migration_trigger() RENAME TO migration_trigger_renamed",
    ] {
        engine.sql(ddl, &[]).unwrap();
    }
    assert_eq!(
        scalar(engine, "SELECT migration_caller(7) AS v"),
        Value::Int(8)
    );
    migrated_oid
}

fn assert_migrated_identity_metadata(database: &std::path::Path) {
    use uqa_storage::{Catalog, ManagedConnection};

    let catalog = Catalog::open(ManagedConnection::open(database).unwrap()).unwrap();
    let functions = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
    let definitions: serde_json::Value = serde_json::from_str(&functions).unwrap();
    assert!(functions.contains("object_id"), "{definitions}");
    let view = catalog
        .load_views()
        .unwrap()
        .into_iter()
        .find(|view| view.relation.name == "migration_view")
        .unwrap();
    assert!(view.definition_json.contains("object_id"));
    let table = catalog
        .load_tables()
        .unwrap()
        .into_iter()
        .find(|table| table.relation.name == "migration_generated")
        .unwrap();
    assert!(table.columns_json.contains("object_id"));
    let rules = catalog.get_metadata("sql_rules_json").unwrap().unwrap();
    assert!(rules.contains(r#""format_version":3"#));
    assert!(rules.contains("object_id"));
    let triggers = catalog.get_metadata("sql_triggers_json").unwrap().unwrap();
    assert!(triggers.contains("function_object_id"));
}

#[test]
fn legacy_routine_catalog_gains_persistent_object_identities() {
    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-object-id-migration.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        create_legacy_migration_fixture(&engine);
    }
    make_catalog_legacy(&database);

    let migrated_oid;
    {
        let reopened = Engine::open(&database).unwrap();
        assert_migrated_dependents(&reopened, 6, 8, 10);
        migrated_oid = rename_migrated_routines(&reopened);
    }
    assert_migrated_identity_metadata(&database);

    let reopened = Engine::open(&database).unwrap();
    assert_eq!(
        scalar(
            &reopened,
            "SELECT oid AS v FROM pg_catalog.pg_proc WHERE proname = 'migration_renamed'",
        ),
        migrated_oid
    );
    assert_migrated_dependents(&reopened, 12, 14, 16);
}

#[test]
fn secondary_session_rejects_legacy_routine_metadata_without_repair() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-load-only.sqlite");
    let engine = Engine::open(&database).unwrap();
    engine
        .sql(
            "CREATE FUNCTION load_only(value integer) RETURNS integer RETURN value",
            &[],
        )
        .unwrap();
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
    let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
    assert!(remove_routine_identity_fields(&mut definitions) >= 1);
    let legacy = serde_json::to_string(&definitions).unwrap();
    catalog.set_metadata("sql_functions_json", &legacy).unwrap();

    let Err(error) = engine.new_session() else {
        panic!("secondary session must not repair legacy routine metadata");
    };
    assert!(error
        .to_string()
        .contains("initial-open object-identity migration"));
    assert_eq!(
        catalog.get_metadata("sql_functions_json").unwrap(),
        Some(legacy)
    );
}

#[test]
fn failed_initial_routine_migration_rolls_back_catalog_writes() {
    use uqa_storage::{Catalog, ManagedConnection};

    let directory = TempDir::new().unwrap();
    let database = directory.path().join("routine-migration-atomicity.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql("CREATE TABLE atomic_source(value integer)", &[])
            .unwrap();
        engine
            .sql(
                "CREATE FUNCTION atomic_trigger() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$",
                &[],
            )
            .unwrap();
        engine
            .sql(
                "CREATE TRIGGER atomic_before BEFORE INSERT ON atomic_source FOR EACH ROW EXECUTE FUNCTION atomic_trigger()",
                &[],
            )
            .unwrap();
    }
    let legacy_functions;
    {
        let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
        let encoded = catalog.get_metadata("sql_functions_json").unwrap().unwrap();
        let mut definitions: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert!(remove_routine_identity_fields(&mut definitions) >= 1);
        legacy_functions = serde_json::to_string(&definitions).unwrap();
        catalog
            .set_metadata("sql_functions_json", &legacy_functions)
            .unwrap();

        let encoded = catalog.get_metadata("sql_triggers_json").unwrap().unwrap();
        let mut triggers: serde_json::Value = serde_json::from_str(&encoded).unwrap();
        assert!(remove_routine_identity_fields(&mut triggers) >= 1);
        triggers["triggers"][0]["definition"]["function"] =
            serde_json::Value::String("public.missing_trigger".into());
        catalog
            .set_metadata(
                "sql_triggers_json",
                &serde_json::to_string(&triggers).unwrap(),
            )
            .unwrap();
    }

    let Err(error) = Engine::open(&database) else {
        panic!("invalid trigger must abort initial catalog migration");
    };
    assert!(error.to_string().contains("missing_trigger"));
    let catalog = Catalog::open(ManagedConnection::open(&database).unwrap()).unwrap();
    assert_eq!(
        catalog.get_metadata("sql_functions_json").unwrap(),
        Some(legacy_functions)
    );
}
