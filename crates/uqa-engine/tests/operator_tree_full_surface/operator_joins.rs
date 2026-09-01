//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn similarity_and_cross_paradigm_joins_execute_physically() {
    let engine = fixture();
    let driver = EngineDriver::new(&engine, "docs", &[]);

    let text_join = driver
        .execute_node(&OperatorTree::TextSimilarityJoin {
            left: Box::new(term("rust", "title")),
            right: Box::new(term("rust", "title")),
            threshold: 0.2,
        })
        .unwrap();
    assert_eq!(
        text_join
            .as_generalized()
            .expect("text join must produce tuple rows")
            .len(),
        4
    );

    let vector_join = driver
        .execute_node(&OperatorTree::VectorSimilarityJoin {
            left: Box::new(vector([1.0, 0.0], -1.0)),
            right: Box::new(vector([1.0, 0.0], -1.0)),
            threshold: 0.8,
        })
        .unwrap();
    assert!(
        vector_join
            .as_generalized()
            .expect("vector join must produce tuple rows")
            .len()
            >= 4
    );

    let hybrid_operand = || {
        OperatorTree::Intersect(vec![
            OperatorTree::Filter {
                field: "category".into(),
                predicate: Predicate::Equals(Value::Str("A".into())),
                source: None,
            },
            vector([1.0, 0.0], -1.0),
        ])
    };
    let hybrid_join = driver
        .execute_node(&OperatorTree::HybridJoin {
            left: Box::new(hybrid_operand()),
            right: Box::new(hybrid_operand()),
        })
        .unwrap();
    assert_eq!(
        hybrid_join
            .as_generalized()
            .expect("hybrid join must produce tuple rows")
            .len(),
        4
    );

    let cross_join = driver
        .execute_node(&OperatorTree::CrossParadigmJoin {
            left: Box::new(page_rank()),
            right: Box::new(OperatorTree::Filter {
                field: "category".into(),
                predicate: Predicate::IsNotNull,
                source: None,
            }),
        })
        .unwrap();
    assert_eq!(
        cross_join
            .as_generalized()
            .expect("cross-paradigm join must produce tuple rows")
            .len(),
        5
    );
}

fn assert_operator_join_result(engine: &Engine, name: &str, sql: &str, expected_rows: usize) {
    let result = engine.sql(sql, &[]).unwrap_or_else(|error| {
        panic!("{name} SQL lowering failed: {error}");
    });
    assert_eq!(result.rows.len(), expected_rows, "{name}");
    assert!(result.rows.iter().all(|row| {
        matches!(row.get("left_doc_id"), Some(Value::Int(_)))
            && matches!(row.get("right_doc_id"), Some(Value::Int(_)))
    }));
}

