//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn repository_named_indexes_finish_empty_and_populated_registration_before_activation() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE empty_values(body text); CREATE INDEX empty_expression ON empty_values((lower(body))); CREATE TABLE populated_values(body text); CREATE INDEX populated_expression ON populated_values((lower(body))); INSERT INTO populated_values VALUES('Saved'),(NULL)", &[]).unwrap();
    let expected = image(&source);
    let query = "SELECT lower(body) AS value FROM populated_values ORDER BY value NULLS FIRST";
    let expected_rows = source.sql(query, &[]).unwrap().rows;
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut names = std::collections::BTreeMap::new();
    let state = loop {
        if let Some(state) = input.advance(1).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            if let DecodedMemoryImageAddress::Table {
                unit: DecodedMemoryTableUnit::ValueIndex { name, document },
                ..
            } = unit.address
            {
                let counts = names
                    .entry((name.to_string(), *document))
                    .or_insert((0u64, 0u64));
                counts.0 += 1;
                counts.1 = unit.decoded_bytes;
            }
        }
    };
    assert_eq!(
        names
            .keys()
            .filter(|(_, document)| document.is_none())
            .count(),
        2
    );
    assert_eq!(
        names
            .keys()
            .filter(|(_, document)| document.is_some())
            .count(),
        2
    );
    for (calls, bytes) in names.values() {
        assert!(*calls > 1 && *bytes > 1);
        assert!(bytes <= calls);
    }
    drop(input);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    drop(repository);
    control.cancellation().cancel();
    assert_eq!(image(&restored), expected);
    assert_eq!(restored.sql(query, &[]).unwrap().rows, expected_rows);
    restored.sql("BEGIN; INSERT INTO empty_values VALUES('discarded'); UPDATE populated_values SET body='discarded'; ROLLBACK; INSERT INTO empty_values VALUES('Later'); INSERT INTO populated_values VALUES('New'); CREATE INDEX later_expression ON empty_values((upper(body)))", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT lower(body) AS value FROM empty_values", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Str("later".into())
    );
    assert_eq!(restored.sql(query, &[]).unwrap().rows.len(), 3);
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn repository_evaluated_index_input_obeys_current_windows_and_preserves_writes() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TABLE items(id integer PRIMARY KEY, body text); CREATE INDEX items_body ON items((lower(body))); INSERT INTO items VALUES(1, repeat('한글', 8192))", &[]).unwrap();
    let expected = image(&source);
    let rows = source.sql("SELECT * FROM items", &[]).unwrap().rows;
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let selected = |address: &DecodedMemoryImageAddress| match address {
        DecodedMemoryImageAddress::Table {
            unit: DecodedMemoryTableUnit::ValueIndex { document, .. },
            ..
        } => Some(usize::from(document.is_some())),
        _ => None,
    };
    let mut calls = [0_u64; 2];
    let mut allowed = [0_u64; 2];
    let mut lengths = [0_u64; 2];
    let state = loop {
        let before = input
            .image
            .as_ref()
            .unwrap()
            .current_unit()
            .and_then(|unit| selected(unit.address));
        let maximum = if before.is_some_and(|index| calls[index] % 2 == 0) {
            1
        } else {
            17
        };
        if let Some(state) = input.advance(maximum).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            if let Some(index) = selected(unit.address) {
                // A completed component can be replaced on this call; attribute its allowance to the returned component.
                calls[index] += 1;
                allowed[index] += maximum as u64;
                lengths[index] = unit.decoded_bytes;
            }
        }
    };
    for index in 0..2 {
        assert!(lengths[index] > 17 && calls[index] > 1);
        assert!(
            lengths[index] <= allowed[index],
            "component {index}: {} bytes with {} bytes of call allowance",
            lengths[index],
            allowed[index]
        );
    }
    drop(input);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
    drop(repository);
    control.cancellation().cancel();
    assert_eq!(image(&restored), expected);
    assert_eq!(restored.sql("SELECT * FROM items", &[]).unwrap().rows, rows);
    restored.sql("BEGIN; UPDATE items SET body='discarded'; ROLLBACK; INSERT INTO items VALUES(2,'later'); UPDATE items SET body='changed' WHERE id=1", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT id FROM items WHERE lower(body)='changed'", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(1)
    );
    assert_eq!(
        restored
            .sql("SELECT id FROM items WHERE lower(body)='later'", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(2)
    );
    drop(restored);
    assert_eq!(control.memory().used(), 0);
}

