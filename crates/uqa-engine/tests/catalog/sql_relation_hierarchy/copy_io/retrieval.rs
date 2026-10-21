//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Bounded complete text scores at the public COPY boundary.

use super::{copy_engine, exec, Engine};

struct Lines {
    count: usize,
    value: usize,
}
impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            if *byte == b'\n' {
                self.count += 1;
                assert_eq!(self.value, self.count);
                self.value = 0;
            } else {
                assert!(byte.is_ascii_digit());
                self.value = self.value * 10 + usize::from(*byte - b'0');
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
#[test]
fn copy_text_matches_capture_scores_without_complete_posting_vectors() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, body TEXT);
         INSERT INTO copy_search SELECT n,'alpha beta' FROM generate_series(1,32768) AS s(n);
         CREATE INDEX copy_search_body ON copy_search USING gin(body);
         SET work_mem='64kB'",
    );
    let mut output = Lines { count: 0, value: 0 };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            engine
                .copy_to(
                    "COPY (SELECT id FROM copy_search WHERE text_match(body,'alpha beta alpha')) TO STDOUT",
                    &mut output,
                )
                .unwrap(),
            32_768
        );
    });
    assert_eq!(output.count, 32_768);
    assert_eq!(output.value, 0);
    // The input scores and posting payloads exceed 2MiB; only bounded capture and output batches may remain live.
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn copy_exact_text_fusion_does_not_retain_complete_signal_populations() {
    verify_complete_fusion_memory("fuse_bayesian_evidence");
}

#[test]
fn copy_positive_text_pool_does_not_retain_complete_signal_populations() {
    verify_complete_fusion_memory("pool_positive_evidence");
}

