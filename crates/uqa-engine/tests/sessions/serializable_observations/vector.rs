//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Vector searches observe canonical candidates before returning empty, indexed or calibrated results.

use super::{
    assert_cycle, finish, fixtures, EngineDriver, OperatorOutput, OperatorTree, OperatorTreeDriver,
    Session,
};

fn prepare(session: &Session, kind: &str, populated: bool) {
    session
        .sql("CREATE TABLE vectors (id INTEGER PRIMARY KEY, embedding VECTOR(2), other VECTOR(2))");
    if kind != "exact" {
        let options = if kind == "ivf" {
            "WITH (lists = 1, probes = 1, train_threshold = 1)"
        } else {
            "WITH (m = 4, ef_construction = 16, ef_search = 16, seed = 7)"
        };
        session.sql(&format!(
            "CREATE INDEX vector_index ON vectors USING {kind} (embedding) {options}"
        ));
    }
    if populated {
        session.sql("INSERT INTO vectors VALUES (1, ARRAY[1.0, 0.0], NULL)");
    }
}

fn search(session: &Session, route: &str, k: usize) -> usize {
    let tree = match route {
        "threshold" => {
            return session
                .engine
                .vector_similarity_search("vectors", "embedding", vec![1.0, 0.0], 0.5)
                .unwrap()
                .len()
        }
        "sql" => {
            return session
                .sql(&format!(
                    "SELECT id FROM vectors WHERE knn_match(embedding, ARRAY[1.0, 0.0], {k})"
                ))
                .rows
                .len()
        }
        "calibrated" => OperatorTree::CalibratedVectorMatch {
            query_vector: vec![1.0, 0.0],
            k,
            field: "embedding".into(),
            threshold: None,
        },
        "knn" => OperatorTree::KNN {
            query_vector: vec![1.0, 0.0],
            k,
            field: "embedding".into(),
        },
        _ => panic!("unknown vector route"),
    };
    let OperatorOutput::Posting(posting) = EngineDriver::new(&session.engine, "vectors", &[])
        .execute_node(&tree)
        .unwrap()
    else {
        panic!("expected vector postings");
    };
    posting.len()
}

fn pivot(a: &Session, b: &Session) {
    b.sql("SELECT v FROM right_t");
    a.sql("UPDATE right_t SET v = 2 WHERE id = 1");
}

#[test]
fn admitted_vector_searches_retain_empty_candidate_phantoms_for_every_index_kind() {
    for kind in ["exact", "ivf", "hnsw"] {
        for route in ["knn", "threshold", "calibrated", "sql"] {
            let (_directory, sessions) = fixtures();
            for a in sessions {
                prepare(&a, kind, false);
                let b = a.sibling();
                a.begin();
                b.begin();
                assert_eq!(search(&a, route, 2), 0, "{kind} {route}");
                pivot(&a, &b);
                b.sql("INSERT INTO vectors VALUES (2, ARRAY[1.0, 0.0], NULL)");
                assert_cycle(&a, &b);
            }
        }
    }
}

#[test]
fn admitted_vector_replacements_deletions_and_nulls_conflict_with_candidate_reads() {
    for mutation in [
        "DELETE FROM vectors WHERE id = 1",
        "UPDATE vectors SET embedding = NULL WHERE id = 1",
        "UPDATE vectors SET embedding = ARRAY[0.0, 1.0] WHERE id = 1 RETURNING id",
        "direct",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a, "exact", true);
            let b = a.sibling();
            a.begin();
            b.begin();
            assert_eq!(search(&a, "knn", 2), 1);
            pivot(&a, &b);
            if mutation == "direct" {
                b.engine
                    .add_vector("vectors", 1, "embedding", vec![0.0, 1.0])
                    .unwrap();
            } else {
                b.sql(mutation);
            }
            assert_cycle(&a, &b);
            assert_eq!(search(&a, "threshold", 2), 1);
        }
    }
}