fn source_with_catalog_entries() -> Engine {
    let source = Engine::new();
    for index in 0..7 {
        source.sql(&format!("CREATE ROLE entry_{index}; CREATE SCHEMA entry_{index} AUTHORIZATION entry_{index}"), &[]).unwrap();
    }
    source.sql("CREATE SEQUENCE entry_sequence; GRANT entry_0 TO entry_1; GRANT CONNECT ON DATABASE uqa TO entry_0; GRANT EXECUTE ON FUNCTION pg_catalog.lower(text) TO entry_0; GRANT SELECT(relname) ON pg_class TO entry_0; CREATE FOREIGN DATA WRAPPER saved_wrapper; CREATE SERVER saved_server FOREIGN DATA WRAPPER memory_fdw; SELECT create_graph('saved_oids'); CREATE TABLE saved_table(id integer PRIMARY KEY); CREATE TYPE saved_type AS ENUM ('low', 'high'); COMMENT ON TYPE saved_type IS 'native comment'; CREATE DOMAIN saved_domain AS integer DEFAULT 11 CHECK (VALUE > 0); CREATE FUNCTION saved_trigger_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.id := NEW.id + 100; RETURN NEW; END $$; CREATE TRIGGER saved_trigger BEFORE INSERT ON saved_table FOR EACH ROW WHEN (NEW.id > 0) EXECUTE FUNCTION saved_trigger_fn()", &[]).unwrap();
    source.sql("CREATE FOREIGN TABLE saved_foreign(id integer DEFAULT 17 CHECK(id > 0), payload text, obsolete integer) SERVER saved_server; ALTER FOREIGN TABLE saved_foreign DROP COLUMN obsolete; GRANT SELECT(payload) ON saved_foreign TO entry_0", &[]).unwrap();
    source.sql("CREATE VIEW saved_view AS SELECT id, row_number() OVER (ORDER BY id) AS position FROM saved_table; CREATE MATERIALIZED VIEW saved_materialized AS SELECT 'original 한글'::text AS body; GRANT SELECT(body) ON saved_materialized TO entry_0", &[]).unwrap();
    source.sql("CREATE FUNCTION saved_bound(a integer) RETURNS integer LANGUAGE SQL IMMUTABLE RETURN a + 7; CREATE TABLE saved_audit(id integer); CREATE RULE saved_rule AS ON UPDATE TO saved_table WHERE NEW.id > OLD.id DO ALSO INSERT INTO saved_audit VALUES(NEW.id)", &[]).unwrap();
    source
        .save_scoring_params("signal", "{ \"x\": 1.00, \"x\": 2 }")
        .unwrap();
    source
}

#[test]
fn repository_catalog_steps_publish_only_complete_generations() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = source_with_catalog_entries();
    let expected_counts = (
        source.durable.schemas.read().len(),
        source.durable.roles.read().len(),
    );
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut counts = (0, 0);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            let candidate = input.candidate.as_ref().unwrap();
            let published = match unit.address {
                DecodedMemoryImageAddress::Catalog(6) => {
                    counts.0 += 1;
                    candidate
                        .durable
                        .schemas
                        .read()
                        .iter()
                        .filter(|(key, _)| key.starts_with("entry_"))
                        .count()
                }
                DecodedMemoryImageAddress::Catalog(9) => {
                    counts.1 += 1;
                    candidate
                        .durable
                        .roles
                        .read()
                        .iter()
                        .filter(|(key, _)| key.starts_with("entry_"))
                        .count()
                }
                _ => continue,
            };
            assert!(
                published == 0 || published == 7,
                "partial catalog generation became visible"
            );
        }
    };
    assert_eq!(counts, expected_counts);
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    assert_eq!(image(&restored), expected);
    restored.sql("CREATE SCHEMA after_restore; CREATE TABLE entry_0.saved(value integer); INSERT INTO entry_0.saved VALUES(17)", &[]).unwrap();
    drop((input, restored));
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn repository_catalog_steps_retire_original_input_between_entries() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = source_with_catalog_entries();
    let root = publish(&source, &repository);
    drop(source);
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        loop {
            assert!(input.advance(1024).unwrap().is_none());
            if matches!(
                input
                    .image
                    .as_ref()
                    .unwrap()
                    .current_unit()
                    .map(|unit| unit.address),
                Some(DecodedMemoryImageAddress::Catalog(6))
            ) {
                break;
            }
        }
        let full = if cancel {
            control.cancellation().cancel();
            None
        } else {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        };
        assert!(input.advance(1024).is_err());
        drop(full);
        assert_eq!(control.memory().used(), 0);
        assert!(matches!(input.advance(1024), Err(BackupError::Finished)));
        assert!(input.image.is_none());
        assert!(input.candidate.is_none());
    }
    repository
        .remove_backup(root.backup(), &StorageReadControl::with_limit(1 << 20))
        .unwrap();
}

