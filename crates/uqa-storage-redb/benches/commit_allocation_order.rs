//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Isolate redb table-definition copy allocations from index algorithms.

use std::collections::BTreeMap;

use redb::{Database, ReadableDatabase, TableDefinition, Value, WriteTransaction};
use serde_json::json;

const SAMPLES: usize = 64;
// Match the provider's physical table definitions; the control changes only types.
const DATA: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_key_value");
const METADATA: TableDefinition<&str, u64> = TableDefinition::new("uqa_storage_metadata");
const CONTROL: TableDefinition<&[u8], &[u8]> = TableDefinition::new("uqa_storage_metadata");

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Allocation {
    count_total: u64,
    count_peak: u64,
    count_net: i64,
    bytes_total: u64,
    bytes_peak: u64,
    bytes_net: i64,
}

fn write_tables(transaction: &WriteTransaction, mixed_types: bool, value: &[u8], version: u64) {
    transaction
        .open_table(DATA)
        .unwrap()
        .insert(b"key".as_slice(), value)
        .unwrap();
    if mixed_types {
        transaction
            .open_table(METADATA)
            .unwrap()
            .insert("change_version", version)
            .unwrap();
    } else {
        transaction
            .open_table(CONTROL)
            .unwrap()
            .insert(b"key".as_slice(), value)
            .unwrap();
    }
}

fn sample(mixed_types: bool, existing_tables: bool) -> Allocation {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("db");
    let database = Database::create(&path).unwrap();
    if existing_tables {
        let transaction = database.begin_write().unwrap();
        write_tables(&transaction, mixed_types, b"value", 1);
        transaction.commit().unwrap();
    }
    let transaction = database.begin_write().unwrap();
    write_tables(&transaction, mixed_types, b"next value", 2);
    let info = allocation_counter::measure(|| transaction.commit().unwrap());

    drop(database);
    let database = Database::open(&path).unwrap();
    let transaction = database.begin_read().unwrap();
    let table = transaction.open_table(DATA).unwrap();
    assert_eq!(
        table.get(b"key".as_slice()).unwrap().unwrap().value(),
        b"next value"
    );
    if mixed_types {
        let table = transaction.open_table(METADATA).unwrap();
        assert_eq!(table.get("change_version").unwrap().unwrap().value(), 2);
    } else {
        let table = transaction.open_table(CONTROL).unwrap();
        assert_eq!(
            table.get(b"key".as_slice()).unwrap().unwrap().value(),
            b"next value"
        );
    }
    Allocation {
        count_total: info.count_total,
        count_peak: info.count_max,
        count_net: info.count_current,
        bytes_total: info.bytes_total,
        bytes_peak: info.bytes_max,
        bytes_net: info.bytes_current,
    }
}

fn observe(mixed_types: bool, existing_tables: bool, type_name_delta: u64) -> serde_json::Value {
    let mut histogram = BTreeMap::<Allocation, usize>::new();
    for _ in 0..SAMPLES {
        *histogram
            .entry(sample(mixed_types, existing_tables))
            .or_default() += 1;
    }
    if mixed_types && existing_tables {
        assert_eq!(
            histogram.len(),
            2,
            "both table-update orders must be observed"
        );
        let mut values = histogram.keys().copied();
        let lower = values.next().unwrap();
        let upper = values.next().unwrap();
        assert_eq!(upper.bytes_total - lower.bytes_total, type_name_delta);
        assert_eq!(
            Allocation {
                bytes_total: 0,
                ..lower
            },
            Allocation {
                bytes_total: 0,
                ..upper
            }
        );
    } else {
        assert_eq!(histogram.len(), 1, "control allocations must be stable");
    }
    let outcomes: Vec<_> = histogram
        .into_iter()
        .map(|(allocation, samples)| {
            json!({
                "samples": samples,
                "allocation": {
                    "count_total": allocation.count_total,
                    "count_peak": allocation.count_peak,
                    "count_net": allocation.count_net,
                    "bytes_total": allocation.bytes_total,
                    "bytes_peak": allocation.bytes_peak,
                    "bytes_net": allocation.bytes_net,
                }
            })
        })
        .collect();
    json!({"mixed_types": mixed_types, "existing_tables": existing_tables,
        "verified_reopened_samples": SAMPLES, "outcomes": outcomes})
}

fn main() {
    let bytes_name = <&[u8]>::type_name();
    let string_name = <&str>::type_name();
    let integer_name = u64::type_name();
    let type_name_delta = u64::try_from(
        2 * bytes_name.name().len() - string_name.name().len() - integer_name.name().len(),
    )
    .unwrap();
    assert_eq!(type_name_delta, 3);
    let mut measurements = Vec::new();
    for existing_tables in [false, true] {
        for mixed_types in [false, true] {
            measurements.push(observe(mixed_types, existing_tables, type_name_delta));
        }
    }
    println!("{}", serde_json::to_string_pretty(&json!({
        "schema_version": 1,
        "owner": "uqa-storage-redb",
        "target_os": std::env::consts::OS,
        "target_arch": std::env::consts::ARCH,
        "pointer_bits": usize::BITS,
        "allocation_scope": "commit only; table writes, seed creation and reopened reads excluded",
        "samples_per_condition": SAMPLES,
        "type_names": {"data": [bytes_name.name(), bytes_name.name()],
            "metadata": [string_name.name(), integer_name.name()]},
        "type_name_bytes_delta": type_name_delta,
        "measurements": measurements,
    })).unwrap());
}
