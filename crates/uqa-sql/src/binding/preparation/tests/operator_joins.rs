//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

fn context() -> BindingContext<'static> {
    let tables = [
        ("papers", "venue integer, body text, embedding vector(3)"),
        ("archive", "venue text, body text, embedding vector(3)"),
    ]
    .into_iter()
    .map(|(name, columns)| {
        let crate::Statement::CreateTable(table) =
            crate::compile(&format!("CREATE TABLE {name} ({columns})"))
                .unwrap()
                .remove(0)
        else {
            unreachable!()
        };
        (
            RelationIdentity::new("public", name),
            crate::binding::fixture::table_definition(table.columns),
        )
    })
    .collect();
    BindingContext {
        catalog: crate::binding::fixture::catalog(tables),
        ..assignment_context()
    }
}

#[test]
fn operator_join_preparation_uses_each_relation_for_predicate_types() {
    for (name, options) in [
        ("text_similarity_join", ", 0.5"),
        ("vector_similarity_join", ", 0.5"),
        ("graph_join", ", 'cites', 'papers_graph'"),
        ("hybrid_join", ""),
        ("cross_paradigm_join", ""),
    ] {
        for grouped in [false, true] {
            let call = format!("{name}(papers, venue = $1, archive, archive.venue = $2{options})");
            let source = if grouped {
                format!("ROWS FROM ({call})")
            } else {
                call
            };
            let sql = format!("SELECT * FROM {source}");
            let plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
            let types =
                infer_prepared_parameter_types(&NoRoutines, &plan, &[None, None], &context())
                    .unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(
                types,
                vec![Some(ColumnType::Integer), Some(ColumnType::Text)],
                "{sql}"
            );
        }
    }
}

#[test]
fn operator_join_preparation_preserves_retrieval_field_arguments() {
    let sql = "SELECT pairs.left_doc_id FROM hybrid_join(\
        papers, venue IS NOT NULL AND knn_match(embedding, ARRAY[1.0,0.0,0.0], 6), \
        archive, venue IS NOT NULL AND text_match(body, 'rust')) AS pairs";
    let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    read_prepared_inputs(&NoRoutines, &mut plan, &[], &context(), None).unwrap();
}

#[test]
fn operator_join_preparation_keeps_relation_and_constant_namespaces_separate() {
    for (sql, state) in [
        ("SELECT * FROM hybrid_join(papers, archive.venue IS NOT NULL, archive, true)", "42P01"),
        ("SELECT * FROM hybrid_join(papers, true, archive, papers.venue IS NOT NULL)", "42P01"),
        ("SELECT * FROM papers p, hybrid_join(papers, p.venue IS NOT NULL, archive, true)", "42P01"),
        ("SELECT * FROM text_similarity_join(papers, true, archive, true, venue)", "42703"),
        ("SELECT * FROM ROWS FROM (hybrid_join(papers, true, archive, papers.venue IS NOT NULL))", "42P01"),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error =
            infer_prepared_parameter_types(&NoRoutines, &plan, &[], &context()).expect_err(sql);
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
}

#[test]
fn operator_join_preparation_reads_input_constants_in_operand_order() {
    for source in [
        "hybrid_join(papers, venue = 'bad', archive, missing IS NULL)",
        "ROWS FROM (hybrid_join(papers, venue = 'bad', archive, missing IS NULL))",
    ] {
        let sql = format!("SELECT * FROM {source}");
        let mut plan = UnifiedPlan::lower(crate::compile(&sql).unwrap().remove(0));
        let error =
            read_prepared_inputs(&NoRoutines, &mut plan, &[], &context(), None).unwrap_err();
        assert_eq!(error.sqlstate(), Some("22P02"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            "invalid input syntax for type integer: \"bad\""
        );
    }
}