#[test]
fn repository_catalog_decoder_obeys_the_current_call_byte_window() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = source_with_catalog_entries();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let mut calls = [0_u64; 34];
    let mut lengths = [0_u64; 34];
    let component = |address: &DecodedMemoryImageAddress| match address {
        DecodedMemoryImageAddress::Catalog(slot @ 1..=24) => Some(usize::from(*slot) - 1),
        DecodedMemoryImageAddress::Table {
            relation,
            unit: DecodedMemoryTableUnit::Metadata(slot @ 0..=9),
            ..
        } if &**relation == "saved_table" => Some(24 + usize::from(*slot)),
        _ => None,
    };
    let state = loop {
        let before = input
            .image
            .as_ref()
            .unwrap()
            .current_unit()
            .and_then(|unit| component(unit.address));
        // The public limit belongs to this call, including an already admitted component.
        let maximum = if before.is_some() { 1 } else { 7 };
        if let Some(state) = input.advance(maximum).unwrap() {
            break state;
        }
        if let Some(unit) = input.image.as_ref().unwrap().current_unit() {
            if let Some(index) = component(unit.address) {
                calls[index] += 1;
                lengths[index] = unit.decoded_bytes;
            }
        }
    };
    for (calls, length) in calls.into_iter().zip(lengths) {
        assert!(calls > 0 && length >= 16);
        // The inventory length is fixed, not a read-position counter. At most seven bytes
        // enter on the first call and one on each subsequent call for this component.
        assert!(length < 7 + calls, "{length} bytes in {calls} calls");
    }
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    assert_eq!(image(&restored), expected);
    check_restored_table_values(&restored);
    check_restored_view_routine_and_rule_edits(&restored);
    drop((input, restored));
    assert_eq!(control.memory().used(), 0);
}

fn check_restored_table_values(restored: &Engine) {
    restored
        .sql(
            "CREATE ROLE later; CREATE SCHEMA later AUTHORIZATION later",
            &[],
        )
        .unwrap();
    assert_eq!(
        restored
            .sql("INSERT INTO saved_table VALUES(1) RETURNING id", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(101)
    );
    assert_eq!(
        restored
            .sql("INSERT INTO saved_table VALUES(0) RETURNING id", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(0)
    );
    restored.sql("CREATE TABLE domain_after_restore(value saved_domain); INSERT INTO domain_after_restore DEFAULT VALUES", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT value FROM domain_after_restore", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(11)
    );
    assert_eq!(
        restored
            .sql("INSERT INTO domain_after_restore VALUES(-1)", &[])
            .unwrap_err()
            .sqlstate(),
        Some("23514")
    );
}

fn check_restored_view_routine_and_rule_edits(restored: &Engine) {
    assert_eq!(
        restored
            .sql("SELECT body FROM saved_materialized", &[])
            .unwrap()
            .rows[0]["body"],
        Value::Str("original 한글".into())
    );
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM saved_view", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(2)
    );
    assert_eq!(
        restored
            .sql("SELECT saved_bound(3) AS result", &[])
            .unwrap()
            .rows[0]["result"],
        Value::Int(10)
    );
    restored
        .sql("UPDATE saved_table SET id=2 WHERE id=0", &[])
        .unwrap();
    assert_eq!(
        restored
            .sql("SELECT id FROM saved_audit", &[])
            .unwrap()
            .rows[0]["id"],
        Value::Int(2)
    );
    restored.sql("CREATE OR REPLACE FUNCTION saved_bound(a integer) RETURNS integer LANGUAGE SQL IMMUTABLE RETURN a + 9; ALTER TABLE saved_table DISABLE RULE saved_rule; UPDATE saved_table SET id=3 WHERE id=2", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT saved_bound(3) AS result", &[])
            .unwrap()
            .rows[0]["result"],
        Value::Int(12)
    );
    assert_eq!(
        restored
            .sql("SELECT count(*) AS n FROM saved_audit", &[])
            .unwrap()
            .rows[0]["n"],
        Value::Int(1)
    );
    restored.sql("CREATE OR REPLACE VIEW saved_view AS SELECT id, 42::bigint AS position FROM saved_table", &[]).unwrap();
}
