//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn populations(engine: &Engine) -> (Option<u64>, Option<u64>) {
    let tree = uqa_operators::OperatorTree::KNN {
        field: "embedding".into(),
        query_vector: vec![1.0, 0.0],
        k: 1,
    };
    let optimizer =
        uqa_planner::retrieval_planning::query_optimizer(engine, "diskann_docs", &tree).unwrap();
    let populations = optimizer
        .index_stats
        .diskann_query("embedding", &[1.0, 0.0])
        .unwrap()
        .index
        .populations;
    (populations.current_vectors, populations.changed_vectors)
}

#[test]
fn diskann_definition_population_reaches_planning_on_every_provider() {
    for method in [None, Some("hnsw"), Some("ivf")] {
        owners(|engine| {
            sql(engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0]),(2,ARRAY[0.0,1.0])");
            if let Some(method) = method {
                sql(
                    engine,
                    &format!(
                        "CREATE INDEX preceding_idx ON diskann_docs USING {method}(embedding)"
                    ),
                );
            }
            sql(
                engine,
                "BEGIN ISOLATION LEVEL REPEATABLE READ; SELECT * FROM diskann_docs",
            );
            if method.is_some() {
                sql(engine, "DROP INDEX preceding_idx");
            }
            sql(
                engine,
                "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
            );
            let reader = copied(engine);
            sql(engine, "ROLLBACK");
            assert!(
                matches!(populations(&reader), (None, None) | (Some(2), Some(0))),
                "an unobserved selection must retain either unknown or independently maintained counts: {method:?}"
            );
            assert_search(&reader, &[(1, 1.0), (2, 0.0)]);
            assert_eq!(populations(&reader), (Some(2), Some(0)), "{method:?}");
            let nested = copied(&reader);
            drop(reader);
            assert_eq!(populations(&nested), (Some(2), Some(0)), "{method:?}");
            assert_search(&nested, &[(1, 1.0), (2, 0.0)]);
        });
    }
}

#[test]
fn diskann_raw_definition_population_uses_fixed_rows_after_concurrent_writes() {
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        create(&engine, false);
        sql(&peer, "UPDATE diskann_docs SET embedding=ARRAY[-1.0,0.0] WHERE id=1; DELETE FROM diskann_docs WHERE id=2; INSERT INTO diskann_docs VALUES(3,ARRAY[1.0,0.0])");
        sql(
            &engine,
            "CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)",
        );
        let reader = copied(&engine);
        sql(&engine, "ROLLBACK");
        assert_eq!(populations(&reader), (None, None));
        assert_search(&reader, &[(1, 1.0), (2, 0.0)]);
        assert_eq!(populations(&reader), (Some(2), Some(2)));
        sql(&peer, "TRUNCATE diskann_docs");
        assert_eq!(populations(&reader), (Some(2), Some(2)));
        assert_search(&reader, &[(1, 1.0), (2, 0.0)]);
    }
}
