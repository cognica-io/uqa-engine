//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! NOT NULL, CHECK and partition violations name the relation by its own name and describe the failing row as `PostgreSQL` 18 does, showing a role only what it may read; table alterations report the rows they find without describing them. Every expectation is `PostgreSQL` 18.4's report of the same statement.

use uqa_engine::Engine;

/// The SQLSTATE, message and detail of the error `statement` fails with.
fn report(engine: &Engine, statement: &str) -> (String, String, Option<String>) {
    match engine.sql(statement, &[]) {
        Err(uqa_sql::SQLError::Diagnostic {
            sqlstate,
            message,
            detail,
            ..
        }) => (sqlstate, message, detail),
        Err(error) => (
            error.sqlstate().unwrap_or_default().to_owned(),
            error.to_string(),
            None,
        ),
        Ok(_) => panic!("{statement}: expected an error"),
    }
}

fn assert_reports(engine: &Engine, cases: &[(&str, &str, &str, Option<&str>)]) {
    for (statement, sqlstate, message, detail) in cases {
        assert_eq!(
            report(engine, statement),
            (
                (*sqlstate).to_owned(),
                (*message).to_owned(),
                detail.map(str::to_owned)
            ),
            "{statement}"
        );
    }
}

#[test]
fn violations_name_the_relation_and_describe_the_failing_row() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE SCHEMA other;
             CREATE TABLE typed (id int NOT NULL, v text, n numeric, d date, b boolean, x int[]);
             CREATE TABLE checked (id int, v int CHECK (v > 0));
             CREATE TABLE generated (x int NOT NULL, y int GENERATED ALWAYS AS (x * 2) STORED, z int GENERATED ALWAYS AS (x * 3) VIRTUAL, w text NOT NULL);
             CREATE TABLE other.qualified (a int NOT NULL, b int CHECK (b > 0));
             CREATE TABLE wide (a int CHECK (a > 0), b text, c text);
             CREATE TABLE periods (id int4range, valid_at daterange, PRIMARY KEY (id, valid_at WITHOUT OVERLAPS));
             INSERT INTO checked VALUES (1, 1)",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "INSERT INTO typed VALUES (NULL, 'hello', 1.50, '2026-01-02', true, ARRAY[1,2])",
                "23502",
                "null value in column \"id\" of relation \"typed\" violates not-null constraint",
                Some("Failing row contains (null, hello, 1.50, 2026-01-02, t, {1,2})."),
            ),
            // A value longer than 64 bytes is cut and marked.
            (
                "INSERT INTO typed VALUES (NULL, repeat('a', 100), NULL, NULL, NULL, NULL)",
                "23502",
                "null value in column \"id\" of relation \"typed\" violates not-null constraint",
                Some("Failing row contains (null, aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa..., null, null, null, null)."),
            ),
            (
                "INSERT INTO checked VALUES (2, -1)",
                "23514",
                "new row for relation \"checked\" violates check constraint \"checked_v_check\"",
                Some("Failing row contains (2, -1)."),
            ),
            (
                "UPDATE checked SET v = -2",
                "23514",
                "new row for relation \"checked\" violates check constraint \"checked_v_check\"",
                Some("Failing row contains (1, -2)."),
            ),
            // A stored generated column shows its value, a virtual one the word `virtual`.
            (
                "INSERT INTO generated (x, w) VALUES (1, NULL)",
                "23502",
                "null value in column \"w\" of relation \"generated\" violates not-null constraint",
                Some("Failing row contains (1, 2, virtual, null)."),
            ),
            (
                "INSERT INTO other.qualified VALUES (NULL, 1)",
                "23502",
                "null value in column \"a\" of relation \"qualified\" violates not-null constraint",
                Some("Failing row contains (null, 1)."),
            ),
            (
                "INSERT INTO other.qualified VALUES (1, -1)",
                "23514",
                "new row for relation \"qualified\" violates check constraint \"qualified_b_check\"",
                Some("Failing row contains (1, -1)."),
            ),
            (
                "INSERT INTO periods VALUES ('[1,2)', 'empty')",
                "23514",
                "empty WITHOUT OVERLAPS value found in column \"valid_at\" in relation \"periods\"",
                None,
            ),
            (
                "INSERT INTO wide VALUES (-1, 'it''s', E'line\\nbreak')",
                "23514",
                "new row for relation \"wide\" violates check constraint \"wide_a_check\"",
                Some("Failing row contains (-1, it's, line\nbreak)."),
            ),
        ],
    );
    // A value is cut at a character boundary: 64 bytes hold 32 of these two-byte letters.
    let word = "\u{3ba}\u{3cc}\u{3c3}\u{3bc}\u{3b5}";
    let statement = format!("INSERT INTO wide VALUES (-1, '{}', NULL)", word.repeat(10));
    let clipped = format!("{}\u{3ba}\u{3cc}", word.repeat(6));
    assert_eq!(
        report(&engine, &statement),
        (
            "23514".to_owned(),
            "new row for relation \"wide\" violates check constraint \"wide_a_check\"".to_owned(),
            Some(format!("Failing row contains (-1, {clipped}..., null)."))
        )
    );
}

