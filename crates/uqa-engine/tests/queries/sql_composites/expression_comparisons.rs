//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Composite expression consumers preserve their selected SQL comparison.

#[rstest::rstest]
#[case::memory(0)]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn composite_expression_comparisons_match_postgresql(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("composite-expressions.db");
    let engine = super::addition::open(provider, &path);
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_expression_comparisons_oracle.expected.json"
    ))
    .unwrap();
    let mut initial = reference.clone();
    initial["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] != true);
    crate::pg18_oracle::verify(&engine, &initial.to_string());
    reconstruct_precedence_views(&engine);
    verify_reconstructed_predicates(&engine, &reference);
    let engine = if provider == 0 {
        engine
    } else {
        drop(engine);
        super::addition::open(provider, &path)
    };
    let mut durable = reference.clone();
    durable["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["reopen"] == true);
    crate::pg18_oracle::verify(&engine, &durable.to_string());
    verify_reconstructed_predicates(&engine, &reference);
}

fn reconstruct_precedence_views(engine: &uqa_engine::Engine) {
    for pretty in [false, true] {
        let result = engine
            .sql(
                &format!("SELECT pg_get_viewdef('ce_precedence'::regclass,{pretty}) AS definition"),
                &[],
            )
            .unwrap();
        let uqa_core::Value::Str(definition) = &result.rows[0]["definition"] else {
            panic!("view definition");
        };
        engine
            .sql(
                &format!("CREATE VIEW ce_reconstructed_{pretty} AS {definition}"),
                &[],
            )
            .unwrap();
    }
}

fn verify_reconstructed_predicates(engine: &uqa_engine::Engine, reference: &serde_json::Value) {
    let expected = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "precedence_values")
        .unwrap();
    for pretty in [false, true] {
        let mut expected = expected.clone();
        expected["sql"] = format!("SELECT * FROM ce_reconstructed_{pretty} ORDER BY id").into();
        crate::pg18_oracle::verify(
            engine,
            &serde_json::json!({
                "postgresql_version": reference["postgresql_version"],
                "cases": [expected],
            })
            .to_string(),
        );
    }
}

#[rstest::rstest]
#[case::sqlite(1)]
#[case::sqlite_key_value(2)]
#[case::redb(3)]
fn retained_constructor_comparison_survives_type_changes_after_reopen(#[case] provider: usize) {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("retained-comparison.db");
    let reference: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../tests/parity/pg18/composite_expression_comparisons_oracle.expected.json"
    ))
    .unwrap();
    let engine = super::addition::open(provider, &path);
    let setup = reference["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["id"] == "distinct_stored_definition")
        .unwrap();
    engine.sql(setup["sql"].as_str().unwrap(), &[]).unwrap();
    drop(engine);
    // Restore the predecessor's complete constructor comparison in this disposable catalog. Public type and view identities remain unchanged.
    super::addition::restoration::catalog(provider, &path, |catalog| {
        let mut views = catalog.load_views().unwrap();
        assert_eq!(views.len(), 1);
        let view = &mut views[0];
        let mut definition: uqa_sql::catalog::stored_view::StoredViewDefinition =
            serde_json::from_str(&view.definition_json).unwrap();
        let uqa_sql::Statement::Select(query) = uqa_sql::compile(
            "SELECT ROW(1,'a')::ce_stored_pair IS DISTINCT FROM ROW(2,'b')::ce_stored_pair AS result"
        ).unwrap().remove(0) else { panic!("SELECT"); };
        definition.query = uqa_sql::plan::QueryPlan::lower(*query);
        view.definition_json = serde_json::to_string(&definition).unwrap();
        catalog.save_view(view).unwrap();
    });
    let engine = super::addition::open(provider, &path);
    let mut changed = reference.clone();
    changed["cases"].as_array_mut().unwrap().retain(|case| {
        matches!(
            case["id"].as_str(),
            Some("distinct_stored_change" | "distinct_stored_drop")
        )
    });
    crate::pg18_oracle::verify(&engine, &changed.to_string());
    drop(engine);
    let engine = super::addition::open(provider, &path);
    let mut reopened = reference;
    reopened["cases"]
        .as_array_mut()
        .unwrap()
        .retain(|case| case["id"] == "reopen_distinct");
    crate::pg18_oracle::verify(&engine, &reopened.to_string());
}
