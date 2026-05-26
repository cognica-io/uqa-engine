//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` and Apache AGE compatibility matrices.
//!
//! These tests keep broad SQL, `PostgreSQL` 17 delta, and AGE query
//! shapes from the manual compatibility suite under CI-grade Rust
//! coverage.

use uqa_core::Value;
use uqa_engine::Engine;
use uqa_sql::SQLResult;

fn exec(engine: &Engine, sql: &str) {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|err| panic!("SQL failed:\n{sql}\n{err:?}"));
}

fn query(engine: &Engine, sql: &str) -> SQLResult {
    engine
        .sql(sql, &[])
        .unwrap_or_else(|err| panic!("SQL failed:\n{sql}\n{err:?}"))
}

fn assert_check(engine: &Engine, name: &str, sql: &str) {
    let result = query(engine, sql);
    assert_eq!(result.rows.len(), 1, "{name}: {result:?}");
    let row = &result.rows[0];
    assert_eq!(row.get("check"), Some(&Value::Str(name.into())), "{name}");
    assert_eq!(row.get("ok"), Some(&Value::Bool(true)), "{name}: {row:?}");
}

fn assert_sql_error_contains(engine: &Engine, sql: &str, needle: &str) {
    let err = match engine.sql(sql, &[]) {
        Ok(result) => panic!("SQL unexpectedly succeeded:\n{sql}\n{result:?}"),
        Err(err) => format!("{err:?}"),
    };
    assert!(
        err.to_lowercase().contains(&needle.to_lowercase()),
        "expected `{needle}` in error for:\n{sql}\n{err}"
    );
}

#[test]
fn postgresql_sql_compatibility_matrix() {
    let eng = Engine::new();
    postgresql_schema_constraints_and_account_dml(&eng);
    postgresql_account_followup_dml(&eng);
    postgresql_update_delete_dml(&eng);
    postgresql_merge_returning_and_join_shapes(&eng);
    postgresql_recursive_lateral_subquery_and_predicates(&eng);
    postgresql_aggregate_set_and_table_functions(&eng);
    postgresql_prepared_sequence_temporal_and_nulls(&eng);
    postgresql_scalar_tx_catalog_and_session_state(&eng);
    postgresql_foreign_key_and_ddl_lifecycle(&eng);
}

fn postgresql_schema_constraints_and_account_dml(eng: &Engine) {
    exec(eng, "CREATE SCHEMA compat_sql");
    exec(eng, "SET search_path TO compat_sql, public");
    assert_check(
        eng,
        "schema_search_path",
        "SELECT 'schema_search_path' AS check, EXISTS (
             SELECT 1 FROM pg_catalog.pg_settings
             WHERE name = 'search_path' AND setting LIKE 'compat_sql%'
         ) AS ok",
    );

    exec(
        eng,
        "CREATE TABLE accounts (
             id SERIAL PRIMARY KEY,
             owner TEXT NOT NULL UNIQUE,
             balance INTEGER DEFAULT 0 CHECK (balance >= 0),
             active BOOLEAN DEFAULT TRUE,
             opened DATE,
             tags TEXT[],
             profile JSONB
         )",
    );
    exec(
        eng,
        "INSERT INTO accounts (owner, balance, opened, tags, profile) VALUES
             ('alice', 100, '2026-05-20', ARRAY['search', 'sql'],
              '{\"tier\": \"pro\", \"flags\": [\"beta\", \"pg\"]}'::jsonb),
             ('bob', 40, '2026-05-21', ARRAY['billing'],
              '{\"tier\": \"free\", \"flags\": [\"trial\"]}'::jsonb),
             ('carol', 210, '2026-05-22', ARRAY['search', 'graph'],
              '{\"tier\": \"enterprise\", \"flags\": [\"pg\", \"graph\"]}'::jsonb)",
    );
    assert_check(
        eng,
        "serial_defaults_json_array",
        "SELECT 'serial_defaults_json_array' AS check,
             COUNT(*) = 3
             AND MIN(id) = 1
             AND MAX(id) = 3
             AND COUNT(*) FILTER (WHERE active) = 3
             AND EXISTS (
                 SELECT 1 FROM accounts
                 WHERE owner = 'alice'
                   AND profile->>'tier' = 'pro'
                   AND profile ? 'flags'
                   AND profile @> '{\"tier\": \"pro\"}'::jsonb
                   AND jsonb_array_length(profile->'flags') = 2
                   AND array_length(tags, 1) = 2
             ) AS ok
         FROM accounts",
    );
    assert_check(
        eng,
        "jsonb_operator_matrix",
        "SELECT 'jsonb_operator_matrix' AS check,
             EXISTS (
                 SELECT 1 FROM accounts
                 WHERE owner = 'alice'
                   AND profile ?| ARRAY['missing', 'flags']
                   AND profile ?& ARRAY['tier', 'flags']
                   AND '{\"tier\": \"pro\"}'::jsonb <@ profile
                   AND profile @> '{\"flags\": [\"pg\"]}'::jsonb
             ) AS ok",
    );
    assert_sql_error_contains(
        eng,
        "INSERT INTO accounts (owner, balance) VALUES ('alice', 1)",
        "unique",
    );
    assert_sql_error_contains(
        eng,
        "INSERT INTO accounts (owner, balance) VALUES ('erin', -1)",
        "check",
    );
    assert_sql_error_contains(
        eng,
        "INSERT INTO accounts (balance) VALUES (50)",
        "not null",
    );
    assert_check(
        eng,
        "constraint_rejections_do_not_mutate",
        "SELECT 'constraint_rejections_do_not_mutate' AS check, COUNT(*) = 3 AS ok
         FROM accounts",
    );
}