#[test]
fn check_constraints_run_in_name_order_and_a_partition_checks_its_bound_after_them() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE ordered (a int, CONSTRAINT z CHECK (a > 0), CONSTRAINT b CHECK (a > 5), CONSTRAINT k CHECK (a > 3));
             CREATE TABLE ranged (k int, v text, w int) PARTITION BY RANGE (k);
             CREATE TABLE ranged1 PARTITION OF ranged FOR VALUES FROM (0) TO (10);
             ALTER TABLE ranged1 ALTER COLUMN v SET NOT NULL;
             ALTER TABLE ranged1 ADD CONSTRAINT small CHECK (w < 100);
             CREATE TABLE nested (k int, v int) PARTITION BY LIST (k);
             CREATE TABLE nested1 PARTITION OF nested FOR VALUES IN (1) PARTITION BY LIST (v);
             CREATE TABLE nested11 PARTITION OF nested1 FOR VALUES IN (5)",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[(
            "INSERT INTO ordered VALUES (-1)",
            "23514",
            "new row for relation \"ordered\" violates check constraint \"b\"",
            Some("Failing row contains (-1)."),
        )],
    );
    engine
        .sql("ALTER TABLE ordered ADD CONSTRAINT a CHECK (a > 10)", &[])
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "INSERT INTO ordered VALUES (-1)",
                "23514",
                "new row for relation \"ordered\" violates check constraint \"a\"",
                Some("Failing row contains (-1)."),
            ),
            // A partition that a statement names checks NOT NULL, then CHECK, then its bound.
            (
                "INSERT INTO ranged1 VALUES (20, NULL, 200)",
                "23502",
                "null value in column \"v\" of relation \"ranged1\" violates not-null constraint",
                Some("Failing row contains (20, null, 200)."),
            ),
            (
                "INSERT INTO ranged1 VALUES (20, 'a', 200)",
                "23514",
                "new row for relation \"ranged1\" violates check constraint \"small\"",
                Some("Failing row contains (20, a, 200)."),
            ),
            (
                "INSERT INTO ranged1 VALUES (20, 'a', 5)",
                "23514",
                "new row for relation \"ranged1\" violates partition constraint",
                Some("Failing row contains (20, a, 5)."),
            ),
            (
                "INSERT INTO ranged VALUES (5, NULL, 200)",
                "23502",
                "null value in column \"v\" of relation \"ranged1\" violates not-null constraint",
                Some("Failing row contains (5, null, 200)."),
            ),
            // A partitioned table that is a partition checks its own bound before it routes.
            (
                "INSERT INTO nested VALUES (1, 6)",
                "23514",
                "no partition of relation \"nested1\" found for row",
                Some("Partition key of the failing row contains (v) = (6)."),
            ),
            (
                "INSERT INTO nested1 VALUES (2, 5)",
                "23514",
                "new row for relation \"nested1\" violates partition constraint",
                Some("Failing row contains (2, 5)."),
            ),
            (
                "INSERT INTO nested11 VALUES (2, 5)",
                "23514",
                "new row for relation \"nested11\" violates partition constraint",
                Some("Failing row contains (2, 5)."),
            ),
        ],
    );
}

