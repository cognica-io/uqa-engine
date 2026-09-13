//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Application-facing SQL, transaction and durable-session costs through the facade.

use std::hint::black_box;
use std::time::Instant;

use allocation_counter::{measure, opt_out, AllocationInfo};
use serde_json::{json, Value as JSONValue};
use sha2::{Digest, Sha256};
use uqa::{SQLParam, SQLResult};

#[path = "nori_sql/fixture.rs"]
mod fixture;
use fixture::{Database, Provider, DOCUMENTS, QUERY};

const CORPUS: &str = include_str!("../../uqa-analysis/benches/nori/corpus.json");
const SAMPLES: usize = 7;

fn allocation(info: AllocationInfo) -> JSONValue {
    json!({
        "count_total": info.count_total, "count_peak": info.count_max, "count_retained": info.count_current,
        "bytes_total": info.bytes_total, "bytes_peak": info.bytes_max, "bytes_retained": info.bytes_current,
    })
}

fn measured(name: &str, elapsed: &[u64], info: AllocationInfo) -> JSONValue {
    let mut sorted = elapsed.to_vec();
    sorted.sort_unstable();
    eprintln!("measured {name}");
    json!({"name": name, "elapsed_ns": elapsed, "median_ns": sorted[SAMPLES / 2],
           "verified_samples": SAMPLES + 2, "allocation": allocation(info)})
}

fn verify_snapshot(actual: &JSONValue, expected: &JSONValue) {
    for key in ["documents", "rows_sha256", "analyzer_fingerprint"] {
        assert_eq!(actual[key], expected[key], "SQL snapshot {key}");
    }
    let actual: Vec<Vec<(i64, f64)>> = serde_json::from_value(actual["queries"].clone()).unwrap();
    let expected: Vec<Vec<(i64, f64)>> =
        serde_json::from_value(expected["queries"].clone()).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (actual, expected) in actual.iter().zip(&expected) {
        fixture::same_scores(actual, expected);
    }
}

fn transaction(
    provider: Provider,
    base: usize,
    added: usize,
    rollback: bool,
    cases: &[JSONValue],
) -> JSONValue {
    let name = format!(
        "{}/{}_{added}/{base}",
        provider.name(),
        if rollback { "rollback" } else { "commit" }
    );
    let count = base + if rollback { 0 } else { added };
    let reference = Database::seed(Provider::Memory, "mixed", count, false, cases);
    let expected = fixture::snapshot(&reference.engine, count, cases);
    drop(reference);
    let seed = (provider != Provider::Memory)
        .then(|| Database::seed(provider, "mixed", base, false, cases).close());
    let setup = || {
        seed.as_ref().map_or_else(
            || Database::seed(provider, "mixed", base, false, cases),
            |directory| Database::copy_seed(provider, directory.path()),
        )
    };
    let statements = fixture::inserts(base, added, cases);
    let mutate = |database: &Database| {
        database.engine.sql("BEGIN", &[]).unwrap();
        let inserted = fixture::insert(&database.engine, &statements);
        database
            .engine
            .sql(if rollback { "ROLLBACK" } else { "COMMIT" }, &[])
            .unwrap();
        inserted
    };
    let verify = |database: Database| {
        let live = fixture::snapshot(&database.engine, count, cases);
        verify_snapshot(&live, &expected);
        let reopened = if provider == Provider::Memory {
            None
        } else {
            let reopened = database.reopen();
            let snapshot = fixture::snapshot(&reopened.engine, count, cases);
            verify_snapshot(&snapshot, &expected);
            Some(snapshot)
        };
        (live, reopened)
    };
    let mut elapsed = Vec::with_capacity(SAMPLES);
    opt_out(|| {
        for sample in 0..=SAMPLES {
            let database = setup();
            let start = Instant::now();
            let inserted = black_box(mutate(&database));
            let nanos = start.elapsed().as_nanos() as u64;
            assert_eq!(inserted, added);
            if sample > 0 {
                elapsed.push(nanos);
            }
            verify(database);
        }
    });
    let database = setup();
    let mut inserted = 0;
    let info = measure(|| inserted = mutate(&database));
    assert_eq!(inserted, added);
    let (live, reopened) = verify(database);
    let mut report = measured(&name, &elapsed, info);
    report["snapshot"] = live;
    report["reopened_snapshot"] = json!(reopened);
    report["reopened_samples"] = json!(if provider == Provider::Memory {
        0
    } else {
        SAMPLES + 2
    });
    report
}

