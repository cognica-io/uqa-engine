//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn each_owner(run: impl Fn(&Engine)) {
    run(&Engine::new());
    for provider in 0..3 {
        let (_directory, engine, peer) = sessions(provider);
        run(&engine);
        let factory = Arc::clone(engine.storage.provider.as_ref().unwrap());
        drop((engine, peer));
        let reopened = Engine::from_persistent_provider(factory).unwrap();
        assert_eq!(kind(&reopened), "diskann");
        assert_search(&reopened, &[(1, 1.0)]);
    }
}

fn create(engine: &Engine, populated: bool) {
    sql(engine, "CREATE TABLE diskann_docs(id int, embedding vector(2)); CREATE INDEX diskann_idx ON diskann_docs USING diskann(embedding)");
    if populated {
        sql(engine, "INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0])");
    }
}

#[test]
fn diskann_sql_same_type_rebuild_preserves_scores_catalog_and_undo() {
    each_owner(|engine| {
        create(engine, true);
        let original = super::super::definition(engine, "diskann_idx");
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(2); UPDATE diskann_docs SET embedding=ARRAY[0.0,1.0]");
        assert_search(engine, &[(1, 0.0)]);
        sql(engine, "ROLLBACK TO kept; COMMIT; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(2)");
        assert_eq!(kind(engine), "diskann");
        assert_eq!(super::super::definition(engine, "diskann_idx"), original);
        assert_search(engine, &[(1, 1.0)]);
    });
}

#[test]
fn diskann_sql_empty_dimension_change_and_undo_preserve_effective_parameters() {
    each_owner(|engine| {
        create(engine, false);
        let original = super::super::definition(engine, "diskann_idx");
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE tensor(3); INSERT INTO diskann_docs VALUES(1,ARRAY[ARRAY[1.0,0.0,0.0]])");
        let found = sql(
            engine,
            "SELECT _score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0,0.0],1)",
        );
        assert_eq!(found.rows[0]["_score"], Value::Float(1.0));
        assert_eq!(super::super::definition(engine, "diskann_idx"), original);
        sql(
            engine,
            "ROLLBACK TO kept; COMMIT; INSERT INTO diskann_docs VALUES(1,ARRAY[1.0,0.0])",
        );
        assert_eq!(kind(engine), "diskann");
        assert_search(engine, &[(1, 1.0)]);
    });
}

#[test]
fn diskann_sql_rewritten_dimensions_preserve_index_identity_and_rollback() {
    each_owner(|engine| {
        create(engine, true);
        let original = super::super::definition(engine, "diskann_idx");
        let table = engine.try_table("diskann_docs").unwrap().unwrap();
        let retained = table
            .vector_indexes
            .read()
            .get("embedding")
            .unwrap()
            .snapshot()
            .unwrap();
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(3) USING ARRAY[1.0,0.0,0.0]");
        let found = sql(
            engine,
            "SELECT _score FROM diskann_docs WHERE knn_match(embedding,ARRAY[1.0,0.0,0.0],1)",
        );
        assert_eq!(found.rows[0]["_score"], Value::Float(1.0));
        assert_eq!(super::super::definition(engine, "diskann_idx"), original);
        assert_eq!(
            retained.search_knn(&[1.0, 0.0], 1).unwrap().entries()[0]
                .payload
                .score,
            1.0
        );
        sql(engine, "ROLLBACK TO kept; COMMIT");
        sql(engine, "ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(3) USING ARRAY[1.0,0.0,0.0]; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE vector(2) USING ARRAY[1.0,0.0]");
        assert_eq!(kind(engine), "diskann");
        assert_search(engine, &[(1, 1.0)]);
    });
}