#[test]
fn rows_written_through_a_parent_are_described_in_its_columns() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE root (k int, v text, w int CONSTRAINT root_w_check CHECK (w > 0)) PARTITION BY RANGE (k);
             CREATE TABLE leaf (w int CONSTRAINT root_w_check CHECK (w > 0) CONSTRAINT leaf_small CHECK (w < 100), v text NOT NULL, k int, dropped int);
             ALTER TABLE leaf DROP COLUMN dropped;
             ALTER TABLE root ATTACH PARTITION leaf FOR VALUES FROM (0) TO (10);
             CREATE TABLE other_leaf PARTITION OF root FOR VALUES FROM (10) TO (20);
             CREATE TABLE ancestor (a int, b text);
             CREATE TABLE descendant (c int, CHECK (a < 10)) INHERITS (ancestor);
             INSERT INTO descendant VALUES (1, 'x', 7)",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[
            // A row routed from the root is described in the root's columns, one written to the partition in the partition's.
            (
                "INSERT INTO root VALUES (1, 'a', 200)",
                "23514",
                "new row for relation \"leaf\" violates check constraint \"leaf_small\"",
                Some("Failing row contains (1, a, 200)."),
            ),
            (
                "INSERT INTO leaf VALUES (200, 'a', 1)",
                "23514",
                "new row for relation \"leaf\" violates check constraint \"leaf_small\"",
                Some("Failing row contains (200, a, 1)."),
            ),
            (
                "INSERT INTO root VALUES (1, NULL, 5)",
                "23502",
                "null value in column \"v\" of relation \"leaf\" violates not-null constraint",
                Some("Failing row contains (1, null, 5)."),
            ),
            (
                "INSERT INTO leaf (k, v, w) VALUES (11, 'a', 5)",
                "23514",
                "new row for relation \"leaf\" violates partition constraint",
                Some("Failing row contains (5, a, 11)."),
            ),
        ],
    );
    engine
        .sql("INSERT INTO root VALUES (1, 'a', 5)", &[])
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "UPDATE root SET w = 300",
                "23514",
                "new row for relation \"leaf\" violates check constraint \"leaf_small\"",
                Some("Failing row contains (1, a, 300)."),
            ),
            (
                "UPDATE leaf SET w = 300",
                "23514",
                "new row for relation \"leaf\" violates check constraint \"leaf_small\"",
                Some("Failing row contains (300, a, 1)."),
            ),
            (
                "UPDATE root SET k = 50",
                "23514",
                "no partition of relation \"root\" found for row",
                Some("Partition key of the failing row contains (k) = (50)."),
            ),
            // An UPDATE of a partition may not move its row out of it.
            (
                "UPDATE leaf SET k = 50",
                "23514",
                "new row for relation \"leaf\" violates partition constraint",
                Some("Failing row contains (5, a, 50)."),
            ),
            (
                "UPDATE root SET k = 15, w = -3",
                "23514",
                "new row for relation \"other_leaf\" violates check constraint \"root_w_check\"",
                Some("Failing row contains (15, a, -3)."),
            ),
            // An inheritance child's row updated through its parent shows the parent's columns.
            (
                "UPDATE ancestor SET a = 20",
                "23514",
                "new row for relation \"descendant\" violates check constraint \"descendant_a_check\"",
                Some("Failing row contains (20, x)."),
            ),
            (
                "UPDATE descendant SET a = 20",
                "23514",
                "new row for relation \"descendant\" violates check constraint \"descendant_a_check\"",
                Some("Failing row contains (20, x, 7)."),
            ),
        ],
    );
}