fn postgresql_account_followup_dml(eng: &Engine) {
    exec(
        eng,
        "ALTER TABLE accounts ADD COLUMN plan TEXT NOT NULL DEFAULT 'free'",
    );
    exec(
        eng,
        "INSERT INTO accounts (owner, opened, tags, profile) VALUES
             ('dave', '2026-05-25', ARRAY['trial'],
              '{\"tier\": \"free\", \"flags\": [\"new\"]}'::jsonb)",
    );
    assert_check(
        eng,
        "alter_default_backfill",
        "SELECT 'alter_default_backfill' AS check,
             COUNT(*) = 4 AND COUNT(*) FILTER (WHERE plan = 'free') = 4 AS ok
         FROM accounts",
    );

    let returning = query(
        eng,
        "UPDATE accounts SET balance = balance + 15
         WHERE owner = 'alice'
         RETURNING id, balance",
    );
    assert_eq!(returning.rows[0].get("balance"), Some(&Value::Int(115)));
    assert_check(
        eng,
        "update_returning_followup",
        "SELECT 'update_returning_followup' AS check,
             EXISTS (SELECT 1 FROM accounts WHERE owner = 'alice' AND balance = 115) AS ok",
    );

    exec(
        eng,
        "INSERT INTO accounts (id, owner, balance) VALUES (1, 'alice-rewrite', 125)
         ON CONFLICT (id) DO UPDATE SET balance = EXCLUDED.balance",
    );
    assert_check(
        eng,
        "upsert_excluded",
        "SELECT 'upsert_excluded' AS check,
             EXISTS (
                 SELECT 1 FROM accounts
                 WHERE id = 1 AND owner = 'alice' AND balance = 125
             ) AS ok",
    );
}

fn postgresql_update_delete_dml(eng: &Engine) {
    exec(
        eng,
        "CREATE TABLE adjustments (account_id INTEGER PRIMARY KEY, amount INTEGER)",
    );
    exec(
        eng,
        "INSERT INTO adjustments (account_id, amount) VALUES (1, 5), (2, -10)",
    );
    exec(
        eng,
        "UPDATE accounts
         SET balance = accounts.balance + adjustments.amount
         FROM adjustments
         WHERE accounts.id = adjustments.account_id",
    );
    assert_check(
        eng,
        "update_from",
        "SELECT 'update_from' AS check,
             EXISTS (SELECT 1 FROM accounts WHERE id = 1 AND balance = 130)
             AND EXISTS (SELECT 1 FROM accounts WHERE id = 2 AND balance = 30) AS ok",
    );

    exec(
        eng,
        "CREATE TABLE to_delete (id INTEGER PRIMARY KEY, marker TEXT)",
    );
    exec(eng, "CREATE TABLE delete_keys (id INTEGER)");
    exec(
        eng,
        "INSERT INTO to_delete (id, marker) VALUES (1, 'keep'), (2, 'drop'), (3, 'drop')",
    );
    exec(eng, "INSERT INTO delete_keys (id) VALUES (2), (3)");
    exec(
        eng,
        "DELETE FROM to_delete USING delete_keys WHERE to_delete.id = delete_keys.id",
    );
    assert_check(
        eng,
        "delete_using",
        "SELECT 'delete_using' AS check, COUNT(*) = 1 AND MIN(id) = 1 AS ok FROM to_delete",
    );
}

