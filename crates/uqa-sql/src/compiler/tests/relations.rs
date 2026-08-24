//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM items, joins, table functions, wildcards, CTEs, and relation identities.

use super::*;

#[test]
fn rows_from_preserves_members_column_definitions_aliases_and_ordinality() {
    let Statement::Select(select) = first(
        "SELECT * FROM ROWS FROM (f(1) AS (left_id int4, left_label text), g(2)) \
         WITH ORDINALITY AS grouped(a, b, c, sequence)",
    ) else {
        panic!("expected SELECT");
    };
    let Some(FromClause::FunctionGroup {
        functions,
        alias,
        column_aliases,
        ordinality,
    }) = select.from
    else {
        panic!("expected function group");
    };
    assert_eq!(alias.as_deref(), Some("grouped"));
    assert_eq!(column_aliases, ["a", "b", "c", "sequence"]);
    assert!(ordinality);
    assert_eq!(functions.len(), 2);
    assert_eq!(functions[0].name, "f");
    assert_eq!(functions[0].output_name, "f");
    assert_eq!(functions[0].column_aliases, ["left_id", "left_label"]);
    assert_eq!(functions[0].column_types, ["int4", "text"]);
    assert_eq!(functions[0].args.len(), 1);
    assert_eq!(functions[1].name, "g");
    assert!(functions[1].column_aliases.is_empty());
    assert!(functions[1].column_types.is_empty());
}

#[test]
fn table_function_column_definitions_preserve_type_modifiers() {
    let Statement::Select(select) =
        first("SELECT * FROM f() AS (label varchar(8), amount numeric(10,2), labels varchar(3)[])")
    else {
        panic!("expected SELECT");
    };
    let Some(FromClause::Function { column_types, .. }) = select.from else {
        panic!("expected function source");
    };
    assert_eq!(
        column_types,
        ["varchar(8)", "numeric(10,2)", "varchar(3)[]"]
    );
}

#[test]
fn multi_argument_from_unnest_expands_to_canonical_unary_group_members() {
    let Statement::Select(select) =
        first("SELECT * FROM unnest(ARRAY[1, 2], ARRAY['a']) AS expanded(left_value, right_value)")
    else {
        panic!("expected SELECT");
    };
    let Some(FromClause::FunctionGroup {
        functions,
        alias,
        column_aliases,
        ordinality,
    }) = select.from
    else {
        panic!("expected expanded function group");
    };
    assert_eq!(alias.as_deref(), Some("expanded"));
    assert_eq!(column_aliases, ["left_value", "right_value"]);
    assert!(!ordinality);
    assert_eq!(functions.len(), 2);
    for function in &functions {
        assert_eq!(function.name, "pg_catalog.unnest");
        assert_eq!(function.output_name, "unnest");
        assert_eq!(function.args.len(), 1);
        assert!(function.column_aliases.is_empty());
        assert!(function.column_types.is_empty());
    }
}

#[test]
fn rows_from_uses_the_same_multi_argument_unnest_expansion() {
    let Statement::Select(select) =
        first("SELECT * FROM ROWS FROM (unnest(ARRAY[1], ARRAY[2]), generate_series(1, 2))")
    else {
        panic!("expected SELECT");
    };
    let Some(FromClause::FunctionGroup { functions, .. }) = select.from else {
        panic!("expected function group");
    };
    assert_eq!(functions.len(), 3);
    assert_eq!(functions[0].name, "pg_catalog.unnest");
    assert_eq!(functions[1].name, "pg_catalog.unnest");
    assert_eq!(functions[2].name, "generate_series");
}