#[test]
fn routing_reports_the_partition_key_of_the_row_it_rejects() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE listed (k text, v int) PARTITION BY LIST (k);
             CREATE TABLE multi (k int, j text, v int) PARTITION BY RANGE (k, j);
             CREATE TABLE expressions (k int, j text, \"Mixed Case\" int) PARTITION BY RANGE ((k * 2), lower(j), \"Mixed Case\", (j || 'x'));
             CREATE TABLE cased (k int, j text) PARTITION BY LIST ((CASE WHEN k > 0 THEN 1 ELSE 0 END));
             CREATE INDEX case_index ON cased ((CASE WHEN k > 0 THEN 1 ELSE 0 END))",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "INSERT INTO listed VALUES (NULL, 1)",
                "23514",
                "no partition of relation \"listed\" found for row",
                Some("Partition key of the failing row contains (k) = (null)."),
            ),
            (
                "INSERT INTO listed VALUES (repeat('z', 100), 1)",
                "23514",
                "no partition of relation \"listed\" found for row",
                Some("Partition key of the failing row contains (k) = (zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz...)."),
            ),
            (
                "INSERT INTO multi VALUES (1, 'abc', 1)",
                "23514",
                "no partition of relation \"multi\" found for row",
                Some("Partition key of the failing row contains (k, j) = (1, abc)."),
            ),
            // An expression key prints in parentheses unless it is a function call.
            (
                "INSERT INTO expressions VALUES (1, 'ABC', 3)",
                "23514",
                "no partition of relation \"expressions\" found for row",
                Some("Partition key of the failing row contains ((k * 2), lower(j), \"Mixed Case\", (j || 'x'::text)) = (2, abc, 3, ABCx)."),
            ),
            (
                "INSERT INTO cased VALUES (1, 'a')",
                "23514",
                "no partition of relation \"cased\" found for row",
                Some("Partition key of the failing row contains ((\nCASE\n    WHEN k > 0 THEN 1\n    ELSE 0\nEND)) = (1)."),
            ),
        ],
    );
    let definitions = engine
        .sql(
            "SELECT pg_get_partkeydef('expressions'::regclass) AS expressions, pg_get_partkeydef('cased'::regclass) AS cased, pg_get_indexdef('case_index'::regclass) AS case_index",
            &[],
        )
        .unwrap();
    let text = |column: &str| match definitions.rows[0].get(column) {
        Some(uqa_core::Value::Str(text)) => text.clone(),
        other => panic!("{column}: {other:?}"),
    };
    assert_eq!(
        text("expressions"),
        "RANGE (((k * 2)), lower(j), \"Mixed Case\", ((j || 'x'::text)))"
    );
    // A standalone expression indents its CASE from the left margin.
    assert_eq!(
        text("cased"),
        "LIST ((\nCASE\n    WHEN (k > 0) THEN 1\n    ELSE 0\nEND))"
    );
    assert_eq!(
        text("case_index"),
        "CREATE INDEX case_index ON ONLY public.cased USING btree ((\nCASE\n    WHEN (k > 0) THEN 1\n    ELSE 0\nEND))"
    );
}

#[test]
fn an_on_conflict_update_cannot_move_its_row_to_another_partition() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE conflicted (k int PRIMARY KEY, v int) PARTITION BY RANGE (k);
             CREATE TABLE conflicted1 PARTITION OF conflicted FOR VALUES FROM (0) TO (10);
             CREATE TABLE conflicted2 PARTITION OF conflicted FOR VALUES FROM (10) TO (20);
             INSERT INTO conflicted VALUES (1, 1)",
            &[],
        )
        .unwrap();
    let moved =
        Some("The result tuple would appear in a different partition than the original tuple.");
    assert_reports(
        &engine,
        &[
            (
                "INSERT INTO conflicted VALUES (1, 2) ON CONFLICT (k) DO UPDATE SET k = 15",
                "0A000",
                "invalid ON UPDATE specification",
                moved,
            ),
            // No partition accepts the row, which it would leave all the same.
            (
                "INSERT INTO conflicted VALUES (1, 2) ON CONFLICT (k) DO UPDATE SET k = 50",
                "0A000",
                "invalid ON UPDATE specification",
                moved,
            ),
            (
                "INSERT INTO conflicted1 VALUES (1, 2) ON CONFLICT (k) DO UPDATE SET k = 15",
                "0A000",
                "invalid ON UPDATE specification",
                moved,
            ),
        ],
    );
}

