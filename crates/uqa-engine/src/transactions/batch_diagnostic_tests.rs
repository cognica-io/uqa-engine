//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::Engine;

#[test]
fn diagnostics_distinguish_vector_dimensions_and_both_text_index_boundaries() {
    let engine = Engine::new();
    engine.sql("CREATE TABLE diagnostic_items (id INTEGER PRIMARY KEY, body TEXT, embedding VECTOR(3))", &[]).unwrap();
    let vector = engine
        .sql(
            "INSERT INTO diagnostic_items VALUES (1, 'private diagnostic value', $1)",
            &[crate::SQLParam::vector(vec![1.0, 0.0])],
        )
        .unwrap_err();
    assert_eq!(vector.sqlstate(), Some("22023"));
    assert!(matches!(
        vector,
        uqa_sql::SQLError::VectorDimMismatch {
            expected: 3,
            actual: 2
        }
    ));
    let sql_index = engine
        .sql(
            "SELECT id FROM diagnostic_items WHERE text_match(body, 'private diagnostic value')",
            &[],
        )
        .unwrap_err();
    let direct_index = uqa_sql::semantics::text_indexes::require_physical_text_index(
        "diagnostic_items",
        "body",
        &[],
        Vec::new,
    )
    .unwrap_err();
    for error in [sql_index, direct_index] {
        assert_eq!(error.sqlstate(), Some("42804"));
        assert!(matches!(error, uqa_sql::SQLError::TextIndexRequired { .. }));
        assert!(error.to_string().contains("has no text index"));
    }
}

#[test]
fn diagnosed_batch_retains_second_member_and_rolls_back_all_writes() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE diagnostic_items (id INTEGER PRIMARY KEY)",
            &[],
        )
        .unwrap();
    let error = engine
        .sql_batch_diagnosed(&[
            ("INSERT INTO diagnostic_items VALUES (1)", &[]),
            ("SELECT absent_column FROM diagnostic_items", &[]),
            ("INSERT INTO diagnostic_items VALUES (3)", &[]),
        ])
        .unwrap_err();
    assert_eq!(error.statement_index, Some(1));
    assert_eq!(error.error.sqlstate(), Some("42703"));
    assert!(engine
        .sql("SELECT * FROM diagnostic_items", &[])
        .unwrap()
        .rows
        .is_empty());
    engine
        .sql("INSERT INTO diagnostic_items VALUES (4)", &[])
        .unwrap();
    assert_eq!(
        engine
            .sql("SELECT * FROM diagnostic_items", &[])
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn diagnosed_batch_retains_parser_position_and_success_semantics() {
    let engine = Engine::new();
    let error = engine
        .sql_batch_diagnosed(&[("SELECT 1", &[]), ("SELECT )", &[])])
        .unwrap_err();
    assert_eq!(error.statement_index, Some(1));
    assert_eq!(error.error.sqlstate(), Some("42601"));
    assert_eq!(error.error.position(), Some(8));
    let diagnosed = engine
        .sql_batch_diagnosed(&[("SELECT 1 AS n", &[]), ("SELECT 2 AS n", &[])])
        .unwrap();
    let original = engine
        .sql_batch(&[("SELECT 1 AS n", &[]), ("SELECT 2 AS n", &[])])
        .unwrap();
    assert_eq!(diagnosed.len(), original.len());
    for (diagnosed, original) in diagnosed.iter().zip(&original) {
        assert_eq!(diagnosed.rows, original.rows);
        assert_eq!(diagnosed.columns, original.columns);
    }
}
