//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared loader for the checked-in, deterministic TPC-H compatibility fixture.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uqa_core::Value;
use uqa_engine::Engine;

const INSERT_ROWS: usize = 128;

#[derive(Clone, Copy)]
enum FieldKind {
    Integer,
    Numeric,
    Date,
    Text,
}

struct TableSpec {
    name: &'static str,
    fields: &'static [FieldKind],
}

const REGION: &[FieldKind] = &[FieldKind::Integer, FieldKind::Text, FieldKind::Text];
const NATION: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Integer,
    FieldKind::Text,
];
const SUPPLIER: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Numeric,
    FieldKind::Text,
];
const CUSTOMER: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Numeric,
    FieldKind::Text,
    FieldKind::Text,
];
const PART: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Numeric,
    FieldKind::Text,
];
const PARTSUPP: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Integer,
    FieldKind::Integer,
    FieldKind::Numeric,
    FieldKind::Text,
];
const ORDERS: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Integer,
    FieldKind::Text,
    FieldKind::Numeric,
    FieldKind::Date,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Integer,
    FieldKind::Text,
];
const LINEITEM: &[FieldKind] = &[
    FieldKind::Integer,
    FieldKind::Integer,
    FieldKind::Integer,
    FieldKind::Integer,
    FieldKind::Numeric,
    FieldKind::Numeric,
    FieldKind::Numeric,
    FieldKind::Numeric,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Date,
    FieldKind::Date,
    FieldKind::Date,
    FieldKind::Text,
    FieldKind::Text,
    FieldKind::Text,
];

const TABLES: &[TableSpec] = &[
    TableSpec {
        name: "region",
        fields: REGION,
    },
    TableSpec {
        name: "nation",
        fields: NATION,
    },
    TableSpec {
        name: "supplier",
        fields: SUPPLIER,
    },
    TableSpec {
        name: "customer",
        fields: CUSTOMER,
    },
    TableSpec {
        name: "part",
        fields: PART,
    },
    TableSpec {
        name: "partsupp",
        fields: PARTSUPP,
    },
    TableSpec {
        name: "orders",
        fields: ORDERS,
    },
    TableSpec {
        name: "lineitem",
        fields: LINEITEM,
    },
];

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../benchmarks/tpch")
}

pub fn load_engine() -> Engine {
    let root = fixture_root();
    let engine = Engine::new();
    let schema = std::fs::read_to_string(root.join("schema.sql")).expect("read TPC-H schema");
    engine.sql(&schema, &[]).expect("create TPC-H schema");
    for table in TABLES {
        load_table(&engine, &root, table);
    }
    engine
}

pub fn load_queries() -> Vec<String> {
    let root = fixture_root().join("queries");
    (1..=22)
        .map(|number| {
            std::fs::read_to_string(root.join(format!("q{number:02}.sql")))
                .unwrap_or_else(|error| panic!("read TPC-H Q{number}: {error}"))
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct ExpectedFixture {
    schema_version: u32,
    queries: Vec<ExpectedQuery>,
}

#[derive(Debug, Deserialize)]
struct ExpectedQuery {
    query: usize,
    result: CanonicalResult,
}

pub fn load_expected_results() -> Vec<CanonicalResult> {
    let path = fixture_root().join("expected/pg18.json");
    let fixture: ExpectedFixture = serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
    )
    .unwrap_or_else(|error| panic!("decode {}: {error}", path.display()));
    assert_eq!(fixture.schema_version, 1, "TPC-H expected schema version");
    assert_eq!(fixture.queries.len(), 22, "TPC-H expected query count");
    fixture
        .queries
        .into_iter()
        .enumerate()
        .map(|(index, query)| {
            assert_eq!(query.query, index + 1, "TPC-H expected query sequence");
            query.result
        })
        .collect()
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct CanonicalResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

pub fn canonical_result(result: &uqa_engine::SQLResult) -> CanonicalResult {
    CanonicalResult {
        columns: result.columns.clone(),
        rows: result
            .rows
            .iter()
            .map(|row| {
                result
                    .columns
                    .iter()
                    .map(|column| canonical_value(row.get(column).unwrap_or(&Value::Null)))
                    .collect()
            })
            .collect(),
    }
}

fn canonical_value(value: &Value) -> String {
    match value {
        Value::Null => "<NULL>".into(),
        Value::Void => String::new(),
        Value::Bool(value) => {
            if *value {
                "t".into()
            } else {
                "f".into()
            }
        }
        Value::Int(value) => value.to_string(),
        Value::Float(value) => value.to_string(),
        Value::Decimal(value) => value.to_canonical_string(),
        Value::Str(value) | Value::FixedChar(value) | Value::Json(value) | Value::JsonB(value) => {
            value.clone()
        }
        Value::Temporal(value) => value.to_sql_string(),
        Value::Bytes(value) => {
            let mut encoded = String::with_capacity(value.len() * 2);
            for byte in value {
                write!(encoded, "{byte:02x}").expect("write canonical byte value");
            }
            encoded
        }
        Value::Array(_) | Value::List(_) | Value::Row(_) | Value::Record(_) | Value::Map(_) => {
            serde_json::to_string(value).expect("serialize canonical TPC-H value")
        }
    }
}

fn load_table(engine: &Engine, root: &Path, spec: &TableSpec) {
    let path = root.join("data").join(format!("{}.tbl", spec.name));
    let input = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let mut rows = Vec::with_capacity(INSERT_ROWS);
    for (line_number, line) in input.lines().enumerate() {
        let mut values: Vec<&str> = line.split('|').collect();
        assert_eq!(
            values.pop(),
            Some(""),
            "{} line {} must end in |",
            spec.name,
            line_number + 1
        );
        assert_eq!(
            values.len(),
            spec.fields.len(),
            "{} line {} field count",
            spec.name,
            line_number + 1
        );
        rows.push(render_row(&values, spec.fields));
        if rows.len() == INSERT_ROWS {
            insert_rows(engine, spec.name, &rows);
            rows.clear();
        }
    }
    if !rows.is_empty() {
        insert_rows(engine, spec.name, &rows);
    }
}

fn render_row(values: &[&str], kinds: &[FieldKind]) -> String {
    let mut row = String::from("(");
    for (index, (value, kind)) in values.iter().zip(kinds).enumerate() {
        if index != 0 {
            row.push_str(", ");
        }
        match kind {
            FieldKind::Integer | FieldKind::Numeric => row.push_str(value),
            FieldKind::Date | FieldKind::Text => {
                row.push('\'');
                row.push_str(&value.replace('\'', "''"));
                row.push('\'');
            }
        }
    }
    row.push(')');
    row
}

fn insert_rows(engine: &Engine, table: &str, rows: &[String]) {
    let values_len = rows.iter().map(String::len).sum::<usize>() + rows.len() * 2;
    let mut sql = String::with_capacity(table.len() + values_len + 20);
    write!(sql, "INSERT INTO {table} VALUES ").expect("write INSERT prefix");
    sql.push_str(&rows.join(", "));
    engine
        .sql(&sql, &[])
        .unwrap_or_else(|error| panic!("load TPC-H table {table}: {error}"));
}