#[test]
fn table_alterations_report_the_rows_they_find_without_describing_them() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE altered (a int);
             INSERT INTO altered VALUES (-1);
             CREATE TABLE nullable (a int);
             INSERT INTO nullable VALUES (NULL);
             CREATE TABLE expressed (a int, g int GENERATED ALWAYS AS (a) STORED CHECK (g > -10), n int GENERATED ALWAYS AS (a) STORED NOT NULL);
             INSERT INTO expressed (a) VALUES (1), (-1);
             CREATE TABLE ranged (k int, v text, w int) PARTITION BY RANGE (k);
             CREATE TABLE ranged1 PARTITION OF ranged FOR VALUES FROM (0) TO (10);
             CREATE TABLE detached (k int, v text, w int);
             INSERT INTO detached VALUES (50, 'a', 1)",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "ALTER TABLE altered ADD COLUMN z int NOT NULL",
                "23502",
                "column \"z\" of relation \"altered\" contains null values",
                None,
            ),
            (
                "ALTER TABLE altered ADD COLUMN z int NOT NULL DEFAULT NULL",
                "23502",
                "column \"z\" of relation \"altered\" contains null values",
                None,
            ),
            (
                "ALTER TABLE altered ADD COLUMN z int DEFAULT 5 CHECK (z > 10)",
                "23514",
                "check constraint \"altered_z_check\" of relation \"altered\" is violated by some row",
                None,
            ),
            (
                "ALTER TABLE altered ADD CONSTRAINT altered_positive CHECK (a > 0)",
                "23514",
                "check constraint \"altered_positive\" of relation \"altered\" is violated by some row",
                None,
            ),
            (
                "ALTER TABLE altered ADD COLUMN g int GENERATED ALWAYS AS (a * 2) STORED CHECK (g > 0)",
                "23514",
                "check constraint \"altered_g_check\" of relation \"altered\" is violated by some row",
                None,
            ),
            (
                "ALTER TABLE altered ADD COLUMN h int GENERATED ALWAYS AS (NULLIF(a, -1)) STORED NOT NULL",
                "23502",
                "column \"h\" of relation \"altered\" contains null values",
                None,
            ),
            (
                "ALTER TABLE nullable ALTER COLUMN a SET NOT NULL",
                "23502",
                "column \"a\" of relation \"nullable\" contains null values",
                None,
            ),
            (
                "ALTER TABLE expressed ALTER COLUMN g SET EXPRESSION AS (a * 100)",
                "23514",
                "check constraint \"expressed_g_check\" of relation \"expressed\" is violated by some row",
                None,
            ),
            (
                "ALTER TABLE expressed ALTER COLUMN n SET EXPRESSION AS (NULLIF(a, -1))",
                "23502",
                "column \"n\" of relation \"expressed\" contains null values",
                None,
            ),
            (
                "ALTER TABLE ranged ATTACH PARTITION detached FOR VALUES FROM (10) TO (20)",
                "23514",
                "partition constraint of relation \"detached\" is violated by some row",
                None,
            ),
        ],
    );
    engine
        .sql(
            "ALTER TABLE altered ADD CONSTRAINT later_positive CHECK (a > 0) NOT VALID",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[(
            "ALTER TABLE altered VALIDATE CONSTRAINT later_positive",
            "23514",
            "check constraint \"later_positive\" of relation \"altered\" is violated by some row",
            None,
        )],
    );
}

#[test]
fn a_role_sees_only_the_columns_it_may_read_or_supplies() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE ROLE writer;
             CREATE TABLE secret (a int NOT NULL, b text, c int CHECK (c > 0));
             GRANT INSERT (a, c), UPDATE (c) ON secret TO writer;
             GRANT SELECT (b) ON secret TO writer;
             INSERT INTO secret VALUES (1, 'hidden', 1);
             CREATE TABLE supplied (a int NOT NULL, b text);
             GRANT INSERT (b) ON supplied TO writer;
             CREATE TABLE defaults (a int DEFAULT -1 CHECK (a > 0), b text);
             GRANT INSERT ON defaults TO writer;
             CREATE TABLE referenced (id int PRIMARY KEY);
             INSERT INTO referenced VALUES (1);
             CREATE TABLE referencing (id int, referenced_id int REFERENCES referenced ON UPDATE CASCADE, CONSTRAINT small_reference CHECK (referenced_id < 5));
             INSERT INTO referencing VALUES (10, 1);
             GRANT SELECT, UPDATE ON referenced TO writer;
             SET ROLE writer",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "INSERT INTO secret (a, c) VALUES (NULL, 1)",
                "23502",
                "null value in column \"a\" of relation \"secret\" violates not-null constraint",
                Some("Failing row contains (a, b, c) = (null, null, 1)."),
            ),
            (
                "INSERT INTO secret (a, c) VALUES (1, -1)",
                "23514",
                "new row for relation \"secret\" violates check constraint \"secret_c_check\"",
                Some("Failing row contains (a, b, c) = (1, null, -1)."),
            ),
            (
                "UPDATE secret SET c = -5",
                "23514",
                "new row for relation \"secret\" violates check constraint \"secret_c_check\"",
                Some("Failing row contains (b, c) = (hidden, -5)."),
            ),
            (
                "INSERT INTO supplied (b) VALUES ('q')",
                "23502",
                "null value in column \"a\" of relation \"supplied\" violates not-null constraint",
                Some("Failing row contains (b) = (q)."),
            ),
            // A role that can see no column gets no description.
            (
                "INSERT INTO defaults DEFAULT VALUES",
                "23514",
                "new row for relation \"defaults\" violates check constraint \"defaults_a_check\"",
                None,
            ),
            (
                "INSERT INTO defaults (b) VALUES ('x')",
                "23514",
                "new row for relation \"defaults\" violates check constraint \"defaults_a_check\"",
                Some("Failing row contains (b) = (x)."),
            ),
            // A referential action writes as the owner of the referencing table, who sees the whole row.
            (
                "UPDATE referenced SET id = 7",
                "23514",
                "new row for relation \"referencing\" violates check constraint \"small_reference\"",
                Some("Failing row contains (10, 7)."),
            ),
        ],
    );
    engine
        .sql(
            "RESET ROLE; GRANT SELECT (a) ON secret TO writer; SET ROLE writer",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[(
            "UPDATE secret SET c = -5 WHERE a = 1",
            "23514",
            "new row for relation \"secret\" violates check constraint \"secret_c_check\"",
            Some("Failing row contains (a, b, c) = (1, hidden, -5)."),
        )],
    );
}

