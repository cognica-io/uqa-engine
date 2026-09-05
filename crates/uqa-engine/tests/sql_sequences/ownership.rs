//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::SequenceOwnerDependency;

fn sequence_exists(engine: &Engine, name: &str) -> bool {
    engine.sequence_state(name).unwrap().is_some()
}

#[test]
fn explicit_sequence_ownership_tracks_renames_drop_and_truncate() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE owner_table(id bigint, other bigint)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE SEQUENCE owned_ids START WITH 10 OWNED BY owner_table.id",
            &[],
        )
        .unwrap();
    let owner = engine
        .sequence_state("owned_ids")
        .unwrap()
        .unwrap()
        .1
        .owner
        .expect("sequence owner");
    assert_eq!(owner.dependency, SequenceOwnerDependency::Automatic);
    assert_eq!(
        engine
            .sql(
                "SELECT pg_get_serial_sequence('owner_table', 'id') AS sequence",
                &[],
            )
            .unwrap()
            .rows[0]["sequence"],
        Value::Str("public.owned_ids".into())
    );

    assert_eq!(engine.nextval("owned_ids").unwrap(), 10);
    engine
        .sql("TRUNCATE owner_table CONTINUE IDENTITY", &[])
        .unwrap();
    assert_eq!(engine.nextval("owned_ids").unwrap(), 11);
    engine
        .sql("TRUNCATE owner_table RESTART IDENTITY", &[])
        .unwrap();
    assert_eq!(engine.nextval("owned_ids").unwrap(), 10);

    engine
        .sql("ALTER SEQUENCE owned_ids OWNED BY owner_table.other", &[])
        .unwrap();
    engine
        .sql("ALTER TABLE owner_table RENAME TO renamed_owner", &[])
        .unwrap();
    engine
        .sql(
            "ALTER TABLE renamed_owner RENAME COLUMN other TO renamed_other",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(
                "SELECT pg_get_serial_sequence('renamed_owner', 'renamed_other') AS sequence",
                &[],
            )
            .unwrap()
            .rows[0]["sequence"],
        Value::Str("public.owned_ids".into())
    );
    engine
        .sql("ALTER TABLE renamed_owner DROP COLUMN id", &[])
        .unwrap();
    assert!(sequence_exists(&engine, "owned_ids"));
    engine
        .sql("ALTER TABLE renamed_owner DROP COLUMN renamed_other", &[])
        .unwrap();
    assert!(!sequence_exists(&engine, "owned_ids"));
}

#[test]
fn owned_by_none_detaches_without_creating_a_column_default() {
    let engine = Engine::new();
    engine
        .sql("CREATE TABLE detached_owner(id bigint)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE SEQUENCE detached_ids OWNED BY detached_owner.id",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE SEQUENCE retained_ids OWNED BY detached_owner.id",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(
                "INSERT INTO detached_owner (id) VALUES (DEFAULT) RETURNING id",
                &[],
            )
            .unwrap()
            .rows[0]["id"],
        Value::Null
    );
    engine
        .sql("ALTER SEQUENCE detached_ids OWNED BY NONE", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql(
                "SELECT pg_get_serial_sequence('detached_owner', 'id') AS sequence",
                &[],
            )
            .unwrap()
            .rows[0]["sequence"],
        Value::Str("public.retained_ids".into())
    );
    engine.sql("DROP TABLE detached_owner", &[]).unwrap();
    assert!(sequence_exists(&engine, "detached_ids"));
    assert!(!sequence_exists(&engine, "retained_ids"));
    engine.sql("DROP SEQUENCE detached_ids", &[]).unwrap();

    engine
        .sql("CREATE TABLE direct_owner(id bigint)", &[])
        .unwrap();
    engine
        .sql(
            "CREATE SEQUENCE directly_dropped OWNED BY direct_owner.id",
            &[],
        )
        .unwrap();
    engine.sql("DROP SEQUENCE directly_dropped", &[]).unwrap();
    assert!(engine.sql("SELECT id FROM direct_owner", &[]).is_ok());
}

