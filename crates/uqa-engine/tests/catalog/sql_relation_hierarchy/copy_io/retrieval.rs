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

fn native_text_score_bits(engine: &Engine, boolean: bool) -> std::collections::BTreeMap<i64, u64> {
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
    let terms = if boolean {
        vec!["alpha", "beta"]
    } else {
        vec!["alpha beta alpha"]
    };
    for query in terms {
        for entry in engine
            .search(
                "copy_search",
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
    let expected = native_text_score_bits(&engine, boolean);
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