#[test]
fn unnest_expansion_respects_single_argument_and_qualified_boundaries() {
    let Statement::Select(single) = first("SELECT * FROM unnest(ARRAY[1, 2])") else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        single.from,
        Some(FromClause::Function { ref name, ref args, .. })
            if name == "unnest" && args.len() == 1
    ));

    let Statement::Select(rows_from_single) =
        first("SELECT * FROM ROWS FROM (unnest(ARRAY[1, 2]))")
    else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        rows_from_single.from,
        Some(FromClause::FunctionGroup { ref functions, .. })
            if matches!(functions.as_slice(), [function]
                if function.name == "unnest" && function.args.len() == 1)
    ));

    for (sql, expected_name) in [
        ("SELECT * FROM app.unnest(ARRAY[1], ARRAY[2])", "app.unnest"),
        (
            "SELECT * FROM pg_catalog.unnest(ARRAY[1], ARRAY[2])",
            "pg_catalog.unnest",
        ),
    ] {
        let Statement::Select(select) = first(sql) else {
            panic!("expected SELECT");
        };
        assert!(matches!(
            select.from,
            Some(FromClause::Function { ref name, ref args, .. })
                if name == expected_name && args.len() == 2
        ));
    }
}

#[test]
fn parenthesized_join_alias_survives_compilation() {
    let Statement::Select(select) = first(
        "SELECT j.left_id FROM ((VALUES (1)) AS l(id) JOIN (VALUES (1)) AS r(id) ON l.id = r.id) AS j(left_id, right_id)",
    ) else {
        panic!("not SELECT");
    };
    let Some(FromClause::Join {
        alias,
        column_aliases,
        ..
    }) = select.from
    else {
        panic!("not JOIN");
    };
    assert_eq!(alias.as_deref(), Some("j"));
    assert_eq!(column_aliases, ["left_id", "right_id"]);
}

#[test]
fn table_function_with_ordinality_survives_compilation() {
    let Statement::Select(select) = first(
        "SELECT * FROM pg_catalog.generate_series(1, 2) \
         WITH ORDINALITY AS g(value, sequence)",
    ) else {
        panic!("not SELECT");
    };
    let Some(FromClause::Function {
        name,
        output_name,
        alias,
        column_aliases,
        ordinality,
        ..
    }) = select.from
    else {
        panic!("not a table function");
    };
    assert_eq!(name, "pg_catalog.generate_series");
    assert_eq!(output_name, "generate_series");
    assert_eq!(alias.as_deref(), Some("g"));
    assert_eq!(column_aliases, ["value", "sequence"]);
    assert!(ordinality);
}

#[test]
fn join_using_and_natural_metadata_survive_compilation() {
    let Statement::Select(using_select) =
        first("SELECT * FROM left_table l FULL JOIN right_table r USING (id, tenant_id) AS joined")
    else {
        panic!("expected SELECT");
    };
    let Some(FromClause::Join {
        kind,
        on,
        using,
        natural,
        ..
    }) = using_select.from
    else {
        panic!("expected USING join");
    };
    assert_eq!(kind, JoinKind::Full);
    assert!(on.is_none());
    assert!(!natural);
    let using = using.expect("USING metadata");
    assert_eq!(using.columns, ["id", "tenant_id"]);
    assert_eq!(using.alias.as_deref(), Some("joined"));

    let Statement::Select(natural_select) =
        first("SELECT * FROM left_table NATURAL LEFT JOIN right_table")
    else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        natural_select.from,
        Some(FromClause::Join {
            kind: JoinKind::Left,
            on: None,
            using: None,
            natural: true,
            ..
        })
    ));
}

#[test]
fn operator_join_relation_is_compiled_as_an_identifier() {
    let Statement::Select(select) = first(
        "SELECT * FROM vector_similarity_join(\
             app.passages,\
             knn_match(embedding, ARRAY[1.0, 0.0], 6),\
             knn_match(embedding, ARRAY[0.8, 0.2], 6),\
             0.8\
         ) AS pairs",
    ) else {
        panic!("not SELECT");
    };
    let Some(FromClause::Function {
        name,
        relation,
        args,
        alias,
        ..
    }) = select.from
    else {
        panic!("not a table function");
    };
    assert_eq!(name, "vector_similarity_join");
    assert_eq!(relation.as_deref(), Some("app.passages"));
    assert_eq!(args.len(), 3);
    assert_eq!(alias.as_deref(), Some("pairs"));
}