#[test]
fn owner_drop_restrict_and_cascade_follow_sequence_dependents() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE cascade_table_owner(id bigint);
             CREATE SEQUENCE cascade_table_ids OWNED BY cascade_table_owner.id;
             CREATE TABLE cascade_table_dependent(value bigint DEFAULT nextval('cascade_table_ids'));
             CREATE VIEW cascade_table_view AS SELECT nextval('cascade_table_ids') AS value",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql("DROP TABLE cascade_table_owner", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    assert!(engine.try_has_table("cascade_table_owner").unwrap());
    assert!(sequence_exists(&engine, "cascade_table_ids"));
    engine
        .sql("DROP TABLE cascade_table_owner CASCADE", &[])
        .unwrap();
    assert!(!sequence_exists(&engine, "cascade_table_ids"));
    assert!(engine.try_has_table("cascade_table_dependent").unwrap());
    assert!(engine
        .column_default_expr("cascade_table_dependent", "value")
        .unwrap()
        .is_none());
    assert_eq!(
        engine
            .sql(
                "SELECT to_regclass('cascade_table_view') IS NULL AS dropped",
                &[],
            )
            .unwrap()
            .rows[0]["dropped"],
        Value::Bool(true)
    );

    engine
        .sql(
            "CREATE TABLE cascade_column_owner(id bigint, retained bigint);
             CREATE SEQUENCE cascade_column_ids OWNED BY cascade_column_owner.id;
             CREATE TABLE cascade_column_dependent(value bigint DEFAULT nextval('cascade_column_ids'));
             CREATE VIEW cascade_column_view AS SELECT nextval('cascade_column_ids') AS value",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(
                "ALTER TABLE cascade_column_owner DROP COLUMN id RESTRICT",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    assert!(sequence_exists(&engine, "cascade_column_ids"));
    engine
        .sql(
            "ALTER TABLE cascade_column_owner DROP COLUMN id CASCADE",
            &[],
        )
        .unwrap();
    assert!(!sequence_exists(&engine, "cascade_column_ids"));
    assert!(engine
        .column_default_expr("cascade_column_dependent", "value")
        .unwrap()
        .is_none());
    assert_eq!(
        engine
            .sql(
                "SELECT to_regclass('cascade_column_view') IS NULL AS dropped",
                &[],
            )
            .unwrap()
            .rows[0]["dropped"],
        Value::Bool(true)
    );
}