fn explain_plan(engine: &Engine, sql: &str) -> String {
    engine
        .sql(&format!("EXPLAIN {sql}"), &[])
        .unwrap()
        .rows
        .iter()
        .filter_map(|row| match row.get("plan") {
            Some(Value::Str(line)) => Some(line.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn operator_join_table_functions_lower_and_execute_from_sql() {
    let engine = fixture();
    let cases = [
        (
            "text_similarity_join",
            "SELECT left_doc_id, right_doc_id \
             FROM text_similarity_join(\
                 docs,\
                 text_match(title, 'rust'),\
                 docs,\
                 text_match(title, 'rust'),\
                 0.2\
             )",
            4,
        ),
        (
            "vector_similarity_join",
            "SELECT left_doc_id, right_doc_id \
             FROM vector_similarity_join(\
                 docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 0.8\
             )",
            5,
        ),
        (
            "graph_join",
            "SELECT left_doc_id, right_doc_id \
             FROM graph_join(\
                 docs,\
                 graph_pagerank('social'),\
                 docs,\
                 graph_pagerank('social'),\
                 'follows',\
                 'social'\
             )",
            2,
        ),
        (
            "hybrid_join",
            "SELECT left_doc_id, right_doc_id \
             FROM hybrid_join(\
                 docs,\
                 category = 'A' AND knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 docs,\
                 category = 'A' AND knn_match(embedding, ARRAY[1.0, 0.0], 3)\
             )",
            4,
        ),
        (
            "cross_paradigm_join",
            "SELECT left_doc_id, right_doc_id \
             FROM cross_paradigm_join(\
                 docs,\
                 graph_pagerank('social'),\
                 docs,\
                 category IS NOT NULL\
             )",
            5,
        ),
    ];

    for (name, sql, expected_rows) in cases {
        assert_operator_join_result(&engine, name, sql, expected_rows);
    }
}

#[test]
fn operator_join_table_functions_validate_thresholds() {
    let engine = fixture();
    let invalid_threshold = engine
        .sql(
            "SELECT left_doc_id \
             FROM vector_similarity_join(\
                 docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                 2.0\
             )",
            &[],
        )
        .expect_err("out-of-range SQL operator threshold must be rejected");
    assert!(invalid_threshold
        .to_string()
        .contains("must be finite and in [-1, 1]"));
}

#[test]
fn operator_join_relation_uses_catalog_name_resolution() {
    let engine = Engine::new();
    engine.sql("CREATE SCHEMA search_scope", &[]).unwrap();
    engine
        .sql(
            "CREATE TABLE search_scope.scoped_docs (\
                 id INTEGER PRIMARY KEY, embedding VECTOR(2)\
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO search_scope.scoped_docs (id, embedding) \
             VALUES (1, ARRAY[1.0, 0.0])",
            &[],
        )
        .unwrap();

    let qualified = engine
        .sql(
            "SELECT left_doc_id, right_doc_id \
             FROM vector_similarity_join(\
                 search_scope.scoped_docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 1),\
                 search_scope.scoped_docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 1),\
                 0.8\
             )",
            &[],
        )
        .unwrap();
    assert_eq!(qualified.rows.len(), 1);

    engine
        .sql("SET search_path TO search_scope, public", &[])
        .unwrap();
    let unqualified = engine
        .sql(
            "SELECT left_doc_id, right_doc_id \
             FROM vector_similarity_join(\
                 scoped_docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 1),\
                 scoped_docs,\
                 knn_match(embedding, ARRAY[1.0, 0.0], 1),\
                 0.8\
             )",
            &[],
        )
        .unwrap();
    assert_eq!(unqualified.rows, qualified.rows);
}

#[test]
fn operator_join_result_can_be_nested_relationally() {
    let engine = fixture();
    let result = engine
        .sql(
            "WITH pairs AS (\
                 SELECT left_doc_id, right_doc_id, _score \
                 FROM vector_similarity_join(\
                     docs,\
                     knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                     docs,\
                     knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                     0.8\
                 )\
             ) \
             SELECT pairs.left_doc_id, left_doc.title, right_doc.title AS right_title \
             FROM pairs \
             JOIN docs AS left_doc ON left_doc.id = pairs.left_doc_id \
             JOIN docs AS right_doc ON right_doc.id = pairs.right_doc_id",
            &[],
        )
        .unwrap();

    assert_eq!(result.rows.len(), 5);
}

#[test]
fn operator_join_sources_participate_in_two_way_dpccp() {
    let engine = fixture();
    let joined_sql = "SELECT pairs.left_doc_id, d.id \
                      FROM vector_similarity_join(\
                          docs,\
                          knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                          docs,\
                          knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                          0.8\
                      ) AS pairs \
                      JOIN docs AS d ON d.id = pairs.left_doc_id";
    let plan = explain_plan(&engine, joined_sql);
    assert!(
        plan.contains("strategy: Hash"),
        "operator join source must participate in DPccp: {plan}"
    );
    let joined = engine.sql(joined_sql, &[]).unwrap();
    assert_eq!(joined.rows.len(), 5);

    let reverse_joined_sql = "SELECT d.id, pairs.right_doc_id \
                              FROM docs AS d \
                              JOIN vector_similarity_join(\
                                  docs,\
                                  knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                                  docs,\
                                  knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                                  0.8\
                              ) AS pairs ON d.id = pairs.left_doc_id";
    let reverse_plan = explain_plan(&engine, reverse_joined_sql);
    assert!(
        reverse_plan.contains("strategy: Hash"),
        "right-side operator join source must participate in DPccp: {reverse_plan}"
    );
    let reverse_joined = engine.sql(reverse_joined_sql, &[]).unwrap();
    assert_eq!(reverse_joined.rows.len(), 5);
}

#[test]
fn operator_join_sources_participate_in_three_way_dpccp() {
    let engine = fixture();
    engine
        .sql(
            "CREATE TABLE join_groups (id INTEGER PRIMARY KEY, category TEXT)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO join_groups (id, category) VALUES \
             (1, 'A'), (2, 'B'), (3, 'A'), (4, 'B'), (5, 'A'), \
             (6, 'B'), (7, 'A'), (8, 'B'), (9, 'A'), (10, 'B')",
            &[],
        )
        .unwrap();
    engine.sql("ANALYZE docs", &[]).unwrap();
    engine.sql("ANALYZE join_groups", &[]).unwrap();
    let three_way_sql = "SELECT pairs.left_doc_id, d.id, g.id AS group_id \
                         FROM vector_similarity_join(\
                             docs,\
                             knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                             docs,\
                             knn_match(embedding, ARRAY[1.0, 0.0], 3),\
                             0.8\
                         ) AS pairs \
                         JOIN docs AS d ON d.id = pairs.left_doc_id \
                         JOIN join_groups AS g ON g.category = d.category";
    let three_way_plan = explain_plan(&engine, three_way_sql);
    let pairs_position = three_way_plan
        .find("vector_similarity_join")
        .expect("operator source in three-way plan");
    let docs_position = three_way_plan
        .find("name: \"docs\"")
        .expect("docs source in three-way plan");
    let groups_position = three_way_plan
        .find("name: \"join_groups\"")
        .expect("group source in three-way plan");
    assert!(
        pairs_position < groups_position && docs_position < groups_position,
        "costed operator source and docs must form the first join: {three_way_plan}"
    );
    assert_eq!(three_way_plan.matches("strategy: Hash").count(), 2);
    let three_way = engine.sql(three_way_sql, &[]).unwrap();
    assert_eq!(three_way.rows.len(), 25);
}