fn postgresql_merge_returning_and_join_shapes(eng: &Engine) {
    exec(
        eng,
        "CREATE TABLE inventory (id INTEGER PRIMARY KEY, qty INTEGER)",
    );
    exec(
        eng,
        "INSERT INTO inventory (id, qty) VALUES (1, 10), (2, 20)",
    );
    exec(
        eng,
        "CREATE TABLE deltas (id INTEGER PRIMARY KEY, change INTEGER)",
    );
    exec(eng, "INSERT INTO deltas (id, change) VALUES (1, 5), (3, 7)");
    let merged = query(
        eng,
        "MERGE INTO inventory AS t
         USING deltas AS d
         ON t.id = d.id
         WHEN MATCHED THEN UPDATE SET qty = t.qty + d.change
         WHEN NOT MATCHED THEN INSERT (id, qty) VALUES (d.id, d.change)
         RETURNING merge_action() AS action, t.id AS id, t.qty AS qty, d.change AS delta",
    );
    assert_eq!(merged.affected_rows, 2);
    assert_eq!(merged.columns, vec!["action", "id", "qty", "delta"]);
    assert!(
        merged.rows.iter().any(
            |row| row.get("action") == Some(&Value::Str("UPDATE".into()))
                && row.get("id") == Some(&Value::Int(1))
                && row.get("qty") == Some(&Value::Int(15))
                && row.get("delta") == Some(&Value::Int(5))
        ),
        "{merged:?}"
    );
    assert!(
        merged.rows.iter().any(
            |row| row.get("action") == Some(&Value::Str("INSERT".into()))
                && row.get("id") == Some(&Value::Int(3))
                && row.get("qty") == Some(&Value::Int(7))
                && row.get("delta") == Some(&Value::Int(7))
        ),
        "{merged:?}"
    );
    assert_check(
        eng,
        "merge_update_insert_returning",
        "SELECT 'merge_update_insert_returning' AS check,
             EXISTS (SELECT 1 FROM inventory WHERE id = 1 AND qty = 15)
             AND EXISTS (SELECT 1 FROM inventory WHERE id = 2 AND qty = 20)
             AND EXISTS (SELECT 1 FROM inventory WHERE id = 3 AND qty = 7) AS ok",
    );
    let err = eng
        .sql("SELECT merge_action()", &[])
        .unwrap_err()
        .to_string();
    assert!(err.contains("MERGE RETURNING"), "{err}");

    exec(eng, "CREATE TABLE removals (id INTEGER PRIMARY KEY)");
    exec(eng, "INSERT INTO removals (id) VALUES (2)");
    let deleted = query(
        eng,
        "MERGE INTO inventory AS t
         USING removals AS r
         ON t.id = r.id
         WHEN MATCHED THEN DELETE
         RETURNING merge_action() AS action, t.id AS id, r.id AS removed_id",
    );
    assert_eq!(deleted.affected_rows, 1);
    assert_eq!(deleted.columns, vec!["action", "id", "removed_id"]);
    assert_eq!(
        deleted.rows[0].get("action"),
        Some(&Value::Str("DELETE".into()))
    );
    assert_eq!(deleted.rows[0].get("id"), Some(&Value::Int(2)));
    assert_eq!(deleted.rows[0].get("removed_id"), Some(&Value::Int(2)));

    exec(
        eng,
        "CREATE TABLE people (id INTEGER PRIMARY KEY, name TEXT)",
    );
    exec(
        eng,
        "INSERT INTO people (id, name) VALUES (1, 'Alice'), (2, 'Bob'), (3, 'Carol')",
    );
    exec(
        eng,
        "CREATE TABLE orders (oid INTEGER PRIMARY KEY, person_id INTEGER, product TEXT)",
    );
    exec(
        eng,
        "INSERT INTO orders (oid, person_id, product) VALUES
             (10, 1, 'Book'), (11, 1, 'Pen'), (12, 2, 'Notebook'), (13, 99, 'Ghost')",
    );
    assert_check(
        eng,
        "join_shapes",
        "SELECT 'join_shapes' AS check,
             (SELECT COUNT(*) FROM people INNER JOIN orders ON people.id = orders.person_id) = 3
             AND (SELECT COUNT(*) FROM people LEFT JOIN orders ON people.id = orders.person_id) = 4
             AND (SELECT COUNT(*) FROM people RIGHT JOIN orders ON people.id = orders.person_id) = 4
             AND (SELECT COUNT(*) FROM people FULL OUTER JOIN orders ON people.id = orders.person_id) = 5 AS ok",
    );
}