#[test]
fn sequence_owner_validation_is_atomic_and_matches_postgresql_states() {
    let engine = Engine::new();
    engine
        .sql("CREATE SCHEMA app; CREATE SCHEMA other", &[])
        .unwrap();
    engine
        .sql("CREATE TABLE app.owner_table(id bigint)", &[])
        .unwrap();
    engine.sql("CREATE SEQUENCE app.existing", &[]).unwrap();

    assert_eq!(
        engine
            .sql("SELECT pg_get_serial_sequence('app.absent', 'id')", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42P01")
    );
    assert_eq!(
        engine
            .sql(
                "SELECT pg_get_serial_sequence('app.owner_table', 'absent')",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );

    for (sql, state) in [
        (
            "CREATE SEQUENCE app.missing_table OWNED BY app.absent.id",
            "42P01",
        ),
        (
            "CREATE SEQUENCE app.missing_column OWNED BY app.owner_table.absent",
            "42703",
        ),
        (
            "CREATE SEQUENCE other.wrong_schema OWNED BY app.owner_table.id",
            "55000",
        ),
        (
            "CREATE SEQUENCE app.wrong_kind OWNED BY app.existing.last_value",
            "42809",
        ),
    ] {
        assert_eq!(engine.sql(sql, &[]).unwrap_err().sqlstate(), Some(state));
    }
    for sequence in ["missing_table", "missing_column", "wrong_kind"] {
        assert!(!sequence_exists(&engine, &format!("app.{sequence}")));
    }
    assert!(!sequence_exists(&engine, "other.wrong_schema"));

    engine
        .sql(
            "CREATE SEQUENCE IF NOT EXISTS app.existing OWNED BY app.absent.id",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "ALTER SEQUENCE IF EXISTS app.absent OWNED BY app.absent.id",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "CREATE SEQUENCE app.still_owned OWNED BY app.owner_table.id",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(
                "ALTER SEQUENCE app.still_owned OWNED BY app.owner_table.absent",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("42703")
    );
    engine.sql("DROP TABLE app.owner_table", &[]).unwrap();
    assert!(!sequence_exists(&engine, "app.still_owned"));
}

#[test]
fn pg_get_serial_sequence_has_postgresql_catalog_identity() {
    let engine = Engine::new();
    let result = engine
        .sql(
            "SELECT oid, prosrc, proisstrict, provolatile, proparallel, prorettype, proargtypes
             FROM pg_catalog.pg_proc WHERE oid = 1665",
            &[],
        )
        .unwrap();
    assert_eq!(result.rows.len(), 1);
    let row = &result.rows[0];
    assert_eq!(row["oid"], Value::Int(1665));
    assert_eq!(row["prosrc"], Value::Str("pg_get_serial_sequence".into()));
    assert_eq!(row["proisstrict"], Value::Bool(true));
    assert_eq!(row["provolatile"], Value::Str("s".into()));
    assert_eq!(row["proparallel"], Value::Str("s".into()));
    assert_eq!(row["prorettype"], Value::Int(25));
    assert_eq!(
        row["proargtypes"],
        Value::List(vec![Value::Int(25), Value::Int(25)])
    );
}

fn assert_owner_transaction_semantics(engine: &Engine) {
    engine
        .sql(
            "CREATE TABLE attach_owner(id bigint);
             CREATE TABLE detach_owner(id bigint);
             CREATE SEQUENCE attach_ids;
             CREATE SEQUENCE detach_ids OWNED BY detach_owner.id",
            &[],
        )
        .unwrap();

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE attach_ids OWNED BY attach_owner.id", &[])
        .unwrap();
    engine.sql("ROLLBACK", &[]).unwrap();
    engine.sql("DROP TABLE attach_owner", &[]).unwrap();
    assert!(sequence_exists(engine, "attach_ids"));

    engine.sql("BEGIN", &[]).unwrap();
    engine.sql("SAVEPOINT owner_boundary", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE detach_ids OWNED BY NONE", &[])
        .unwrap();
    engine
        .sql("ROLLBACK TO SAVEPOINT owner_boundary", &[])
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    engine.sql("DROP TABLE detach_owner", &[]).unwrap();
    assert!(!sequence_exists(engine, "detach_ids"));

    engine
        .sql(
            "CREATE TABLE committed_owner(id bigint);
             CREATE SEQUENCE committed_ids;
             CREATE TABLE committed_detach_owner(id bigint);
             CREATE SEQUENCE committed_detach_ids OWNED BY committed_detach_owner.id",
            &[],
        )
        .unwrap();
    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql(
            "ALTER SEQUENCE committed_ids OWNED BY committed_owner.id",
            &[],
        )
        .unwrap();
    engine.sql("DROP TABLE committed_owner", &[]).unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert!(!sequence_exists(engine, "committed_ids"));

    engine.sql("BEGIN", &[]).unwrap();
    engine
        .sql("ALTER SEQUENCE committed_detach_ids OWNED BY NONE", &[])
        .unwrap();
    engine
        .sql("DROP TABLE committed_detach_owner", &[])
        .unwrap();
    engine.sql("COMMIT", &[]).unwrap();
    assert!(sequence_exists(engine, "committed_detach_ids"));
}

#[test]
fn sequence_owner_changes_follow_transaction_and_savepoint_rollback() {
    assert_owner_transaction_semantics(&Engine::new());
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(&directory.path().join("owned-transactions.sqlite")).unwrap();
    assert_owner_transaction_semantics(&engine);
}

#[test]
fn sequence_ownership_survives_reopen_and_explicit_detach_stays_detached() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("owned-reopen.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql(
                "CREATE TABLE durable_owner(id bigint, detached bigint);
                 CREATE SEQUENCE durable_ids OWNED BY durable_owner.id;
                 CREATE SEQUENCE detached_ids OWNED BY durable_owner.detached;
                 ALTER SEQUENCE detached_ids OWNED BY NONE",
                &[],
            )
            .unwrap();
    }
    {
        let reopened = Engine::open(&database).unwrap();
        reopened
            .sql(
                "ALTER TABLE durable_owner RENAME COLUMN id TO renamed_id",
                &[],
            )
            .unwrap();
        reopened
            .sql("ALTER TABLE durable_owner DROP COLUMN renamed_id", &[])
            .unwrap();
        assert!(!sequence_exists(&reopened, "durable_ids"));
        reopened.sql("DROP TABLE durable_owner", &[]).unwrap();
        assert!(sequence_exists(&reopened, "detached_ids"));
    }
}

#[test]
fn reopening_a_legacy_catalog_rebuilds_serial_sequence_ownership() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("legacy-owned-reopen.sqlite");
    {
        let engine = Engine::open(&database).unwrap();
        engine
            .sql("CREATE TABLE legacy_owner(id serial)", &[])
            .unwrap();
    }
    let connection = ManagedConnection::open(&database).unwrap();
    connection
        .with(|database| {
            database.execute(
                "UPDATE _sequences
                 SET owner_table_object_id = NULL,
                     owner_column_object_id = NULL,
                     owner_dependency = NULL
                 WHERE schema_name = 'public'
                   AND relation_name = 'legacy_owner_id_seq'",
                [],
            )?;
            Ok(())
        })
        .unwrap();

    let reopened = Engine::open(&database).unwrap();
    let owner = reopened
        .sequence_state("legacy_owner_id_seq")
        .unwrap()
        .unwrap()
        .1
        .owner
        .expect("migrated serial owner");
    assert_eq!(owner.dependency, SequenceOwnerDependency::Automatic);
    reopened
        .sql("ALTER TABLE legacy_owner DROP COLUMN id", &[])
        .unwrap();
    assert!(!sequence_exists(&reopened, "legacy_owner_id_seq"));
}

#[test]
fn serial_and_identity_sequences_receive_their_postgresql_dependency_kinds() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_owner(
                serial_id serial,
                identity_id bigint GENERATED BY DEFAULT AS IDENTITY
             )",
            &[],
        )
        .unwrap();
    let serial_owner = engine
        .sequence_state("generated_owner_serial_id_seq")
        .unwrap()
        .unwrap()
        .1
        .owner
        .unwrap();
    let identity_owner = engine
        .sequence_state("generated_owner_identity_id_seq")
        .unwrap()
        .unwrap()
        .1
        .owner
        .unwrap();
    assert_eq!(serial_owner.dependency, SequenceOwnerDependency::Automatic);
    assert_eq!(identity_owner.dependency, SequenceOwnerDependency::Internal);
    assert_eq!(
        engine
            .sql(
                "ALTER SEQUENCE generated_owner_identity_id_seq OWNED BY NONE",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("0A000")
    );
    assert_eq!(
        engine
            .sql("DROP SEQUENCE generated_owner_identity_id_seq CASCADE", &[])
            .unwrap_err()
            .sqlstate(),
        Some("2BP01")
    );
    engine
        .sql("ALTER TABLE generated_owner DROP COLUMN identity_id", &[])
        .unwrap();
    assert!(!sequence_exists(&engine, "generated_owner_identity_id_seq"));
}