#[test]
fn a_role_sees_a_partition_key_only_when_it_may_read_each_key() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE ROLE writer;
             CREATE TABLE keyed (k int, v int) PARTITION BY RANGE (k);
             CREATE TABLE keyed1 PARTITION OF keyed FOR VALUES FROM (0) TO (10);
             GRANT INSERT ON keyed TO writer;
             CREATE TABLE expression_keyed (k int, v int) PARTITION BY RANGE ((k * 2));
             CREATE TABLE expression_keyed1 PARTITION OF expression_keyed FOR VALUES FROM (0) TO (10);
             GRANT INSERT ON expression_keyed TO writer;
             GRANT SELECT (k) ON expression_keyed TO writer;
             SET ROLE writer",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[
            (
                "INSERT INTO keyed VALUES (50, 1)",
                "23514",
                "no partition of relation \"keyed\" found for row",
                None,
            ),
            // An expression key needs the right to read the table.
            (
                "INSERT INTO expression_keyed VALUES (50, 1)",
                "23514",
                "no partition of relation \"expression_keyed\" found for row",
                None,
            ),
        ],
    );
    engine
        .sql(
            "RESET ROLE; GRANT SELECT (k) ON keyed TO writer; SET ROLE writer",
            &[],
        )
        .unwrap();
    assert_reports(
        &engine,
        &[(
            "INSERT INTO keyed VALUES (50, 1)",
            "23514",
            "no partition of relation \"keyed\" found for row",
            Some("Partition key of the failing row contains (k) = (50)."),
        )],
    );
}