fn postgresql_recursive_lateral_subquery_and_predicates(eng: &Engine) {
    assert_check(
        eng,
        "recursive_cte",
        "WITH RECURSIVE cnt(x) AS (
             SELECT 1
             UNION ALL
             SELECT x + 1 FROM cnt WHERE x < 5
         )
         SELECT 'recursive_cte' AS check, COUNT(*) = 5 AND SUM(x) = 15 AS ok FROM cnt",
    );

    exec(
        eng,
        "CREATE TABLE depts (id INTEGER PRIMARY KEY, dept_name TEXT)",
    );
    exec(
        eng,
        "CREATE TABLE emps (
             id INTEGER PRIMARY KEY,
             emp_name TEXT,
             dept_id INTEGER,
             salary INTEGER
         )",
    );
    exec(
        eng,
        "INSERT INTO depts VALUES (1, 'Engineering'), (2, 'Sales')",
    );
    exec(
        eng,
        "INSERT INTO emps VALUES
             (1, 'Alice', 1, 90000), (2, 'Bob', 1, 80000),
             (3, 'Charlie', 2, 70000), (4, 'Diana', 2, 75000)",
    );
    assert_check(
        eng,
        "lateral_subquery",
        "SELECT 'lateral_subquery' AS check, COUNT(*) = 2 AND SUM(top_salary) = 165000 AS ok
         FROM (
             SELECT d.dept_name, sub.top_salary
             FROM depts d,
             LATERAL (
                 SELECT MAX(salary) AS top_salary
                 FROM emps
                 WHERE emps.dept_id = d.id
             ) sub
         ) q",
    );
    assert_check(
        eng,
        "subquery_predicate_matrix",
        "SELECT 'subquery_predicate_matrix' AS check,
             (SELECT COUNT(*) FROM accounts
              WHERE id IN (SELECT account_id FROM adjustments)) = 2
             AND EXISTS (
                 SELECT 1 FROM accounts a
                 WHERE a.owner LIKE 'a%'
                   AND a.balance = (SELECT MAX(balance) FROM accounts WHERE owner LIKE 'a%')
             )
             AND NOT EXISTS (
                 SELECT 1 FROM accounts
                 WHERE owner ILIKE 'z%'
             )
             AND (SELECT CASE WHEN COUNT(*) FILTER (WHERE balance BETWEEN 30 AND 130) = 2
                       THEN 'ok' ELSE 'bad' END
                  FROM accounts) = 'ok'
             AND (SELECT NULLIF(owner, 'alice') FROM accounts WHERE id = 1) IS NULL AS ok",
    );
}

fn postgresql_aggregate_set_and_table_functions(eng: &Engine) {
    exec(
        eng,
        "CREATE TABLE products (
             id INTEGER PRIMARY KEY,
             category TEXT,
             name TEXT,
             price INTEGER,
             active BOOLEAN
         )",
    );
    exec(
        eng,
        "INSERT INTO products (id, category, name, price, active) VALUES
             (1, 'fruit', 'Apple', 3, true),
             (2, 'fruit', 'Banana', 2, true),
             (3, 'fruit', 'Cherry', 5, false),
             (4, 'veggie', 'Daikon', 4, true),
             (5, 'veggie', 'Eggplant', 6, false)",
    );
    assert_check(
        eng,
        "aggregate_filter_window",
        "SELECT 'aggregate_filter_window' AS check,
             (SELECT COUNT(DISTINCT category) FROM products) = 2
             AND (SELECT SUM(price) FILTER (WHERE active) FROM products) = 9
             AND (SELECT BOOL_OR(NOT active) FROM products) = true
             AND (SELECT MAX(rn) FROM (
                 SELECT row_number() OVER (PARTITION BY category ORDER BY price DESC) AS rn
                 FROM products
             ) ranked) = 3 AS ok",
    );
    assert_check(
        eng,
        "rollup_grouping_set",
        "SELECT 'rollup_grouping_set' AS check, COUNT(*) = 3 AND MAX(total) = 20 AS ok
         FROM (
             SELECT category, SUM(price) AS total
             FROM products
             GROUP BY ROLLUP(category)
         ) r",
    );
    assert_check(
        eng,
        "set_operations",
        "SELECT 'set_operations' AS check,
             (SELECT COUNT(*) FROM (
                 SELECT category FROM products UNION SELECT category FROM products
             ) u) = 2
             AND (SELECT COUNT(*) FROM (
                 SELECT category FROM products WHERE category = 'fruit'
                 INTERSECT
                 SELECT category FROM products WHERE name = 'Apple'
             ) i) = 1
             AND (SELECT COUNT(*) FROM (
                 SELECT category FROM products
                 EXCEPT
                 SELECT category FROM products WHERE category = 'fruit'
             ) e) = 1 AS ok",
    );
    assert_check(
        eng,
        "table_functions",
        "SELECT 'table_functions' AS check,
             (SELECT SUM(n) FROM generate_series(1, 5) AS t(n)) = 15
             AND (SELECT COUNT(*) FROM unnest(ARRAY[10, 20, 30]) AS u(v)) = 3
             AND (SELECT COUNT(*) FROM json_each('{\"a\": 1, \"b\": 2}')) = 2 AS ok",
    );
}

