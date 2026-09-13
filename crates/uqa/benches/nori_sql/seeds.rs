//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Pin ordinary SQL-created catalog inputs and capture replacements for review.

use std::path::Path;
use std::sync::OnceLock;

use serde_json::{json, Value as JSONValue};
use sha2::{Digest, Sha256};

use super::fixture::{Database, Provider};

pub const TABLE_SQL: &str = "CREATE TABLE docs (id BIGINT PRIMARY KEY, body TEXT)";
const SQLITE: &[u8] = include_bytes!("catalogs/sqlite-empty.db");
#[cfg(not(target_os = "emscripten"))]
const REDB: &[u8] = include_bytes!("catalogs/redb-empty.db");

enum Catalogs {
    Fresh,
    Captured(Provider, Vec<u8>),
}

static SELECTED: OnceLock<Catalogs> = OnceLock::new();

pub fn selected(provider: Provider) -> Option<&'static [u8]> {
    match SELECTED.get() {
        None => match provider {
            Provider::Memory => None,
            Provider::SQLite => Some(SQLITE),
            #[cfg(not(target_os = "emscripten"))]
            Provider::Redb => Some(REDB),
        },
        Some(Catalogs::Captured(selected, bytes)) if *selected == provider => Some(bytes),
        Some(Catalogs::Fresh | Catalogs::Captured(_, _)) => None,
    }
}

pub fn identities() -> JSONValue {
    let providers = [
        Provider::SQLite,
        #[cfg(not(target_os = "emscripten"))]
        Provider::Redb,
    ];
    let inputs: serde_json::Map<_, _> = providers
        .into_iter()
        .filter_map(|provider| {
            selected(provider).map(|bytes| {
                (
                    provider.name().into(),
                    json!({"bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))}),
                )
            })
        })
        .collect();
    json!(inputs)
}

pub fn command() -> Option<JSONValue> {
    let arguments: Vec<_> = std::env::args()
        .skip(1)
        .filter(|arg| arg != "--bench")
        .collect();
    match arguments.as_slice() {
        [] => None,
        [operation, directory] if operation == "--capture-empty-seeds" => {
            Some(capture(Path::new(directory)))
        }
        [operation, provider] if operation == "--transaction-probe" => Some(probe(provider, None)),
        [operation, provider, directory] if operation == "--transaction-probe" => {
            Some(probe(provider, Some(Path::new(directory))))
        }
        _ => panic!("unexpected SQL benchmark arguments"),
    }
}

fn probe(provider: &str, directory: Option<&Path>) -> JSONValue {
    let provider = match provider {
        "sqlite" => Provider::SQLite,
        #[cfg(not(target_os = "emscripten"))]
        "redb" => Provider::Redb,
        _ => panic!("a supported persistent provider is required"),
    };
    let catalogs = if let Some(directory) = directory {
        let bytes = std::fs::read(directory.join(format!("{}-empty.db", provider.name()))).unwrap();
        Catalogs::Captured(provider, bytes)
    } else {
        Catalogs::Fresh
    };
    assert!(SELECTED.set(catalogs).is_ok());
    let corpus: JSONValue = serde_json::from_str(super::CORPUS).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    let measurements: Vec<_> = [
        (0, 256, false),
        (256, 16, false),
        (2048, 16, false),
        (256, 16, true),
    ]
    .into_iter()
    .map(|(base, added, rollback)| super::transaction(provider, base, added, rollback, cases))
    .collect();
    let seed = selected(provider).map(
        |bytes| json!({"bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(bytes))}),
    );
    json!({"schema_version": 1, "owner": "uqa", "purpose": "transaction_fixture_probe",
           "provider": provider.name(), "empty_seed": seed, "measurements": measurements,
           "corpus_sha256": format!("{:x}", Sha256::digest(super::CORPUS.as_bytes())),
           "target_os": std::env::consts::OS, "target_arch": std::env::consts::ARCH, "pointer_bits": usize::BITS,
           "protocol": {"samples": super::SAMPLES, "warmup": 1, "allocation_samples": 1},
           "allocation_scope": "current-thread Rust allocator requests for BEGIN, bound INSERT batches and COMMIT/ROLLBACK with resulting Engine state retained; excludes fixture construction, validation, pre-existing resources, SQLite C allocations, stack and host heap",
           "gate": {"allocation_and_rows_passed": false, "timing_compared": false}})
}

fn capture(output: &Path) -> JSONValue {
    std::fs::create_dir_all(output.parent().unwrap()).unwrap();
    // A capture never replaces an earlier fixture or an unrelated directory.
    std::fs::create_dir(output).unwrap();
    let providers = [
        Provider::SQLite,
        #[cfg(not(target_os = "emscripten"))]
        Provider::Redb,
    ];
    let mut files = Vec::new();
    for provider in providers {
        let database = Database::empty(provider);
        database.engine.sql(TABLE_SQL, &[]).unwrap();
        let directory = database.close();
        let filename = format!("{}-empty.db", provider.name());
        let destination = output.join(&filename);
        std::fs::copy(directory.path().join("nori.db"), &destination).unwrap();
        // Verify a copy so constraint checks do not change the captured bytes.
        let verification = tempfile::tempdir().unwrap();
        let verification_path = verification.path().join("nori.db");
        std::fs::copy(&destination, &verification_path).unwrap();
        let engine = provider.open(&verification_path);
        let rows = engine.sql("SELECT id, body FROM docs", &[]).unwrap();
        assert!(rows.rows.is_empty());
        assert_eq!(rows.columns, ["id", "body"]);
        engine.sql("BEGIN", &[]).unwrap();
        engine
            .sql("INSERT INTO docs VALUES (1, 'seed check')", &[])
            .unwrap();
        let duplicate = engine
            .sql("INSERT INTO docs VALUES (1, 'duplicate')", &[])
            .unwrap_err();
        assert_eq!(duplicate.sqlstate(), Some("23505"));
        engine.sql("ROLLBACK", &[]).unwrap();
        let null = engine
            .sql("INSERT INTO docs VALUES (NULL, 'null key')", &[])
            .unwrap_err();
        assert_eq!(null.sqlstate(), Some("23502"));
        assert!(engine
            .sql("SELECT id, body FROM docs", &[])
            .unwrap()
            .rows
            .is_empty());
        drop(engine);
        let bytes = std::fs::read(&destination).unwrap();
        files.push(json!({"provider": provider.name(), "file": filename,
                          "bytes": bytes.len(), "sha256": format!("{:x}", Sha256::digest(&bytes))}));
    }
    json!({"schema_version": 1, "owner": "uqa", "purpose": "empty_sql_seed_capture",
           "table_sql": TABLE_SQL, "files": files})
}
