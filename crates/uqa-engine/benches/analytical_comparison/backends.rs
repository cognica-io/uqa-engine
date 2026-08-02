//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use uqa_core::Value;
use uqa_engine::Engine;

use super::fixture::Fixture;

type Q1Row = (String, String, i64, i64);
type ScanRow = (i64, i64);

pub struct Backends {
    uqa: Engine,
    sqlite: rusqlite::Connection,
    duckdb: duckdb::Connection,
}

impl Backends {
    pub fn new(fixture: &Fixture) -> Self {
        assert_ne!(fixture.manifest.seed, 0, "benchmark seed must be explicit");
        let insert = fixture.insert_sql();

        let uqa = Engine::new();
        uqa.sql(&fixture.manifest.schema_sql, &[])
            .expect("create UQA fixture");
        uqa.sql(
            &format!("SET work_mem TO '{}'", fixture.manifest.work_mem),
            &[],
        )
        .expect("set UQA work_mem");
        uqa.sql(&insert, &[]).expect("insert UQA fixture");

        let sqlite = rusqlite::Connection::open_in_memory().expect("open SQLite");
        sqlite
            .execute_batch(&fixture.manifest.schema_sql)
            .expect("create SQLite fixture");
        sqlite
            .execute_batch(&insert)
            .expect("insert SQLite fixture");

        let duckdb = duckdb::Connection::open_in_memory().expect("open DuckDB");
        duckdb
            .execute_batch(&fixture.manifest.schema_sql)
            .expect("create DuckDB fixture");
        duckdb
            .execute_batch(&insert)
            .expect("insert DuckDB fixture");

        Self {
            uqa,
            sqlite,
            duckdb,
        }
    }

    pub fn validate(&self, fixture: &Fixture) {
        let q1 = self.uqa_q1(fixture);
        assert_eq!(q1, self.sqlite_q1(fixture));
        assert_eq!(q1, self.duckdb_q1(fixture));
        let q6 = self.uqa_q6(fixture);
        assert_eq!(q6, self.sqlite_q6(fixture));
        assert_eq!(q6, self.duckdb_q6(fixture));
        let scan = self.uqa_scan(fixture);
        assert_eq!(scan, self.sqlite_scan(fixture));
        assert_eq!(scan, self.duckdb_scan(fixture));
        assert_eq!(scan.len(), fixture.expected_scan_rows());
        let (cursor_scan, cursor_summary) = self.uqa_cursor_scan_with_summary(fixture);
        assert_eq!(scan, cursor_scan);
        assert_eq!(cursor_summary.row_count, scan.len());
        assert!(
            !cursor_summary.spilled_to_disk,
            "the published cursor comparison must isolate in-memory cursor overhead"
        );
    }

    pub fn uqa_q1(&self, fixture: &Fixture) -> Vec<Q1Row> {
        self.uqa
            .sql(&fixture.manifest.queries.q1, &[])
            .expect("UQA Q1")
            .rows
            .into_iter()
            .map(|row| {
                (
                    string(&row, "return_flag"),
                    string(&row, "line_status"),
                    integer(&row, "count_order"),
                    integer(&row, "sum_qty"),
                )
            })
            .collect()
    }

    pub fn sqlite_q1(&self, fixture: &Fixture) -> Vec<Q1Row> {
        relational_q1(&self.sqlite, &fixture.manifest.queries.q1)
    }

    pub fn duckdb_q1(&self, fixture: &Fixture) -> Vec<Q1Row> {
        duckdb_q1(&self.duckdb, &fixture.manifest.queries.q1)
    }

    pub fn uqa_q6(&self, fixture: &Fixture) -> i64 {
        let result = self
            .uqa
            .sql(&fixture.manifest.queries.q6, &[])
            .expect("UQA Q6");
        integer(&result.rows[0], "revenue")
    }

    pub fn sqlite_q6(&self, fixture: &Fixture) -> i64 {
        self.sqlite
            .prepare_cached(&fixture.manifest.queries.q6)
            .expect("prepare SQLite Q6")
            .query_row([], |row| row.get(0))
            .expect("SQLite Q6")
    }

    pub fn duckdb_q6(&self, fixture: &Fixture) -> i64 {
        self.duckdb
            .prepare_cached(&fixture.manifest.queries.q6)
            .expect("prepare DuckDB Q6")
            .query_row([], |row| row.get(0))
            .expect("DuckDB Q6")
    }