#[test]
fn table_rewrites_check_the_rewritten_rows_against_valid_constraints_only() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE unrelated (a int);
             INSERT INTO unrelated VALUES (-1);
             ALTER TABLE unrelated ADD CONSTRAINT unrelated_positive CHECK (a > 0) NOT VALID;
             CREATE TABLE widened (a int, CONSTRAINT widened_small CHECK (a < 100));
             INSERT INTO widened VALUES (50);
             CREATE TABLE unchecked (a int, b int);
             INSERT INTO unchecked VALUES (-1, NULL);
             ALTER TABLE unchecked ADD CONSTRAINT unchecked_positive CHECK (a > 0) NOT VALID;
             ALTER TABLE unchecked ADD CONSTRAINT unchecked_b_present NOT NULL b NOT VALID;
             CREATE TABLE parent (id int PRIMARY KEY);
             INSERT INTO parent VALUES (1), (2);
             CREATE TABLE child (id int, parent_id int REFERENCES parent);
             INSERT INTO child VALUES (10, 1);
             CREATE TABLE orphans (id int, parent_id int);
             INSERT INTO orphans VALUES (1, 99);
             ALTER TABLE orphans ADD CONSTRAINT orphans_parent FOREIGN KEY (parent_id) REFERENCES parent NOT VALID",
            &[],
        )
        .unwrap();
    // Another table's NOT VALID constraint takes no part in a rewrite, and neither do the rewritten table's own.
    for statement in [
        "ALTER TABLE widened ALTER COLUMN a TYPE bigint",
        "ALTER TABLE unchecked ALTER COLUMN a TYPE bigint USING a * 2",
        "ALTER TABLE unchecked ALTER COLUMN b TYPE bigint",
        "ALTER TABLE orphans ALTER COLUMN parent_id TYPE bigint",
    ] {
        engine
            .sql(statement, &[])
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    let rows = engine.sql("SELECT a, b FROM unchecked", &[]).unwrap().rows;
    assert_eq!(rows[0]["a"], uqa_core::Value::Int(-2));
    assert_eq!(rows[0]["b"], uqa_core::Value::Null);
    assert_reports(
        &engine,
        &[(
            "ALTER TABLE widened ALTER COLUMN a TYPE int USING a * 3",
            "23514",
            "check constraint \"widened_small\" of relation \"widened\" is violated by some row",
            None,
        )],
    );
    // A rewrite of a referenced key validates the foreign keys that reference it, whatever the replication role.
    for setting in ["origin", "replica"] {
        engine
            .sql(&format!("SET session_replication_role = {setting}"), &[])
            .unwrap();
        let error = engine
            .sql(
                "ALTER TABLE parent ALTER COLUMN id TYPE bigint USING id + 1",
                &[],
            )
            .unwrap_err();
        assert_eq!(
            (error.sqlstate(), error.to_string().as_str()),
            (
                Some("23503"),
                "insert or update on table \"child\" violates foreign key constraint \"child_parent_id_fkey\""
            ),
            "{setting}"
        );
    }
}

#[test]
fn table_rewrites_check_keys_across_the_rewritten_rows() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE shifted (id int PRIMARY KEY);
             INSERT INTO shifted VALUES (1), (2);
             CREATE TABLE halved (id int PRIMARY KEY);
             INSERT INTO halved VALUES (1), (2);
             CREATE TABLE uniq (a int UNIQUE, b int);
             INSERT INTO uniq VALUES (1, 1), (2, 2);
             CREATE TABLE gen (a int, g int GENERATED ALWAYS AS (a) STORED UNIQUE);
             INSERT INTO gen (a) VALUES (1), (2)",
            &[],
        )
        .unwrap();
    // Keys move together, so a row may take a key another row is giving up.
    for statement in [
        "ALTER TABLE shifted ALTER COLUMN id TYPE bigint USING id + 1",
        "ALTER TABLE halved ALTER COLUMN id TYPE bigint USING id / 2",
        "ALTER TABLE uniq ALTER COLUMN a TYPE bigint USING a + 1",
        "ALTER TABLE gen ALTER COLUMN g SET EXPRESSION AS (a + 1)",
    ] {
        engine
            .sql(statement, &[])
            .unwrap_or_else(|error| panic!("{statement}: {error}"));
    }
    let shifted = engine
        .sql("SELECT id FROM shifted WHERE id = 3", &[])
        .unwrap();
    assert_eq!(shifted.rows.len(), 1);
    assert_reports(
        &engine,
        &[
            (
                "ALTER TABLE halved ALTER COLUMN id TYPE int USING id / 10",
                "23505",
                "could not create unique index \"halved_pkey\"",
                Some("Key (id)=(0) is duplicated."),
            ),
            (
                "ALTER TABLE halved ALTER COLUMN id TYPE int USING NULL",
                "23502",
                "column \"id\" of relation \"halved\" contains null values",
                None,
            ),
            (
                "ALTER TABLE uniq ALTER COLUMN a TYPE bigint USING 7",
                "23505",
                "could not create unique index \"uniq_a_key\"",
                Some("Key (a)=(7) is duplicated."),
            ),
            (
                "ALTER TABLE uniq ADD COLUMN g int GENERATED ALWAYS AS (1) STORED UNIQUE",
                "23505",
                "could not create unique index \"uniq_g_key\"",
                Some("Key (g)=(1) is duplicated."),
            ),
            (
                "ALTER TABLE gen ALTER COLUMN g SET EXPRESSION AS (5)",
                "23505",
                "could not create unique index \"gen_g_key\"",
                Some("Key (g)=(5) is duplicated."),
            ),
        ],
    );
}