#[test]
fn generated_sequence_names_use_postgresql_truncation_and_collision_suffixes() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA generated_name;
             CREATE SEQUENCE generated_name.foreign_generated_sequence_na_identifier_that_is_also_delib_seq;
             CREATE TABLE generated_name.foreign_generated_sequence_name_that_is_deliberately_long(identifier_that_is_also_deliberately_long serial)",
            &[],
        )
        .unwrap();
    assert_eq!(
        engine
            .sql(
                "SELECT pg_get_serial_sequence('generated_name.foreign_generated_sequence_name_that_is_deliberately_long', 'identifier_that_is_also_deliberately_long') AS sequence",
                &[],
            )
            .unwrap()
            .rows[0]["sequence"],
        Value::Str(
            "generated_name.foreign_generated_sequence_na_identifier_that_is_also_deli_seq1"
                .into()
        )
    );

    engine.sql("CREATE SCHEMA pending_name", &[]).unwrap();
    assert_eq!(
        engine
            .sql(
                "CREATE TABLE pending_name.foreign_generated_sequence_name_that_is_deliberately_long(identifier_that_is_also_deliberately_long_a serial, identifier_that_is_also_deliberately_long_b serial)",
                &[],
            )
            .unwrap_err()
            .sqlstate(),
        Some("42P07")
    );
    assert_eq!(
        engine
            .sql(
                "SELECT count(*) AS relation_count FROM pg_catalog.pg_class AS relation_row JOIN pg_catalog.pg_namespace AS namespace_row ON namespace_row.oid = relation_row.relnamespace WHERE namespace_row.nspname = 'pending_name'",
                &[],
            )
            .unwrap()
            .rows[0]["relation_count"],
        Value::Int(0)
    );
}