fn query_probe(
    database: &Database,
    mode: &str,
    case_index: usize,
    cases: &[JSONValue],
    params: &[SQLParam],
    new_session: bool,
) -> JSONValue {
    let provider = database.provider;
    let case = &cases[case_index];
    let name = format!(
        "{}/{}/{mode}/{}",
        provider.name(),
        if new_session {
            "session_and_query"
        } else {
            "sql_query"
        },
        case["name"].as_str().unwrap()
    );
    let run = || {
        if new_session {
            let session = database.engine.new_session().unwrap();
            fixture::configure(&session);
            session.sql(QUERY, params).unwrap()
        } else {
            database.engine.sql(QUERY, params).unwrap()
        }
    };
    let first = run();
    let expected = fixture::scored_rows(&first, case_index, DOCUMENTS, true, cases.len());
    drop(first);
    let verify = |result: &SQLResult| {
        fixture::same_scores(
            &fixture::scored_rows(result, case_index, DOCUMENTS, true, cases.len()),
            &expected,
        );
    };
    let mut elapsed = Vec::with_capacity(SAMPLES);
    opt_out(|| {
        for _ in 0..SAMPLES {
            let start = Instant::now();
            let rows = black_box(run());
            elapsed.push(start.elapsed().as_nanos() as u64);
            verify(&rows);
        }
    });
    let mut retained = None;
    let info = measure(|| retained = Some(run()));
    verify(retained.as_ref().unwrap());
    drop(retained);
    let mut report = measured(&name, &elapsed, info);
    report["rows"] = json!(expected);
    report["analyzer_fingerprint"] = json!(fixture::fingerprint(&database.engine));
    report["query_sha256"] = json!(format!(
        "{:x}",
        Sha256::digest(case["text"].as_str().unwrap().as_bytes())
    ));
    report
}

fn queries(provider: Provider, cases: &[JSONValue], reports: &mut Vec<JSONValue>) {
    for mode in ["none", "discard", "mixed"] {
        let database = Database::seed(provider, mode, DOCUMENTS, true, cases);
        let database = if provider == Provider::Memory {
            database
        } else {
            database.reopen()
        };
        for (case_index, case) in cases.iter().enumerate() {
            let params = fixture::phrase(case);
            reports.push(query_probe(
                &database, mode, case_index, cases, &params, false,
            ));
            if provider != Provider::Memory {
                reports.push(query_probe(
                    &database, mode, case_index, cases, &params, true,
                ));
            }
        }
    }
}

fn provider_settings() -> JSONValue {
    let directory = tempfile::tempdir().unwrap();
    let connection =
        uqa_storage_sqlite::ManagedConnection::open(&directory.path().join("settings.db")).unwrap();
    connection.with(|connection| {
        let version: String = connection.query_row("SELECT sqlite_version()", [], |row| row.get(0))?;
        let journal: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
        let synchronous: i64 = connection.pragma_query_value(None, "synchronous", |row| row.get(0))?;
        let page_size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
        Ok(json!({"sqlite": {"version": version, "journal_mode": journal, "synchronous": synchronous, "page_size": page_size},
                  "redb": if cfg!(target_os = "emscripten") { None } else { Some("default immediate commit durability") }}))
    }).unwrap()
}

fn main() {
    let corpus: JSONValue = serde_json::from_str(CORPUS).unwrap();
    let cases = corpus["cases"].as_array().unwrap();
    let providers = [
        Provider::Memory,
        Provider::SQLite,
        #[cfg(not(target_os = "emscripten"))]
        Provider::Redb,
    ];
    let mut measurements = Vec::new();
    for provider in providers {
        for (base, added, rollback) in [
            (0, 256, false),
            (256, 16, false),
            (2048, 16, false),
            (256, 16, true),
        ] {
            measurements.push(transaction(provider, base, added, rollback, cases));
        }
        queries(provider, cases, &mut measurements);
    }
    println!(
        "{}",
        json!({
            "schema_version": 1, "owner": "uqa", "target_os": std::env::consts::OS, "target_arch": std::env::consts::ARCH,
            "pointer_bits": usize::BITS, "foreground_threads": 1, "work_mem_bytes": 256 * 1024 * 1024,
            "background_statistics": if cfg!(target_os = "emscripten") { "no worker threads" } else { "normal database-level provider worker; every seed is explicitly analyzed before measurement" },
            "protocol": {"samples": SAMPLES, "warmup": 1, "timed_operations_per_sample": 1, "insert_batch_rows": 64},
            "query_documents": DOCUMENTS + cases.len(), "corpus_sha256": format!("{:x}", Sha256::digest(CORPUS.as_bytes())),
            "providers": providers.iter().map(|provider| provider.name()).collect::<Vec<_>>(), "provider_settings": provider_settings(),
            "timing_scope": "public SQL transactions include BEGIN, bound INSERT batches and COMMIT/ROLLBACK; sql_query includes SQL binding, planning, complete quoted-phrase analysis, physical provider reads, calibrated scoring, implicit transaction completion and materialized rows; session_and_query also includes independent session creation, work_mem setup and session drop; excludes fixture setup, verification, initial open and final result drop",
            "allocation_scope": "current-thread Rust allocator requests during the same operations; queries retain the SQL result, transactions retain their Engine state; retained counts are net live-allocation deltas and may be negative when pre-existing state is freed; excludes pre-existing dictionary/fixture allocations, stack, SQLite C allocation and host heap",
            "measurements": measurements,
        })
    );
}