#[test]
fn diskann_sql_failed_type_rewrite_restores_the_original_catalog_and_vectors() {
    each_owner(|engine| {
        create(engine, true);
        let original = super::super::definition(engine, "diskann_idx");
        sql(engine, "CREATE TABLE outer_work(v int); INSERT INTO outer_work VALUES(7); BEGIN; UPDATE outer_work SET v=9; SAVEPOINT kept");
        for target in ["vector(3)", "integer", "vector(1) USING ARRAY[1.0]"] {
            engine
                .sql(
                    &format!("ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE {target}"),
                    &[],
                )
                .unwrap_err();
            sql(engine, "ROLLBACK TO kept");
            assert_eq!(super::super::definition(engine, "diskann_idx"), original);
            assert_eq!(kind(engine), "diskann");
            assert_search(engine, &[(1, 1.0)]);
        }
        sql(engine, "COMMIT");
        assert_eq!(
            sql(engine, "SELECT v FROM outer_work").rows[0]["v"],
            Value::Int(9)
        );
    });
}

#[test]
fn diskann_sql_type_rewrite_preserves_source_expression_types() {
    each_owner(|engine| {
        sql(engine, "CREATE TABLE type_rewrite_reference(id integer,v text); INSERT INTO type_rewrite_reference VALUES(1,'12'); ALTER TABLE type_rewrite_reference ALTER COLUMN v TYPE integer USING length(v)");
        assert_eq!(
            sql(engine, "SELECT v FROM type_rewrite_reference").rows[0]["v"],
            Value::Int(2)
        );
        sql(
            engine,
            "ALTER TABLE type_rewrite_reference ALTER COLUMN v TYPE text USING (v+1)::text",
        );
        assert_eq!(
            sql(engine, "SELECT v FROM type_rewrite_reference").rows[0]["v"],
            Value::Str("3".into())
        );
        create(engine, true);
    });
}

#[test]
fn diskann_sql_scalar_conversion_retires_the_generation_and_supports_undo() {
    each_owner(|engine| {
        create(engine, true);
        sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE diskann_docs ALTER COLUMN embedding TYPE text USING 'converted'");
        assert!(!engine.has_catalog_index("diskann_idx").unwrap());
        assert_eq!(
            sql(engine, "SELECT embedding FROM diskann_docs").rows[0]["embedding"],
            Value::Str("converted".into())
        );
        sql(engine, "ROLLBACK TO kept; COMMIT");
        assert_eq!(kind(engine), "diskann");
        assert_search(engine, &[(1, 1.0)]);
    });
}

#[test]
fn diskann_sql_type_rewrite_keeps_existing_vector_methods_working() {
    each_owner(|engine| {
        for method in ["exact", "hnsw", "ivf"] {
            sql(engine, "CREATE TABLE existing_vectors(id int,embedding vector(2)); INSERT INTO existing_vectors VALUES(1,ARRAY[1.0,0.0])");
            if method != "exact" {
                sql(
                    engine,
                    &format!(
                        "CREATE INDEX existing_index ON existing_vectors USING {method}(embedding)"
                    ),
                );
            }
            sql(engine, "ALTER TABLE existing_vectors ALTER COLUMN embedding TYPE vector(3) USING ARRAY[1.0,0.0,0.0]");
            let found = sql(engine, "SELECT _score FROM existing_vectors WHERE knn_match(embedding,ARRAY[1.0,0.0,0.0],1)");
            assert_eq!(found.rows[0]["_score"], Value::Float(1.0), "{method}");
            sql(engine, "BEGIN; SAVEPOINT kept; ALTER TABLE existing_vectors ALTER COLUMN embedding TYPE text USING 'converted'");
            assert!(!engine.has_catalog_index("existing_index").unwrap());
            sql(engine, "ROLLBACK TO kept; COMMIT");
            let found = sql(engine, "SELECT _score FROM existing_vectors WHERE knn_match(embedding,ARRAY[1.0,0.0,0.0],1)");
            assert_eq!(found.rows[0]["_score"], Value::Float(1.0), "{method}");
            sql(engine, "DROP TABLE existing_vectors");
        }
        create(engine, true);
    });
}