#[test]
fn operator_join_relation_rejects_scalar_values() {
    for relation in ["'passages'", "$1", "lower('passages')"] {
        let sql = format!(
            "SELECT * FROM vector_similarity_join(\
                 {relation},\
                 knn_match(embedding, ARRAY[1.0, 0.0], 6),\
                 knn_match(embedding, ARRAY[0.8, 0.2], 6),\
                 0.8\
             )"
        );
        let error = compile(&sql).expect_err(&sql);
        assert!(
            matches!(&error, SQLError::TypeMismatch(message) if message.contains("relation must be a table identifier")),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn ordinary_table_function_keeps_scalar_identifier_arguments() {
    let Statement::Select(select) = first("SELECT * FROM unnest(items) AS value") else {
        panic!("not SELECT");
    };
    let Some(FromClause::Function { relation, args, .. }) = select.from else {
        panic!("not a table function");
    };
    assert!(relation.is_none());
    assert!(matches!(args.as_slice(), [Expr::Column(name)] if name == "items"));
}

#[test]
fn qualified_wildcard_preserves_its_structured_relation_identity() {
    let Statement::Select(select) = first("SELECT source.* FROM source") else {
        panic!("not SELECT");
    };
    assert!(matches!(
        select.projections.as_slice(),
        [Projection {
            expr: Expr::QualifiedStar(qualifier),
            alias: None,
        }] if qualifier == "source"
    ));
}

#[test]
fn unsupported_cte_control_clauses_fail_explicitly() {
    let not_materialized =
        compile("WITH c AS NOT MATERIALIZED (SELECT 1) SELECT * FROM c").unwrap_err();
    assert!(matches!(
        not_materialized,
        SQLError::Unsupported(message) if message.contains("NOT MATERIALIZED")
    ));

    let search = compile(
        "WITH RECURSIVE t(n) AS (VALUES (1) UNION ALL SELECT n + 1 FROM t WHERE n < 3) \
         SEARCH DEPTH FIRST BY n SET ordering SELECT * FROM t",
    )
    .unwrap_err();
    assert!(matches!(
        search,
        SQLError::Unsupported(message) if message.contains("SEARCH")
    ));

    let cycle = compile(
        "WITH RECURSIVE t(n) AS (VALUES (1) UNION ALL SELECT n + 1 FROM t WHERE n < 3) \
         CYCLE n SET is_cycle USING path SELECT * FROM t",
    )
    .unwrap_err();
    assert!(matches!(
        cycle,
        SQLError::Unsupported(message) if message.contains("CYCLE")
    ));
}

#[test]
fn cte_values_body_is_preserved() {
    let Statement::Select(select) =
        first("WITH rows(id, label) AS (VALUES (1, 'one'), (2, 'two')) SELECT * FROM rows")
    else {
        panic!("expected SELECT");
    };
    let cte = &select.with[0];
    assert_eq!(cte.columns, ["id", "label"]);
    assert_eq!(cte.query.values.len(), 2);
    assert!(cte.query.projections.is_empty());
}

#[test]
fn quoted_dots_preserve_range_var_component_boundaries() {
    let Statement::CreateTable(table) = first("CREATE TABLE \"a.b\".c (id INTEGER)") else {
        panic!("expected CREATE TABLE");
    };
    assert_eq!(table.name, "\"a.b\".c");

    let Statement::Select(select) = first("SELECT * FROM a.\"b.c\"") else {
        panic!("expected SELECT");
    };
    assert!(matches!(
        select.from,
        Some(FromClause::Table { name, .. }) if name == "a.\"b.c\""
    ));

    let Statement::AlterTable(alter) = first("ALTER TABLE \"a.b\".c RENAME TO \"d.e\"") else {
        panic!("expected ALTER TABLE");
    };
    assert!(matches!(
        alter.action,
        AlterTableAction::RenameTable { to } if to == "\"d.e\""
    ));

    let Statement::Drop(drop) = first("DROP TABLE \"a.b\".\"d.e\"") else {
        panic!("expected DROP TABLE");
    };
    assert_eq!(drop.names, vec!["\"a.b\".\"d.e\"".to_string()]);
}