#[test]
fn admitted_unrelated_vector_fields_null_candidates_and_unused_searches_can_commit() {
    for operation in [
        "other_field",
        "null_candidate",
        "zero_k",
        "limit_zero",
        "ordered_limit_zero",
        "aliased_limit_zero",
        "mixed_limit_zero",
    ] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a, "exact", true);
            let b = a.sibling();
            a.begin();
            b.begin();
            if operation.ends_with("limit_zero") {
                let sql = match operation {
                    "ordered_limit_zero" => "SELECT id FROM vectors WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2) ORDER BY _score DESC, id LIMIT 0",
                    "aliased_limit_zero" => "SELECT v.id FROM vectors AS v WHERE knn_match(v.embedding, ARRAY[1.0, 0.0], 2) LIMIT 0",
                    "mixed_limit_zero" => "SELECT id FROM vectors WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2) AND id IN (SELECT id FROM right_t) LIMIT 0",
                    _ => "SELECT id FROM vectors WHERE knn_match(embedding, ARRAY[1.0, 0.0], 2) LIMIT 0",
                };
                assert!(a.sql(sql).rows.is_empty());
            } else {
                let k = if operation == "zero_k" { 0 } else { 2 };
                assert_eq!(search(&a, "knn", k), usize::from(k != 0));
            }
            pivot(&a, &b);
            match operation {
                "other_field" => {
                    b.engine
                        .add_vector("vectors", 1, "other", vec![1.0, 0.0])
                        .unwrap();
                }
                "null_candidate" => {
                    b.sql("INSERT INTO vectors VALUES (2, NULL, NULL)");
                }
                _ => {
                    b.sql("INSERT INTO vectors VALUES (2, ARRAY[1.0, 0.0], NULL)");
                }
            }
            a.engine
                .commit()
                .unwrap_or_else(|error| panic!("{operation}: first commit: {error}"));
            b.engine
                .commit()
                .unwrap_or_else(|error| panic!("{operation}: second commit: {error}"));
        }
    }
}

#[test]
fn admitted_vector_savepoint_undo_removes_intents_but_preserves_observed_edges() {
    for read_before_write in [false, true] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            prepare(&a, "exact", true);
            let b = a.sibling();
            a.begin();
            b.begin();
            if read_before_write {
                assert_eq!(search(&a, "knn", 2), 1);
            }
            b.sql("SAVEPOINT vector_write");
            b.engine
                .add_vector_values("vectors", 1, "embedding", Vec::new())
                .unwrap();
            b.sql("ROLLBACK TO vector_write");
            b.sql("RELEASE vector_write");
            if !read_before_write {
                assert_eq!(search(&a, "knn", 2), 1);
            }
            pivot(&a, &b);
            finish(&a, &b, read_before_write);
            assert_eq!(search(&a, "threshold", 2), 1);
        }
    }
}

#[test]
fn admitted_dynamic_vector_fields_keep_distinct_candidate_spaces() {
    for (matching, declared) in [(false, false), (true, false), (false, true), (true, true)] {
        let (_directory, sessions) = fixtures();
        for a in sessions {
            if declared {
                a.sql("CREATE TABLE dynamic (id INTEGER PRIMARY KEY)");
            } else {
                a.engine
                    .create_default_table("dynamic", Vec::new())
                    .unwrap();
            }
            for field in ["v", "vv"] {
                a.engine.create_vector_field("dynamic", field, 2).unwrap();
            }
            let b = a.sibling();
            a.begin();
            b.begin();
            assert!(a
                .engine
                .knn_search("dynamic", "v", [1.0, 0.0], 2)
                .unwrap()
                .is_empty());
            pivot(&a, &b);
            b.engine
                .add_vector(
                    "dynamic",
                    1,
                    if matching { "v" } else { "vv" },
                    vec![1.0, 0.0],
                )
                .unwrap();
            finish(&a, &b, matching);
        }
    }
}