fn postgresql_prepared_sequence_temporal_and_nulls(eng: &Engine) {
    exec(
        eng,
        "PREPARE set_balance(integer, integer) AS
         UPDATE accounts SET balance = $1 WHERE id = $2",
    );
    exec(eng, "EXECUTE set_balance(31, 2)");
    assert_check(
        eng,
        "prepared_execute",
        "SELECT 'prepared_execute' AS check,
             EXISTS (SELECT 1 FROM accounts WHERE id = 2 AND balance = 31) AS ok",
    );
    exec(eng, "DEALLOCATE set_balance");

    exec(eng, "CREATE SEQUENCE compat_seq START 10 INCREMENT 5");
    let seq_start = query(eng, "SELECT nextval('compat_seq') AS seq_start");
    assert_eq!(seq_start.rows[0].get("seq_start"), Some(&Value::Int(10)));
    assert_check(
        eng,
        "sequence_currval",
        "SELECT 'sequence_currval' AS check, currval('compat_seq') = 10 AS ok",
    );
    exec(eng, "ALTER SEQUENCE compat_seq RESTART WITH 50");
    assert_check(
        eng,
        "sequence_restart",
        "SELECT 'sequence_restart' AS check, nextval('compat_seq') = 50 AS ok",
    );

    exec(
        eng,
        "CREATE TABLE events (
             id INTEGER PRIMARY KEY,
             event_date DATE,
             created_at TIMESTAMP WITHOUT TIME ZONE,
             observed_at TIMESTAMP WITH TIME ZONE
         )",
    );
    exec(
        eng,
        "INSERT INTO events (id, event_date, created_at, observed_at) VALUES
             (1, '2026-05-24', '2026-05-24 09:30:00', '2026-05-24T00:30:00Z'),
             (2, '2026-05-23', '2026-05-23 10:00:00', '2026-05-23T01:00:00Z')",
    );
    assert_check(
        eng,
        "temporal_extract_order",
        "SELECT 'temporal_extract_order' AS check,
             (SELECT COUNT(*) FROM events WHERE created_at >= '2026-05-24 00:00:00') = 1
             AND (SELECT EXTRACT(year FROM created_at) FROM events WHERE id = 1) = 2026
             AND (SELECT id FROM events ORDER BY observed_at LIMIT 1) = 2 AS ok",
    );

    exec(
        eng,
        "CREATE TABLE null_order (id INTEGER PRIMARY KEY, n INTEGER)",
    );
    exec(
        eng,
        "INSERT INTO null_order (id, n) VALUES (1, 10), (2, NULL), (3, 5), (4, NULL), (5, 20)",
    );
    assert_check(
        eng,
        "nulls_order",
        "SELECT 'nulls_order' AS check,
             (SELECT id FROM null_order ORDER BY n ASC LIMIT 1) = 3
             AND (SELECT id FROM null_order ORDER BY n DESC LIMIT 1) IN (2, 4)
             AND (SELECT id FROM null_order ORDER BY n DESC NULLS LAST LIMIT 1) = 5 AS ok",
    );
}