    pub fn uqa_scan(&self, fixture: &Fixture) -> Vec<ScanRow> {
        self.uqa
            .sql(&fixture.manifest.queries.scan, &[])
            .expect("UQA scan")
            .rows
            .into_iter()
            .map(|row| (integer(&row, "id"), integer(&row, "extended_price")))
            .collect()
    }

    pub fn uqa_cursor_scan(&self, fixture: &Fixture) -> Vec<ScanRow> {
        let cursor = self
            .uqa
            .sql_cursor(&fixture.manifest.queries.scan, &[])
            .expect("UQA cursor scan");
        collect_cursor_rows(cursor)
    }

    fn uqa_cursor_scan_with_summary(
        &self,
        fixture: &Fixture,
    ) -> (Vec<ScanRow>, uqa_engine::SQLCursorSummary) {
        let cursor = self
            .uqa
            .sql_cursor(&fixture.manifest.queries.scan, &[])
            .expect("UQA cursor scan");
        let summary = cursor.summary();
        let rows = collect_cursor_rows(cursor);
        (rows, summary)
    }

    pub fn sqlite_scan(&self, fixture: &Fixture) -> Vec<ScanRow> {
        relational_scan(&self.sqlite, &fixture.manifest.queries.scan)
    }

    pub fn duckdb_scan(&self, fixture: &Fixture) -> Vec<ScanRow> {
        duckdb_scan(&self.duckdb, &fixture.manifest.queries.scan)
    }
}

fn collect_cursor_rows(cursor: uqa_engine::SQLCursor) -> Vec<ScanRow> {
    cursor
        .flat_map(|batch| {
            let batch = batch.expect("UQA cursor batch");
            scan_rows_from_batch(&batch)
        })
        .collect()
}

fn scan_rows_from_batch(batch: &uqa_execution::ColumnarBatch) -> Vec<ScanRow> {
    let id = batch
        .columns()
        .iter()
        .find(|column| column.name == "id")
        .expect("UQA cursor id column");
    let extended_price = batch
        .columns()
        .iter()
        .find(|column| column.name == "extended_price")
        .expect("UQA cursor extended_price column");
    id.values
        .iter()
        .zip(&extended_price.values)
        .map(|(id, extended_price)| {
            (
                integer_value(id, "id"),
                integer_value(extended_price, "extended_price"),
            )
        })
        .collect()
}

fn string(row: &uqa_sql::ResultRow, column: &str) -> String {
    match row.get(column) {
        Some(Value::Str(value)) => value.clone(),
        other => panic!("{column} must be text, got {other:?}"),
    }
}

fn integer(row: &uqa_sql::ResultRow, column: &str) -> i64 {
    match row.get(column) {
        Some(Value::Int(value)) => *value,
        other => panic!("{column} must be an integer, got {other:?}"),
    }
}

fn integer_value(value: &Value, column: &str) -> i64 {
    match value {
        Value::Int(value) => *value,
        other => panic!("{column} must be an integer, got {other:?}"),
    }
}

fn relational_q1(connection: &rusqlite::Connection, query: &str) -> Vec<Q1Row> {
    connection
        .prepare_cached(query)
        .expect("prepare SQLite Q1")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("execute SQLite Q1")
        .collect::<Result<_, _>>()
        .expect("read SQLite Q1")
}

fn duckdb_q1(connection: &duckdb::Connection, query: &str) -> Vec<Q1Row> {
    connection
        .prepare_cached(query)
        .expect("prepare DuckDB Q1")
        .query_map([], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .expect("execute DuckDB Q1")
        .collect::<Result<_, _>>()
        .expect("read DuckDB Q1")
}

fn relational_scan(connection: &rusqlite::Connection, query: &str) -> Vec<ScanRow> {
    connection
        .prepare_cached(query)
        .expect("prepare SQLite scan")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("execute SQLite scan")
        .collect::<Result<_, _>>()
        .expect("read SQLite scan")
}

fn duckdb_scan(connection: &duckdb::Connection, query: &str) -> Vec<ScanRow> {
    connection
        .prepare_cached(query)
        .expect("prepare DuckDB scan")
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .expect("execute DuckDB scan")
        .collect::<Result<_, _>>()
        .expect("read DuckDB scan")
}
