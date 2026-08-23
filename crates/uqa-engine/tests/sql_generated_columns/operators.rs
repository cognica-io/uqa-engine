//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Virtual generated columns across non-SQL operator surfaces.

use super::*;

fn generated_operator_fixture() -> Engine {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_operator_rows (
                 id INTEGER PRIMARY KEY,
                 source INTEGER,
                 virtual_group INTEGER GENERATED ALWAYS AS (CASE WHEN source = 2 THEN 0 ELSE 1 END),
                 virtual_value INTEGER GENERATED ALWAYS AS (source * 10),
                 embedding VECTOR(2)
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_operator_rows (id, source, embedding) VALUES
             (1, 1, ARRAY[1.0, 0.0]),
             (2, 2, ARRAY[0.0, 1.0]),
             (3, 3, ARRAY[0.9, 0.1])",
            &[],
        )
        .unwrap();
    engine
}

fn all_generated_operator_vectors() -> OperatorTree {
    OperatorTree::VectorSimilarity {
        query_vector: vec![1.0, 0.0],
        threshold: -1.0,
        field: "embedding".into(),
    }
}

#[test]
fn operator_filter_and_facet_project_virtual_generated_columns() {
    let engine = generated_operator_fixture();
    let driver = EngineDriver::new(&engine, "generated_operator_rows", &[]);

    let OperatorOutput::Posting(filtered) = driver
        .execute_node(&OperatorTree::Filter {
            field: "virtual_value".into(),
            predicate: Predicate::Equals(Value::Int(20)),
            source: None,
        })
        .unwrap()
    else {
        panic!("generated filter must return a posting list");
    };
    assert_eq!(filtered.doc_ids().collect::<Vec<_>>(), vec![2]);

    let OperatorOutput::Posting(faceted) = driver
        .execute_node(&OperatorTree::Facet {
            field: "virtual_group".into(),
            source: None,
        })
        .unwrap()
    else {
        panic!("generated facet must return a posting list");
    };
    let facet_counts = faceted
        .entries()
        .iter()
        .map(|entry| {
            let Value::Str(value) = &entry.payload.fields["_facet_value"] else {
                panic!("facet value must be text");
            };
            let Value::Int(count) = entry.payload.fields["_facet_count"] else {
                panic!("facet count must be an integer");
            };
            (value.clone(), count)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        facet_counts,
        BTreeMap::from([("0".into(), 1), ("1".into(), 2)])
    );
}

#[test]
fn operator_aggregate_and_group_by_project_virtual_generated_columns() {
    let engine = generated_operator_fixture();
    let driver = EngineDriver::new(&engine, "generated_operator_rows", &[]);
    let OperatorOutput::Posting(aggregate) = driver
        .execute_node(&OperatorTree::Aggregate {
            source: None,
            field: "virtual_value".into(),
            monoid: Arc::new(SumMonoid),
        })
        .unwrap()
    else {
        panic!("generated aggregate must return a posting list");
    };
    assert_eq!(
        aggregate.entries()[0].payload.fields.get("_aggregate"),
        Some(&Value::Float(60.0))
    );

    let all_rows = || OperatorTree::Filter {
        field: "id".into(),
        predicate: Predicate::IsNotNull,
        source: None,
    };
    let OperatorOutput::Posting(grouped) = driver
        .execute_node(&OperatorTree::GroupBy {
            source: Box::new(all_rows()),
            group_field: "virtual_group".into(),
            agg_field: "virtual_value".into(),
            monoid: Arc::new(SumMonoid),
        })
        .unwrap()
    else {
        panic!("generated group-by must return a posting list");
    };
    let grouped_values = grouped
        .entries()
        .iter()
        .map(|entry| {
            let Value::Str(key) = &entry.payload.fields["_group_key"] else {
                panic!("group key must be text");
            };
            let Value::Float(value) = entry.payload.fields["_aggregate_result"] else {
                panic!("group aggregate must be numeric");
            };
            (key.clone(), value)
        })
        .collect::<BTreeMap<_, _>>();
    assert_eq!(
        grouped_values,
        BTreeMap::from([("0".into(), 20.0), ("1".into(), 40.0)])
    );
}

#[test]
fn operator_vector_facet_and_join_project_virtual_generated_columns() {
    let engine = generated_operator_fixture();
    let driver = EngineDriver::new(&engine, "generated_operator_rows", &[]);
    let OperatorOutput::Posting(vector_facets) = driver
        .execute_node(&OperatorTree::FacetVector {
            vector_op: Box::new(all_generated_operator_vectors()),
            facet_field: "virtual_group".into(),
        })
        .unwrap()
    else {
        panic!("generated vector facet must return a posting list");
    };
    assert_eq!(vector_facets.len(), 2);

    let hybrid_operand = || {
        OperatorTree::Intersect(vec![
            OperatorTree::Filter {
                field: "virtual_group".into(),
                predicate: Predicate::Equals(Value::Int(1)),
                source: None,
            },
            all_generated_operator_vectors(),
        ])
    };
    let OperatorOutput::Generalized(joined) = driver
        .execute_node(&OperatorTree::HybridJoin {
            left: Box::new(hybrid_operand()),
            right: Box::new(hybrid_operand()),
        })
        .unwrap()
    else {
        panic!("generated hybrid join must return generalized rows");
    };
    assert_eq!(joined.len(), 4);
}

#[test]
fn deep_learning_table_reads_virtual_generated_labels() {
    let engine = Engine::new();
    engine
        .sql(
            "CREATE TABLE generated_training_rows (
                 id INTEGER PRIMARY KEY,
                 features REAL[],
                 source INTEGER,
                 label INTEGER GENERATED ALWAYS AS (source - 1)
             )",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "INSERT INTO generated_training_rows (id, features, source) VALUES
             (1, ARRAY[2.0, 0.0], 1),
             (2, ARRAY[3.0, 0.0], 1),
             (3, ARRAY[0.0, 2.0], 2),
             (4, ARRAY[0.0, 3.0], 2)",
            &[],
        )
        .unwrap();
    engine
        .sql(
            "SELECT deep_learn('generated-label-model', 'generated_training_rows')",
            &[],
        )
        .unwrap();
    assert!(engine
        .load_model("generated-label-model")
        .unwrap()
        .is_some());
}