fn postgresql_scalar_tx_catalog_and_session_state(eng: &Engine) {
    exec(
        eng,
        "CREATE TABLE texts (id INTEGER PRIMARY KEY, body TEXT, opt TEXT, score INTEGER)",
    );
    exec(
        eng,
        "INSERT INTO texts (id, body, opt, score) VALUES
             (1, 'Hello World', NULL, 7),
             (2, 'rust IS great', 'tag', 3)",
    );
    assert_check(
        eng,
        "scalar_functions",
        "SELECT 'scalar_functions' AS check,
             EXISTS (
                 SELECT 1 FROM texts
                 WHERE id = 1
                   AND UPPER(body) = 'HELLO WORLD'
                   AND SUBSTRING(body, 1, 5) = 'Hello'
                   AND CONCAT(body, '!') = 'Hello World!'
                   AND COALESCE(opt, 'fallback') = 'fallback'
             )
             AND (SELECT GREATEST(1, 5, 3)) = 5
             AND (SELECT regexp_replace('a-b-c-d', '-', '_', 'g')) = 'a_b_c_d' AS ok",
    );
    assert_check(
        eng,
        "array_type_ops",
        "SELECT 'array_type_ops' AS check,
             array_length(ARRAY[1, 2, 3], 1) = 3
             AND cardinality(array_cat(ARRAY[1, 2], ARRAY[3, 4])) = 4
             AND array_remove(ARRAY[1, 2, 3, 2], 2) = ARRAY[1, 3] AS ok",
    );

    exec(
        eng,
        "CREATE TABLE tx_log (id INTEGER PRIMARY KEY, body TEXT)",
    );
    exec(eng, "BEGIN");
    exec(eng, "INSERT INTO tx_log (id, body) VALUES (1, 'inside')");
    exec(eng, "SAVEPOINT sp");
    exec(eng, "INSERT INTO tx_log (id, body) VALUES (2, 'savepoint')");
    exec(eng, "RELEASE SAVEPOINT sp");
    exec(eng, "COMMIT");
    assert_check(
        eng,
        "transactions",
        "SELECT 'transactions' AS check, COUNT(*) = 2 AS ok FROM tx_log",
    );

    exec(
        eng,
        "CREATE VIEW account_names AS SELECT owner FROM accounts",
    );
    assert_check(
        eng,
        "view_query",
        "SELECT 'view_query' AS check, (SELECT COUNT(*) FROM account_names) = 4 AS ok",
    );
    exec(eng, "CREATE INDEX accounts_owner_idx ON accounts (owner)");
    exec(eng, "ANALYZE");
    assert_check(
        eng,
        "catalog_views",
        "SELECT 'catalog_views' AS check,
             (SELECT COUNT(*) FROM information_schema.columns WHERE table_name = 'accounts') >= 8
             AND EXISTS (
                 SELECT 1 FROM pg_catalog.pg_indexes
                 WHERE tablename = 'accounts' AND indexname = 'accounts_owner_idx'
             )
             AND EXISTS (
                 SELECT 1 FROM information_schema.views
                 WHERE table_name = 'account_names'
             )
             AND EXISTS (
                 SELECT 1 FROM information_schema.sequences
                 WHERE sequence_name = 'compat_seq'
             ) AS ok",
    );

    exec(eng, "SET work_mem TO '64MB'");
    exec(eng, "DISCARD ALL");
    assert_check(
        eng,
        "discard_all_search_path_has_public",
        "SELECT 'discard_all_search_path_has_public' AS check,
             EXISTS (
                 SELECT 1 FROM pg_catalog.pg_settings
                 WHERE name = 'search_path' AND setting LIKE '%public%'
             ) AS ok",
    );
}