fn verify_complete_fusion_memory(function: &str) {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, body TEXT);
         INSERT INTO copy_search SELECT n,'alpha beta' FROM generate_series(1,32768) AS s(n);
         CREATE INDEX copy_search_body ON copy_search USING gin(body);
         SET work_mem='64kB'",
    );
    let mut output = Lines { count: 0, value: 0 };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(
            engine
                .copy_to(
                    &format!(
                        "COPY (SELECT id FROM copy_search WHERE {function}(
                         bayesian_match(body,'alpha'),bayesian_match(body,'beta'))) TO STDOUT"
                    ),
                    &mut output,
                )
                .unwrap(),
            32_768
        );
    });
    assert_eq!(output.count, 32_768);
    assert_eq!(output.value, 0);
    // Two complete probability signals and their union must not remain in memory.
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_text_capture_preserves_scores_and_statement_generation(#[case] provider: usize) {
    verify_text_capture(provider, false);
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_boolean_text_capture_preserves_scores_and_statement_generation(#[case] provider: usize) {
    verify_text_capture(provider, true);
}

fn native_text_score_bits(
    engine: &Engine,
    table: &str,
    boolean: bool,
) -> std::collections::BTreeMap<i64, u64> {
    let ids = engine
        .sql(&format!("SELECT id,_doc_id FROM ONLY {table}"), &[])
        .unwrap();
    let ids = ids
        .rows
        .iter()
        .map(|row| {
            let (uqa_core::Value::Int(id), uqa_core::Value::Int(doc_id)) =
                (&row["id"], &row["_doc_id"])
            else {
                panic!("integer identities");
            };
            (u64::try_from(*doc_id).unwrap(), *id)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut expected = std::collections::BTreeMap::<i64, f64>::new();
    let terms = if boolean {
        vec!["alpha", "beta"]
    } else {
        vec!["alpha beta alpha"]
    };
    for query in terms {
        for entry in engine
            .search(
                table,
                "body",
                query,
                &uqa_scoring::ScoringMode::BM25(uqa_scoring::BM25Params::default()),
                usize::MAX,
            )
            .unwrap()
        {
            expected
                .entry(ids[&entry.doc_id])
                .and_modify(|score| *score += entry.score)
                .or_insert(entry.score);
        }
    }
    expected
        .into_iter()
        .map(|(id, score)| (id, score.to_bits()))
        .collect::<std::collections::BTreeMap<_, _>>()
}

fn verify_text_capture(provider: usize, boolean: bool) {
    struct Interleave<'a> {
        writer: Option<&'a Engine>,
        bytes: Vec<u8>,
    }
    impl std::io::Write for Interleave<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if let Some(writer) = self.writer.take() {
                exec(
                    writer,
                    "UPDATE copy_search SET body='changed' WHERE id=512;
                    DELETE FROM copy_search WHERE id=513;
                    INSERT INTO copy_search VALUES(514,'alpha')",
                );
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("text-copy.db");
    let engine = copy_engine(provider, &path);
    let body = if boolean {
        "CASE WHEN n%3=0 THEN 'alpha alpha beta' WHEN n%3=1 THEN 'alpha' ELSE 'beta' END"
    } else {
        "CASE WHEN n%3=0 THEN 'alpha alpha beta' ELSE 'alpha' END"
    };
    exec(
        &engine,
        &format!(
            "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO copy_search SELECT n, {body} FROM generate_series(1,513) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body);
        SET work_mem='64kB'"
        ),
    );
    let expected = native_text_score_bits(&engine, "copy_search", boolean);
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut output = Interleave {
        writer: writer.as_ref(),
        bytes: Vec::new(),
    };
    let predicate = if boolean {
        "text_match(body,'alpha') OR text_match(body,'beta')"
    } else {
        "text_match(body,'alpha beta alpha')"
    };
    assert_eq!(
        engine
            .copy_to(
                &format!("COPY (SELECT id,_score FROM copy_search WHERE {predicate}) TO STDOUT"),
                &mut output
            )
            .unwrap(),
        513
    );
    let count_query = if boolean {
        "SELECT count(*) AS n FROM copy_search WHERE text_match(body,'alpha') OR text_match(body,'beta')"
    } else {
        "SELECT count(*) AS n FROM copy_search WHERE text_match(body,'alpha')"
    };
    let actual = String::from_utf8(output.bytes)
        .unwrap()
        .lines()
        .map(|line| {
            let (id, score) = line.split_once('\t').unwrap();
            (
                id.parse::<i64>().unwrap(),
                score.parse::<f64>().unwrap().to_bits(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    let count = engine.sql(count_query, &[]).unwrap();
    assert_eq!(
        count.rows[0]["n"],
        uqa_core::Value::Int(if provider == 0 { 513 } else { 512 })
    );
    drop(writer);
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        let count = reopened.sql(count_query, &[]).unwrap();
        assert_eq!(count.rows[0]["n"], uqa_core::Value::Int(512));
    }
}

#[rstest::rstest]
#[case::intersection("text_match(body,'alpha') AND text_match(body,'beta')")]
#[case::union("text_match(body,'alpha') OR text_match(body,'beta')")]
fn copy_boolean_text_scores_do_not_retain_complete_child_postings(#[case] predicate: &str) {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, body TEXT);
        INSERT INTO copy_search SELECT n,'alpha beta' FROM generate_series(1,32768) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body);
        SET work_mem='64kB'",
    );
    let mut output = Lines { count: 0, value: 0 };
    let sql = format!("COPY (SELECT id FROM copy_search WHERE {predicate}) TO STDOUT");
    let allocated = allocation_counter::measure(|| {
        assert_eq!(engine.copy_to(&sql, &mut output).unwrap(), 32_768);
    });
    assert_eq!(output.count, 32_768);
    assert_eq!(output.value, 0);
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

#[rstest::rstest]
#[case::projected_field(false)]
#[case::indexed_field(true)]
fn copy_filtered_text_scores_do_not_retain_complete_child_postings(#[case] indexed: bool) {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, body TEXT, grp INTEGER);
        INSERT INTO copy_search SELECT n,'alpha beta',1 FROM generate_series(1,32768) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body)",
    );
    if indexed {
        exec(&engine, "CREATE INDEX copy_search_grp ON copy_search(grp)");
    }
    exec(&engine, "SET work_mem='64kB'");
    for operation in ["OR", "AND"] {
        let mut output = Lines { count: 0, value: 0 };
        let sql = format!(
            "COPY (SELECT id FROM copy_search WHERE text_match(body,'alpha') {operation} grp=1) TO STDOUT"
        );
        let allocated = allocation_counter::measure(|| {
            assert_eq!(engine.copy_to(&sql, &mut output).unwrap(), 32_768);
        });
        assert_eq!(output.count, 32_768);
        assert_eq!(output.value, 0);
        assert!(
            allocated.bytes_max < 2 * 1024 * 1024,
            "{operation}: {allocated:?}"
        );
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_filtered_text_capture_keeps_scores_nulls_overlay_and_generation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("filtered-text-copy.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, body TEXT NOT NULL, grp INTEGER);
        INSERT INTO copy_search SELECT n,CASE WHEN n%3=0 THEN 'alpha beta' ELSE 'gamma' END,
            CASE WHEN n%2=0 THEN 1 ELSE NULL END FROM generate_series(1,513) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body)",
    );
    let text = "text_match(body,'alpha beta alpha')";
    let native = native_text_score_bits(&engine, "copy_search", false);
    let expected = (1..=513)
        .filter(|id| id % 2 == 0 || id % 3 == 0)
        .map(|id| (id, native.get(&id).copied().unwrap_or(0.0_f64.to_bits())))
        .collect::<std::collections::BTreeMap<_, _>>();
    check_filtered_text_queries(&engine, text, &native, &expected);
    check_filtered_text_commands(&engine, text, &expected);
    let sql = format!("COPY (SELECT id,_score FROM copy_search WHERE {text} OR grp=1) TO STDOUT");
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut output = FilteredInterleave {
        writer: writer.as_ref(),
        bytes: Vec::new(),
    };
    assert_eq!(
        engine.copy_to(&sql, &mut output).unwrap(),
        expected.len() as u64
    );
    assert_eq!(copied_text_scores(output.bytes), expected);
    drop(writer);
    let count_sql = format!("SELECT count(*) AS n FROM copy_search WHERE {text} OR grp=1");
    let count = if provider == 0 { 342 } else { 341 };
    assert_eq!(
        engine.sql(&count_sql, &[]).unwrap().rows[0]["n"],
        uqa_core::Value::Int(count)
    );
    drop(engine);
    if provider != 0 {
        let reopened = copy_engine(provider, &path);
        assert_eq!(
            reopened.sql(&count_sql, &[]).unwrap().rows[0]["n"],
            uqa_core::Value::Int(341)
        );
    }
}

struct FilteredInterleave<'a> {
    writer: Option<&'a Engine>,
    bytes: Vec<u8>,
}
impl std::io::Write for FilteredInterleave<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(writer) = self.writer.take() {
            exec(
                writer,
                "UPDATE copy_search SET body='gamma',grp=NULL WHERE id=512;
                 DELETE FROM copy_search WHERE id=513;
                 INSERT INTO copy_search VALUES(514,'gamma',1)",
            );
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn copied_text_scores(bytes: Vec<u8>) -> std::collections::BTreeMap<i64, u64> {
    String::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| {
            let (id, score) = line.split_once('\t').unwrap();
            (
                id.parse::<i64>().unwrap(),
                score.parse::<f64>().unwrap().to_bits(),
            )
        })
        .collect()
}

fn check_filtered_text_queries(
    engine: &Engine,
    text: &str,
    native: &std::collections::BTreeMap<i64, u64>,
    expected: &std::collections::BTreeMap<i64, u64>,
) {
    for indexed in [false, true] {
        if indexed {
            exec(
                engine,
                "RESET work_mem; CREATE INDEX copy_search_grp ON copy_search(grp)",
            );
        }
        exec(engine, "SET work_mem='64kB'");
        let cases = [
            (format!("{text} OR grp=1"), expected.clone()),
            (
                format!("({text} OR grp=1) AND grp IS NULL"),
                expected
                    .iter()
                    .filter(|(id, _)| *id % 2 != 0)
                    .map(|(id, score)| (*id, *score))
                    .collect(),
            ),
            (
                format!("({text} AND grp=1) OR id=1"),
                native
                    .iter()
                    .filter(|(id, _)| *id % 2 == 0)
                    .map(|(id, score)| (*id, *score))
                    .chain([(1, 0.0_f64.to_bits())])
                    .collect(),
            ),
            (
                format!("NOT {text} OR grp=1"),
                (1..=513)
                    .filter(|id| id % 3 != 0 || id % 2 == 0)
                    .map(|id| (id, 0.0_f64.to_bits()))
                    .collect(),
            ),
        ];
        for (predicate, expected) in cases {
            let mut output = Vec::new();
            let count = engine
                .copy_to(
                    &format!(
                        "COPY (SELECT id,_score FROM copy_search WHERE {predicate}) TO STDOUT"
                    ),
                    &mut output,
                )
                .unwrap();
            assert_eq!(
                count,
                expected.len() as u64,
                "{predicate}, indexed={indexed}"
            );
            assert_eq!(
                copied_text_scores(output),
                expected,
                "{predicate}, indexed={indexed}"
            );
        }
    }
}

fn check_filtered_text_commands(
    engine: &Engine,
    text: &str,
    expected: &std::collections::BTreeMap<i64, u64>,
) {
    exec(
        engine,
        "BEGIN; UPDATE copy_search SET grp=1 WHERE id=1;
        DELETE FROM copy_search WHERE id=2; INSERT INTO copy_search VALUES(515,'gamma',1)",
    );
    let command_native = native_text_score_bits(engine, "copy_search", false);
    let mut command_expected = expected
        .keys()
        .copied()
        .map(|id| {
            (
                id,
                command_native
                    .get(&id)
                    .copied()
                    .unwrap_or(0.0_f64.to_bits()),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    command_expected.insert(1, 0.0_f64.to_bits());
    command_expected.remove(&2);
    command_expected.insert(515, 0.0_f64.to_bits());
    let sql = format!("COPY (SELECT id,_score FROM copy_search WHERE {text} OR grp=1) TO STDOUT");
    let mut command_output = Vec::new();
    assert_eq!(
        engine.copy_to(&sql, &mut command_output).unwrap(),
        command_expected.len() as u64
    );
    assert_eq!(copied_text_scores(command_output), command_expected);
    exec(engine, "ROLLBACK");
}

#[test]
fn copy_all_field_text_scores_do_not_retain_the_complete_population() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
        INSERT INTO copy_search SELECT n,'alpha','beta' FROM generate_series(1,32768) AS s(n);
        CREATE INDEX copy_search_title ON copy_search USING gin(title);
        CREATE INDEX copy_search_body ON copy_search USING gin(body);
        SET work_mem='64kB'",
    );
    let mut output = Lines { count: 0, value: 0 };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(engine.copy_to("COPY (SELECT id FROM copy_search WHERE text_match('_all','alpha beta')) TO STDOUT", &mut output).unwrap(), 32_768);
    });
    assert_eq!(output.count, 32_768);
    assert_eq!(output.value, 0);
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

fn native_all_field_score_bits(engine: &Engine) -> std::collections::BTreeMap<i64, u64> {
    let ids = engine
        .sql("SELECT id,_doc_id FROM copy_search", &[])
        .unwrap();
    let ids = ids
        .rows
        .iter()
        .map(|row| {
            let (uqa_core::Value::Int(id), uqa_core::Value::Int(doc_id)) =
                (&row["id"], &row["_doc_id"])
            else {
                panic!("integer identities");
            };
            (u64::try_from(*doc_id).unwrap(), *id)
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut expected = std::collections::BTreeMap::<i64, f64>::new();
    for field in ["body", "title"] {
        for entry in engine
            .search(
                "copy_search",
                field,
                "alpha beta alpha",
                &uqa_scoring::ScoringMode::BM25(uqa_scoring::BM25Params::default()),
                usize::MAX,
            )
            .unwrap()
        {
            expected
                .entry(ids[&entry.doc_id])
                .and_modify(|score| *score = score.max(entry.score))
                .or_insert(entry.score);
        }
    }
    expected
        .into_iter()
        .map(|(id, score)| (id, score.to_bits()))
        .collect()
}

struct AllFieldSink<'a> {
    writer: Option<&'a Engine>,
    bytes: Vec<u8>,
}

impl std::io::Write for AllFieldSink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(writer) = self.writer.take() {
            exec(
                writer,
                "UPDATE copy_search SET title='changed',body='changed' WHERE id=512;
                DELETE FROM copy_search WHERE id=513;
                INSERT INTO copy_search VALUES(514,'alpha',NULL)",
            );
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_all_field_text_capture_preserves_max_scores_and_generation(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("all-field-copy.db");
    let engine = copy_engine(provider, &path);
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER PRIMARY KEY, title TEXT, body TEXT);
        INSERT INTO copy_search SELECT n,
            CASE WHEN n%3=0 THEN 'alpha alpha' WHEN n%3=1 THEN 'other' ELSE NULL END,
            CASE WHEN n%2=0 THEN 'alpha beta alpha' ELSE 'beta' END
            FROM generate_series(1,513) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body);
        CREATE INDEX copy_search_title ON copy_search USING gin(title);
        SET work_mem='64kB'",
    );
    let expected = native_all_field_score_bits(&engine);
    assert_eq!(expected.len(), 513);
    let writer = (provider != 0).then(|| engine.new_session().unwrap());
    let mut output = AllFieldSink {
        writer: writer.as_ref(),
        bytes: Vec::new(),
    };
    assert_eq!(engine.copy_to("COPY (SELECT id,_score FROM copy_search WHERE text_match('_all','alpha beta alpha')) TO STDOUT", &mut output).unwrap(), 513);
    let actual = String::from_utf8(output.bytes)
        .unwrap()
        .lines()
        .map(|line| {
            let (id, score) = line.split_once('\t').unwrap();
            (
                id.parse::<i64>().unwrap(),
                score.parse::<f64>().unwrap().to_bits(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(actual, expected);
    let sql = "SELECT count(*) AS n FROM copy_search WHERE text_match('_all','alpha beta alpha')";
    assert_eq!(
        engine.sql(sql, &[]).unwrap().rows[0]["n"],
        uqa_core::Value::Int(if provider == 0 { 513 } else { 512 })
    );
    drop(writer);
    drop(engine);
    if provider != 0 {
        let engine = copy_engine(provider, &path);
        assert_eq!(
            engine.sql(sql, &[]).unwrap().rows[0]["n"],
            uqa_core::Value::Int(512)
        );
    }
}

#[test]
fn copy_inherited_text_scores_do_not_retain_physical_table_populations() {
    let engine = Engine::new();
    exec(
        &engine,
        "CREATE TABLE copy_search(id INTEGER, body TEXT);
        CREATE TABLE copy_child() INHERITS (copy_search);
        INSERT INTO copy_search SELECT n,'alpha beta' FROM generate_series(1,16384) AS s(n);
        INSERT INTO copy_child SELECT n,'alpha beta' FROM generate_series(16385,32768) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body);
        CREATE INDEX copy_child_body ON copy_child USING gin(body);
        SET work_mem='64kB'",
    );
    let mut output = Lines { count: 0, value: 0 };
    let allocated = allocation_counter::measure(|| {
        assert_eq!(engine.copy_to("COPY (SELECT id FROM copy_search WHERE text_match(body,'alpha beta alpha')) TO STDOUT", &mut output).unwrap(), 32_768);
    });
    assert_eq!(output.count, 32_768);
    assert_eq!(output.value, 0);
    assert!(allocated.bytes_max < 2 * 1024 * 1024, "{allocated:?}");
}

struct HierarchySink<'a> {
    writer: Option<&'a Engine>,
    bytes: Vec<u8>,
}

impl std::io::Write for HierarchySink<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if let Some(writer) = self.writer.take() {
            exec(
                writer,
                "UPDATE copy_search SET body='changed' WHERE id IN (1,512);
                DELETE FROM copy_search WHERE id=513;
                INSERT INTO copy_high VALUES(514,'alpha')",
            );
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn create_text_hierarchy(engine: &Engine, partitioned: bool) {
    if partitioned {
        exec(
            engine,
            "CREATE TABLE copy_search(id INTEGER, body TEXT) PARTITION BY RANGE(id);
            CREATE TABLE copy_low PARTITION OF copy_search FOR VALUES FROM (0) TO (257);
            CREATE TABLE copy_high PARTITION OF copy_search DEFAULT",
        );
    } else {
        exec(
            engine,
            "CREATE TABLE copy_search(id INTEGER, body TEXT);
            CREATE TABLE copy_low() INHERITS(copy_search);
            CREATE TABLE copy_high() INHERITS(copy_search)",
        );
    }
    exec(
        engine,
        "INSERT INTO copy_low SELECT n,'alpha alpha beta' FROM generate_series(1,256) AS s(n);
        INSERT INTO copy_high SELECT n,'alpha beta' FROM generate_series(257,513) AS s(n);
        CREATE INDEX copy_search_body ON copy_search USING gin(body);
        CREATE INDEX copy_low_body ON copy_low USING gin(body);
        CREATE INDEX copy_high_body ON copy_high USING gin(body);
        SET work_mem='64kB'",
    );
}

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn copy_hierarchy_text_capture_keeps_physical_scores_and_generations(#[case] provider: usize) {
    for partitioned in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hierarchy-copy.db");
        let engine = copy_engine(provider, &path);
        create_text_hierarchy(&engine, partitioned);
        let mut expected = native_text_score_bits(&engine, "copy_low", false);
        expected.extend(native_text_score_bits(&engine, "copy_high", false));
        assert_eq!(expected.len(), 513);
        let writer = (provider != 0).then(|| engine.new_session().unwrap());
        let mut output = HierarchySink {
            writer: writer.as_ref(),
            bytes: Vec::new(),
        };
        assert_eq!(engine.copy_to("COPY (SELECT id,_score,tableoid::regclass::text FROM copy_search WHERE text_match(body,'alpha beta alpha')) TO STDOUT", &mut output).unwrap(), 513);
        let text = String::from_utf8(output.bytes).unwrap();
        let actual = text
            .lines()
            .map(|line| {
                let fields = line.split('\t').collect::<Vec<_>>();
                assert_eq!(fields.len(), 3);
                let id = fields[0].parse::<i64>().unwrap();
                assert_eq!(fields[2], if id < 257 { "copy_low" } else { "copy_high" });
                (id, fields[1].parse::<f64>().unwrap().to_bits())
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(actual, expected);
        let sql = "SELECT count(*) AS n FROM copy_search WHERE text_match(body,'alpha')";
        assert_eq!(
            engine.sql(sql, &[]).unwrap().rows[0]["n"],
            uqa_core::Value::Int(if provider == 0 { 513 } else { 511 })
        );
        assert_eq!(
            engine
                .sql(
                    "SELECT count(*) AS n FROM ONLY copy_search WHERE text_match(body,'alpha')",
                    &[]
                )
                .unwrap()
                .rows[0]["n"],
            uqa_core::Value::Int(0)
        );
        drop(writer);
        drop(engine);
        if provider != 0 {
            let engine = copy_engine(provider, &path);
            assert_eq!(
                engine.sql(sql, &[]).unwrap().rows[0]["n"],
                uqa_core::Value::Int(511)
            );
        }
    }
}