fn postgresql_foreign_key_and_ddl_lifecycle(eng: &Engine) {
    exec(eng, "CREATE TABLE fk_parent (id INTEGER PRIMARY KEY)");
    exec(
        eng,
        "CREATE TABLE fk_child (
             id INTEGER PRIMARY KEY,
             parent_id INTEGER REFERENCES fk_parent(id)
         )",
    );
    exec(eng, "INSERT INTO fk_parent (id) VALUES (1), (2)");
    exec(eng, "INSERT INTO fk_child (id, parent_id) VALUES (10, 1)");
    assert_sql_error_contains(
        eng,
        "INSERT INTO fk_child (id, parent_id) VALUES (11, 99)",
        "foreign key",
    );
    assert_sql_error_contains(eng, "DELETE FROM fk_parent WHERE id = 1", "foreign key");
    assert_check(
        eng,
        "foreign_key_integrity",
        "SELECT 'foreign_key_integrity' AS check,
             (SELECT COUNT(*) FROM fk_parent) = 2
             AND (SELECT COUNT(*) FROM fk_child) = 1 AS ok",
    );

    exec(
        eng,
        "CREATE TABLE ctas_source (id INTEGER PRIMARY KEY, label TEXT, score INTEGER)",
    );
    exec(
        eng,
        "INSERT INTO ctas_source (id, label, score) VALUES
             (1, 'low', 10), (2, 'mid', 20), (3, 'high', 30)",
    );
    exec(
        eng,
        "CREATE TABLE ctas_copy AS
         SELECT id, label FROM ctas_source WHERE score >= 20",
    );
    exec(
        eng,
        "ALTER TABLE ctas_copy ADD COLUMN tag TEXT DEFAULT 'copied'",
    );
    exec(eng, "ALTER TABLE ctas_copy RENAME COLUMN label TO name");
    exec(eng, "ALTER TABLE ctas_copy DROP COLUMN tag");
    exec(
        eng,
        "CREATE INDEX IF NOT EXISTS ctas_copy_name_idx ON ctas_copy (name)",
    );
    assert_check(
        eng,
        "ddl_lifecycle_catalog",
        "SELECT 'ddl_lifecycle_catalog' AS check,
             (SELECT COUNT(*) FROM ctas_copy) = 2
             AND EXISTS (
                 SELECT 1 FROM information_schema.columns
                 WHERE table_name = 'ctas_copy' AND column_name = 'name'
             )
             AND NOT EXISTS (
                 SELECT 1 FROM information_schema.columns
                 WHERE table_name = 'ctas_copy' AND column_name = 'tag'
             )
             AND EXISTS (
                 SELECT 1 FROM pg_catalog.pg_indexes
                 WHERE tablename = 'ctas_copy' AND indexname = 'ctas_copy_name_idx'
             ) AS ok",
    );
    exec(eng, "TRUNCATE TABLE ctas_copy");
    assert_check(
        eng,
        "truncate_table",
        "SELECT 'truncate_table' AS check, COUNT(*) = 0 AS ok FROM ctas_copy",
    );
    exec(eng, "DROP TABLE IF EXISTS missing_compat_table");
    exec(eng, "DROP TABLE ctas_copy");
    assert_check(
        eng,
        "drop_table_catalog_cleanup",
        "SELECT 'drop_table_catalog_cleanup' AS check,
             NOT EXISTS (
                 SELECT 1 FROM information_schema.tables
                 WHERE table_name = 'ctas_copy'
             ) AS ok",
    );
}

#[test]
fn apache_age_cypher_compatibility_matrix() {
    let eng = Engine::new();
    apache_age_setup_graph(&eng);
    apache_age_read_query_matrix(&eng);
    apache_age_mutation_parameter_and_drop_matrix(&eng);
}

fn apache_age_setup_graph(eng: &Engine) {
    let created = query(eng, "SELECT create_graph('compat_age') AS ok");
    assert_eq!(created.rows[0].get("ok"), Some(&Value::Bool(true)));

    exec(
        eng,
        "SELECT * FROM ag_catalog.cypher('compat_age', $$
             CREATE (:Person {name: 'Alice', age: 30}),
                    (:Person {name: 'Bob', age: 40}),
                    (:Person {name: 'Carol', age: 25}),
                    (:City {name: 'Seoul'})
         $$) AS (ignored agtype)",
    );
    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             MATCH (a:Person), (b:Person)
             WHERE a.name = 'Alice' AND b.name = 'Bob'
             CREATE (a)-[:KNOWS {since: 2024}]->(b)
         $$) AS (ignored agtype)",
    );
    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             MATCH (a:Person), (b:Person)
             WHERE a.name = 'Bob' AND b.name = 'Carol'
             CREATE (a)-[:KNOWS {since: 2025}]->(b)
         $$) AS (ignored agtype)",
    );
    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             MATCH (a:Person), (c:City)
             WHERE a.name = 'Alice' AND c.name = 'Seoul'
             CREATE (a)-[:LIVES_IN {since: 2026}]->(c)
         $$) AS (ignored agtype)",
    );
}

fn apache_age_read_query_matrix(eng: &Engine) {
    assert_check(
        eng,
        "age_match_count",
        "SELECT 'age_match_count' AS check, COUNT(*) = 3 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Person)
             RETURN n.name AS name
         $$) AS (name agtype)",
    );
    assert_check(
        eng,
        "age_where_property",
        "SELECT 'age_where_property' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Person)
             WHERE n.name = 'Alice' AND n.age = 30
             RETURN n.name AS name
         $$) AS (name agtype)",
    );
    assert_check(
        eng,
        "age_relationship_match",
        "SELECT 'age_relationship_match' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (a:Person)-[r:KNOWS]->(b:Person)
             WHERE a.name = 'Alice' AND b.name = 'Bob' AND r.since = 2024
             RETURN r.since AS since
         $$) AS (since agtype)",
    );
    assert_check(
        eng,
        "age_variable_length",
        "SELECT 'age_variable_length' AS check, COUNT(*) = 2 AS ok
         FROM cypher('compat_age', $$
             MATCH (a:Person {name: 'Alice'})-[:KNOWS*1..2]->(b:Person)
             RETURN b.name AS name
         $$) AS (name agtype)",
    );
    assert_check(
        eng,
        "age_order_skip_limit",
        "SELECT 'age_order_skip_limit' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Person)
             RETURN n.name = 'Alice' AS ok
             ORDER BY n.age DESC
             SKIP 1
             LIMIT 1
         $$) AS (ok agtype)
         WHERE ok = true",
    );
    assert_check(
        eng,
        "age_distinct",
        "SELECT 'age_distinct' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (p:Person)-[:LIVES_IN]->(c:City)
             RETURN DISTINCT c.name AS city
         $$) AS (city agtype)",
    );
    assert_check(
        eng,
        "age_with_pipeline",
        "SELECT 'age_with_pipeline' AS check, COUNT(*) = 2 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Person)
             WITH n.name AS name, n.age AS age
             WHERE age > 28
             RETURN name
         $$) AS (name agtype)",
    );
    assert_check(
        eng,
        "age_optional_match",
        "SELECT 'age_optional_match' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Person {name: 'Alice'})
             OPTIONAL MATCH (n)-[:DOES_NOT_EXIST]->(x)
             RETURN n.name AS name
         $$) AS (name agtype)",
    );
    assert_check(
        eng,
        "age_id_labels_type",
        "SELECT 'age_id_labels_type' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (a:Person)-[r:KNOWS]->(b:Person)
             WHERE a.name = 'Alice'
             RETURN labels(a) = ['Person'] AND type(r) = 'KNOWS' AS ok
         $$) AS (ok agtype)
         WHERE ok = true",
    );
}

fn apache_age_mutation_parameter_and_drop_matrix(eng: &Engine) {
    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             MERGE (n:Tag {name: 'rust'})
         $$) AS (ignored agtype)",
    );
    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             MATCH (n:Tag {name: 'rust'})
             SET n.touched = true
         $$) AS (ignored agtype)",
    );
    assert_check(
        eng,
        "age_merge_set",
        "SELECT 'age_merge_set' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Tag {name: 'rust'})
             WHERE n.touched = true
             RETURN n.name AS name
         $$) AS (name agtype)",
    );

    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             UNWIND [1, 2, 3] AS x
             CREATE (:Number {value: x})
         $$) AS (ignored agtype)",
    );
    assert_check(
        eng,
        "age_unwind_create",
        "SELECT 'age_unwind_create' AS check, COUNT(*) = 3 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Number)
             RETURN n.value AS value
         $$) AS (value agtype)",
    );

    exec(
        eng,
        "PREPARE age_find_person AS
         SELECT 'age_prepared_params' AS check, COUNT(*) = 1 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Person)
             WHERE n.name = $name
             RETURN n.age AS age
         $$, $1) AS (age agtype)",
    );
    let prepared = query(eng, "EXECUTE age_find_person('{\"name\":\"Alice\"}')");
    assert_eq!(
        prepared.rows[0].get("check"),
        Some(&Value::Str("age_prepared_params".into()))
    );
    assert_eq!(prepared.rows[0].get("ok"), Some(&Value::Bool(true)));
    exec(eng, "DEALLOCATE age_find_person");

    exec(
        eng,
        "SELECT * FROM cypher('compat_age', $$
             MATCH (n:Number {value: 2})
             DETACH DELETE n
         $$) AS (ignored agtype)",
    );
    assert_check(
        eng,
        "age_detach_delete",
        "SELECT 'age_detach_delete' AS check, COUNT(*) = 2 AS ok
         FROM cypher('compat_age', $$
             MATCH (n:Number)
             RETURN n.value AS value
         $$) AS (value agtype)",
    );

    let dropped = query(eng, "SELECT drop_graph('compat_age', true) AS ok");
    assert_eq!(dropped.rows[0].get("ok"), Some(&Value::Bool(true)));
    assert!(!eng.has_graph("compat_age"));
}
