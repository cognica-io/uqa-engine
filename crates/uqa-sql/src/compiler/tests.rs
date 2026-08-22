//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{ColumnType, FromClause, JoinKind, Projection, TableKeyConstraintKind};

#[test]
fn bundled_parser_is_postgresql_18_4() {
    let parsed = pg_query::parse("SELECT 1").expect("parser accepts a scalar query");
    assert_eq!(parsed.protobuf.version, 180_004);
}

#[test]
fn returning_row_aliases_preserve_quoted_identifier_case() {
    let Statement::Insert(insert) = first(
        "INSERT INTO items VALUES (1) RETURNING WITH (OLD AS \"Image\", NEW AS \"image\") \"Image\".*, \"image\".*",
    ) else {
        panic!("expected INSERT");
    };
    assert_eq!(insert.returning_aliases.old, "Image");
    assert_eq!(insert.returning_aliases.new, "image");
    assert!(insert.returning_aliases.old_explicit);
    assert!(insert.returning_aliases.new_explicit);

    let error =
        compile("INSERT INTO items VALUES (1) RETURNING WITH (OLD AS image, NEW AS IMAGE) image.*")
            .unwrap_err();
    assert_eq!(error.sqlstate(), Some("42712"));
    assert!(error
        .to_string()
        .contains("table name \"image\" specified more than once"));
}

fn first(sql: &str) -> Statement {
    let mut v = compile(sql).unwrap();
    assert_eq!(v.len(), 1, "expected 1 stmt");
    v.remove(0)
}

fn null_literal_node() -> Node {
    Node {
        node: Some(NodeEnum::AConst(pg_query::protobuf::AConst {
            isnull: true,
            ..Default::default()
        })),
    }
}

#[test]
fn analyze_preserves_its_relation_and_rejects_dropped_semantics() {
    let Statement::Analyze { table } = first("ANALYZE app.docs") else {
        panic!("not ANALYZE");
    };
    assert_eq!(table.as_deref(), Some("app.docs"));
    let Statement::Analyze { table } = first("ANALYZE") else {
        panic!("not ANALYZE");
    };
    assert!(table.is_none());

    for (sql, expected) in [
        ("ANALYZE docs (title)", "column lists"),
        ("ANALYZE (VERBOSE) docs", "options"),
        ("VACUUM docs", "VACUUM"),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn malformed_type_cast_never_degrades_to_the_uncast_expression() {
    let cast = Node {
        node: Some(NodeEnum::TypeCast(Box::new(pg_query::protobuf::TypeCast {
            arg: Some(Box::new(null_literal_node())),
            type_name: None,
            ..Default::default()
        }))),
    };

    let error = compile_expr(&cast).unwrap_err();
    assert!(error.to_string().contains("without a target type"));
}

#[test]
fn malformed_operator_name_is_not_silently_discarded() {
    let expression = Node {
        node: Some(NodeEnum::AExpr(Box::new(pg_query::protobuf::AExpr {
            kind: pg_query::protobuf::AExprKind::AexprOp as i32,
            name: vec![Node::default()],
            lexpr: Some(Box::new(null_literal_node())),
            rexpr: Some(Box::new(null_literal_node())),
            ..Default::default()
        }))),
    };

    let error = compile_expr(&expression).unwrap_err();
    assert!(error.to_string().contains("missing string node"));
}

#[test]
fn prefix_minus_preserves_the_cast_operand() {
    let Statement::Select(select) = first("SELECT -1::smallint") else {
        panic!("expected SELECT");
    };
    let [projection] = select.projections.as_slice() else {
        panic!("expected one projection");
    };
    assert!(matches!(
        &projection.expr,
        Expr::UnaryMinus(inner)
            if matches!(inner.as_ref(), Expr::Cast { ty, .. } if ty == "smallint")
    ));
}

#[test]
fn sequence_options_do_not_truncate_or_ignore_values() {
    assert!(compile("CREATE SEQUENCE s START 1.5").is_err());
    let error = compile("CREATE SEQUENCE s CACHE 10").unwrap_err();
    assert!(error.to_string().contains("not supported"));
}

#[test]
fn create_table_with_vector_column() {
    let stmt = first("CREATE TABLE docs (id INTEGER PRIMARY KEY, title TEXT, embedding VECTOR(4))");
    let Statement::CreateTable(ct) = stmt else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(ct.name, "docs");
    assert_eq!(ct.columns.len(), 3);
    assert!(matches!(ct.columns[0].ty, ColumnType::Integer));
    assert!(ct.columns[0].primary_key);
    assert!(matches!(ct.columns[1].ty, ColumnType::Text));
    assert!(matches!(ct.columns[2].ty, ColumnType::Vector(4)));
}

#[test]
fn create_table_preserves_boolean_column_type() {
    let Statement::CreateTable(table) = first("CREATE TABLE flags (enabled BOOLEAN)") else {
        panic!("not CREATE TABLE");
    };
    assert!(matches!(table.columns[0].ty, ColumnType::Boolean));
}

#[test]
fn create_table_preserves_fixed_character_length() {
    let Statement::CreateTable(table) = first("CREATE TABLE labels (code CHAR(7))") else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(table.columns[0].ty, ColumnType::Character(7));
}

#[test]
fn create_table_preserves_postgresql_scalar_type_identity() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE typed_values (
            small_value SMALLINT,
            integer_value INTEGER,
            big_value BIGINT,
            oid_value OID,
            xid_value XID,
            real_value REAL,
            double_value DOUBLE PRECISION,
            text_value TEXT,
            name_value NAME,
            uuid_value UUID,
            varying_value VARCHAR(12),
            interval_value INTERVAL
        )",
    ) else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table
            .columns
            .iter()
            .map(|column| column.ty.clone())
            .collect::<Vec<_>>(),
        vec![
            ColumnType::SmallInteger,
            ColumnType::Integer,
            ColumnType::BigInteger,
            ColumnType::Oid,
            ColumnType::Xid,
            ColumnType::Real,
            ColumnType::DoublePrecision,
            ColumnType::Text,
            ColumnType::Name,
            ColumnType::Uuid,
            ColumnType::Varchar(Some(12)),
            ColumnType::Interval,
        ]
    );
}

#[test]
fn serial_family_preserves_width_and_sequence_semantics() {
    let Statement::CreateTable(table) =
        first("CREATE TABLE generated_ids (small_id SMALLSERIAL, id SERIAL4, big_id SERIAL8)")
    else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table
            .columns
            .iter()
            .map(|column| (column.ty.clone(), column.auto_increment))
            .collect::<Vec<_>>(),
        vec![
            (ColumnType::SmallInteger, true),
            (ColumnType::Integer, true),
            (ColumnType::BigInteger, true),
        ]
    );
}

#[test]
fn create_table_preserves_array_element_types_and_dimensions() {
    let Statement::CreateTable(table) =
        first("CREATE TABLE arrays (tags TEXT[], matrix INTEGER[][])")
    else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(
        table.columns[0].ty,
        ColumnType::Array(Box::new(ColumnType::Text))
    );
    assert_eq!(
        table.columns[1].ty,
        ColumnType::Array(Box::new(ColumnType::Array(Box::new(ColumnType::Integer))))
    );
}

#[test]
fn routine_type_names_preserve_percent_type_and_named_type_qualification() {
    let Statement::CreateFunction(function) = first(
        "CREATE FUNCTION typed_value(v app.items.value%TYPE, d app.amount_domain)
         RETURNS app.items.id%TYPE LANGUAGE sql AS $$ SELECT 1 $$",
    ) else {
        panic!("not CREATE FUNCTION");
    };
    assert_eq!(function.params[0].type_name, "app.items.value%type");
    assert_eq!(
        function.params[0].type_reference,
        Some(crate::ast::RoutineColumnTypeReference::new(
            Some("app".into()),
            "items".into(),
            "value".into()
        ))
    );
    assert_eq!(function.params[1].type_name, "app.amount_domain");
    assert!(matches!(
        function.returns,
        crate::ast::FunctionReturns::Scalar { type_name }
            if type_name == "app.items.id%type"
    ));
}

#[test]
fn routine_builtin_array_names_use_sql_array_spelling() {
    let Statement::CreateFunction(function) = first(
        "CREATE FUNCTION array_names(integer[]) RETURNS text[] LANGUAGE sql AS $$ SELECT ARRAY['x'] $$",
    ) else {
        panic!("not CREATE FUNCTION");
    };
    assert_eq!(function.params[0].type_name, "int4[]");
    assert!(matches!(
        function.returns,
        crate::ast::FunctionReturns::Scalar { type_name } if type_name == "text[]"
    ));
}

#[test]
fn routine_percent_type_keeps_quoted_dotted_components_structured() {
    let Statement::CreateFunction(function) = first(
        "CREATE FUNCTION typed_dot(v \"app.dot\".\"items.dot\".\"value.dot\"%TYPE)
         RETURNS \"app.dot\".\"items.dot\".\"value.dot\"%TYPE LANGUAGE sql AS $$ SELECT $1 $$",
    ) else {
        panic!("not CREATE FUNCTION");
    };
    let expected = crate::ast::RoutineColumnTypeReference::new(
        Some("app.dot".into()),
        "items.dot".into(),
        "value.dot".into(),
    );
    assert_eq!(function.params[0].type_reference, Some(expected.clone()));
    assert_eq!(function.return_type_reference, Some(expected));
    assert_eq!(
        function.params[0].type_name,
        "\"app.dot\".\"items.dot\".\"value.dot\"%type"
    );
}

#[test]
fn create_table_preserves_typed_composite_keys_and_null_policy() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE memberships (
            tenant TEXT,
            member TEXT,
            email TEXT,
            CONSTRAINT memberships_pkey PRIMARY KEY (tenant, member),
            CONSTRAINT memberships_email_key UNIQUE NULLS NOT DISTINCT (tenant, email)
        )",
    ) else {
        panic!("not CREATE TABLE");
    };

    assert_eq!(table.key_constraints.len(), 2);
    assert_eq!(
        table.key_constraints[0].kind,
        TableKeyConstraintKind::PrimaryKey
    );
    assert_eq!(table.key_constraints[0].columns, vec!["tenant", "member"]);
    assert_eq!(
        table.key_constraints[0].name.as_deref(),
        Some("memberships_pkey")
    );
    assert_eq!(
        table.key_constraints[1].kind,
        TableKeyConstraintKind::Unique
    );
    assert_eq!(table.key_constraints[1].columns, vec!["tenant", "email"]);
    assert!(table.key_constraints[1].nulls_not_distinct);

    assert!(table.columns[0].not_null);
    assert!(table.columns[1].not_null);
    assert!(!table.columns[0].primary_key);
    assert!(!table.columns[1].primary_key);
}

#[test]
fn create_table_preserves_named_column_keys() {
    let Statement::CreateTable(table) = first(
        "CREATE TABLE users (
            id INTEGER CONSTRAINT users_pkey PRIMARY KEY,
            email TEXT CONSTRAINT users_email_key UNIQUE
        )",
    ) else {
        panic!("not CREATE TABLE");
    };
    assert_eq!(table.key_constraints.len(), 2);
    assert_eq!(table.key_constraints[0].name.as_deref(), Some("users_pkey"));
    assert_eq!(
        table.key_constraints[1].name.as_deref(),
        Some("users_email_key")
    );
    assert!(table.columns[0].not_null);
}

#[test]
fn create_table_rejects_invalid_key_declarations() {
    for sql in [
        "CREATE TABLE t (a INTEGER, CONSTRAINT same UNIQUE (a), CONSTRAINT same CHECK (a > 0))",
        "CREATE TABLE t (a INTEGER, UNIQUE (missing))",
        "CREATE TABLE t (a INTEGER, UNIQUE (a, a))",
        "CREATE TABLE t (a INTEGER PRIMARY KEY, b INTEGER, PRIMARY KEY (b))",
    ] {
        assert!(compile(sql).is_err(), "expected invalid DDL to fail: {sql}");
    }
}

#[test]
fn explicit_grouping_sets_preserve_every_key_expression() {
    let Statement::Select(select) =
        first("SELECT g, v, count(*) FROM spill_data GROUP BY GROUPING SETS ((g), (v), ())")
    else {
        panic!("not SELECT");
    };
    assert_eq!(
        select.grouping_sets.len(),
        3,
        "compiled grouping sets: {:?}",
        select.grouping_sets
    );
    assert_eq!(select.grouping_sets[0].len(), 1);
    assert_eq!(select.grouping_sets[1].len(), 1);
    assert!(select.grouping_sets[2].is_empty());
}

#[test]
fn rollup_cube_and_multiple_grouping_items_expand_without_dropping_keys() {
    let Statement::Select(rollup) = first("SELECT g, v, count(*) FROM t GROUP BY ROLLUP (g, v)")
    else {
        panic!("not SELECT");
    };
    assert_eq!(
        rollup
            .grouping_sets
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 1, 0]
    );

    let Statement::Select(cube) = first("SELECT g, v, count(*) FROM t GROUP BY CUBE (g, v)") else {
        panic!("not SELECT");
    };
    let mut cube_widths = cube.grouping_sets.iter().map(Vec::len).collect::<Vec<_>>();
    cube_widths.sort_unstable();
    assert_eq!(cube_widths, vec![0, 1, 1, 2]);

    let Statement::Select(product) = first(
        "SELECT a, b, c, d, count(*) FROM t \
         GROUP BY GROUPING SETS ((a), (b)), GROUPING SETS ((c), (d))",
    ) else {
        panic!("not SELECT");
    };
    assert_eq!(product.grouping_sets.len(), 4);
    assert!(product.grouping_sets.iter().all(|set| set.len() == 2));
}

#[test]
fn group_by_distinct_is_preserved_for_post_binding_deduplication() {
    let Statement::Select(plain) = first("SELECT g, count(*) FROM t GROUP BY DISTINCT g") else {
        panic!("not SELECT");
    };
    assert!(plain.group_distinct);
    assert_eq!(plain.group_by.len(), 1);
    assert!(plain.grouping_sets.is_empty());

    let Statement::Select(repeated) = first(
        "SELECT g, v, count(*) FROM t \
         GROUP BY DISTINCT GROUPING SETS ((g), (g), (v), (g))",
    ) else {
        panic!("not SELECT");
    };
    let Statement::Select(all) = first(
        "SELECT g, v, count(*) FROM t \
         GROUP BY ALL GROUPING SETS ((g), (g), (v), (g))",
    ) else {
        panic!("not SELECT");
    };
    assert!(repeated.group_distinct);
    assert!(!all.group_distinct);
    assert_eq!(
        repeated.grouping_sets.len(),
        4,
        "the compiler must retain duplicates until input types are bound"
    );
    assert_eq!(
        serde_json::to_value(&repeated.grouping_sets).unwrap(),
        serde_json::to_value(&all.grouping_sets).unwrap()
    );

    let Statement::Select(alias) = first(
        "SELECT g + 1 AS shifted, count(*) FROM t \
         GROUP BY DISTINCT GROUPING SETS ((shifted), (g + 1))",
    ) else {
        panic!("not SELECT");
    };
    assert_eq!(alias.grouping_sets.len(), 2);
    assert_eq!(
        serde_json::to_value(&alias.grouping_sets[0]).unwrap(),
        serde_json::to_value(&alias.grouping_sets[1]).unwrap(),
        "alias resolution precedes type-aware grouping-set deduplication"
    );

    let Statement::Select(explicit_rows) = first(
        "SELECT count(*) FROM t \
         GROUP BY DISTINCT GROUPING SETS ((ROW(g, v)), (ROW(v, g)))",
    ) else {
        panic!("not SELECT");
    };
    assert!(explicit_rows.group_distinct);
    assert_eq!(explicit_rows.grouping_sets.len(), 2);
    assert!(explicit_rows
        .grouping_sets
        .iter()
        .all(|set| matches!(set.as_slice(), [Expr::Row(_)])));
}

#[test]
fn create_table_with_tensor_column() {
    let stmt = first("CREATE TABLE docs (id INTEGER PRIMARY KEY, chunks TENSOR(4))");
    let Statement::CreateTable(ct) = stmt else {
        panic!("not CREATE TABLE");
    };
    assert!(matches!(ct.columns[1].ty, ColumnType::Tensor(4)));
}

#[test]
fn create_index_records_access_method() {
    let stmt = first("CREATE INDEX idx_body ON docs USING gin (body)");
    let Statement::CreateIndex(ci) = stmt else {
        panic!("not CREATE INDEX");
    };
    assert_eq!(ci.table, "docs");
    assert_eq!(ci.access_method, "gin");
    assert_eq!(ci.columns, vec!["body"]);
}

#[test]
fn table_commands_preserve_qualified_relation_names() {
    let stmt = first("ALTER TABLE app.docs ADD COLUMN version INTEGER");
    let Statement::AlterTable(alter) = stmt else {
        panic!("not ALTER TABLE");
    };
    assert_eq!(alter.table, "app.docs");

    let stmt = first("ALTER TABLE app.docs RENAME TO archived_docs");
    let Statement::AlterTable(rename) = stmt else {
        panic!("not ALTER TABLE RENAME");
    };
    assert_eq!(rename.table, "app.docs");

    let Statement::Update(update) = first("UPDATE app.docs SET version = 2") else {
        panic!("not UPDATE");
    };
    assert_eq!(update.table, "app.docs");

    let Statement::Delete(delete) = first("DELETE FROM app.docs") else {
        panic!("not DELETE");
    };
    assert_eq!(delete.table, "app.docs");

    let Statement::Truncate { tables, .. } = first("TRUNCATE app.docs") else {
        panic!("not TRUNCATE");
    };
    assert_eq!(tables, vec!["app.docs"]);

    let Statement::Insert(insert) = first("INSERT INTO app.docs (version) VALUES (1)") else {
        panic!("not INSERT");
    };
    assert_eq!(insert.table, "app.docs");
}

#[test]
fn alter_column_type_preserves_the_using_expression() {
    let Statement::AlterTable(alter) =
        first("ALTER TABLE metrics ALTER COLUMN value TYPE text USING (value + delta)::text")
    else {
        panic!("not ALTER TABLE ALTER COLUMN TYPE");
    };
    let crate::ast::AlterTableAction::AlterColumnType { name, ty, using } = alter.action else {
        panic!("not ALTER COLUMN TYPE");
    };
    assert_eq!(name, "value");
    assert_eq!(ty, ColumnType::Text);
    assert!(matches!(using, Some(Expr::Cast { ty, .. }) if ty == "text"));
}

#[test]
fn insert_with_array_literal() {
    let stmt = first(
        "INSERT INTO docs (id, title, embedding) VALUES \
         (1, 'rust language', ARRAY[0.1, 0.2, 0.3])",
    );
    let Statement::Insert(i) = stmt else {
        panic!("not INSERT");
    };
    assert_eq!(i.table, "docs");
    assert_eq!(i.columns, vec!["id", "title", "embedding"]);
    assert_eq!(i.rows.len(), 1);
    assert_eq!(i.rows[0].len(), 3);
    match &i.rows[0][2] {
        Expr::Array(v) => assert_eq!(v.len(), 3),
        other => panic!("expected Array, got {other:?}"),
    }
}

#[test]
fn insert_set_operation_is_compiled_as_one_select_source() {
    let Statement::Insert(insert) =
        first("INSERT INTO dst SELECT id FROM lhs UNION ALL SELECT id FROM rhs LIMIT 2 OFFSET 1")
    else {
        panic!("not INSERT");
    };
    assert!(insert.rows.is_empty());
    let source = insert
        .select_source
        .expect("INSERT must retain its SELECT source");
    let set_op = source
        .set_op
        .as_ref()
        .expect("INSERT source must retain its set operation");
    assert_eq!(set_op.kind, crate::ast::SetOpKind::Union);
    assert!(set_op.all);
    assert!(set_op.combined_limit.is_some());
    assert!(set_op.combined_offset.is_some());
}

#[test]
fn select_with_function_call_and_order_by() {
    let stmt = first(
        "SELECT id, title, _score AS s FROM docs \
         WHERE text_match(body, 'rust language') \
         ORDER BY _score DESC LIMIT 5",
    );
    let Statement::Select(s) = stmt else {
        panic!("not SELECT");
    };
    assert_eq!(s.projections.len(), 3);
    assert_eq!(s.projections[2].alias.as_deref(), Some("s"));
    match &s.from {
        Some(FromClause::Table { name, .. }) => assert_eq!(name, "docs"),
        other => panic!("expected single-table FROM, got {other:?}"),
    }
    match &s.r#where {
        Some(Expr::Func {
            distinct: false,
            order_by,
            filter: None,
            ..
        }) if order_by.is_empty() => {}
        other => panic!("expected scalar function call, got {other:?}"),
    }
    assert_eq!(s.order_by.len(), 1);
    assert!(s.order_by[0].descending);
    match &s.limit {
        Some(Expr::Literal(uqa_core::Value::Int(5))) => {}
        other => panic!("expected LIMIT 5, got {other:?}"),
    }
}

#[test]
fn unsupported_select_clauses_fail_instead_of_losing_semantics() {
    for (sql, expected) in [
        ("SELECT 1 INTO created_by_select", "SELECT INTO"),
        (
            "SELECT row_number() OVER named_window FROM employees WINDOW named_window AS (ORDER BY id)",
            "named WINDOW",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn fetch_with_ties_preserves_its_boundary_and_requires_ordering() {
    let Statement::Select(select) =
        first("SELECT id FROM employees ORDER BY department, id DESC FETCH FIRST 2 ROWS WITH TIES")
    else {
        panic!("not SELECT");
    };
    assert!(select.with_ties);
    assert_eq!(select.order_by.len(), 2);
    assert!(matches!(
        select.limit,
        Some(Expr::Literal(uqa_core::Value::Int(2)))
    ));

    let Statement::Select(null_count) =
        first("SELECT id FROM employees ORDER BY id FETCH FIRST NULL ROWS WITH TIES")
    else {
        panic!("not SELECT");
    };
    assert!(null_count.with_ties);
    assert!(matches!(
        null_count.limit,
        Some(Expr::Literal(uqa_core::Value::Null))
    ));

    let error = compile("SELECT id FROM employees FETCH FIRST 1 ROW WITH TIES").unwrap_err();
    assert_eq!(error.sqlstate(), Some("42601"));
    assert_eq!(
        error.to_string(),
        "WITH TIES cannot be specified without ORDER BY clause"
    );

    let Statement::Select(values) =
        first("VALUES (1), (2), (2) ORDER BY 1 FETCH FIRST 2 ROWS WITH TIES")
    else {
        panic!("ordered VALUES must be a SELECT query block");
    };
    assert!(values.with_ties);
    assert!(matches!(values.from, Some(FromClause::Values { .. })));
    assert!(matches!(values.projections[0].expr, Expr::Star));
}

#[test]
fn unsupported_from_forms_fail_instead_of_becoming_cross_joins() {
    for (sql, expected) in [
        (
            "SELECT * FROM ROWS FROM (generate_series(1, 2), generate_series(3, 4)) AS f(a, b)",
            "ROWS FROM",
        ),
        (
            "SELECT * FROM generate_series(1, 2) WITH ORDINALITY",
            "WITH ORDINALITY",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
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

#[test]
fn alter_table_add_key_constraint_preserves_tuple_shape() {
    let Statement::AlterTable(alter) =
        first("ALTER TABLE labels ADD CONSTRAINT labels_tenant_slug_key UNIQUE (tenant, slug)")
    else {
        panic!("expected ALTER TABLE");
    };
    assert!(matches!(
        alter.action,
        AlterTableAction::AddKeyConstraint { constraint }
            if constraint.name.as_deref() == Some("labels_tenant_slug_key")
                && constraint.kind == TableKeyConstraintKind::Unique
                && constraint.columns == ["tenant", "slug"]
    ));
}

#[test]
fn alter_sequence_preserves_if_exists() {
    let Statement::AlterSequence(sequence) =
        first("ALTER SEQUENCE IF EXISTS absent RESTART WITH 7")
    else {
        panic!("expected ALTER SEQUENCE");
    };
    assert!(sequence.if_exists);
    assert_eq!(sequence.restart, crate::ast::SequenceRestart::With(7));
}

#[test]
fn unsupported_create_ddl_never_loses_lifecycle_semantics() {
    for (sql, expected) in [
        ("CREATE TEMP TABLE temp_t (id INTEGER)", "TEMPORARY"),
        ("CREATE UNLOGGED TABLE unlogged_t (id INTEGER)", "UNLOGGED"),
        (
            "CREATE TABLE inherited (id INTEGER) INHERITS (parent)",
            "INHERITS",
        ),
        (
            "CREATE TABLE optioned (id INTEGER) WITH (fillfactor = 70)",
            "storage options",
        ),
        (
            "CREATE TABLE spaced (id INTEGER) TABLESPACE fastspace",
            "TABLESPACE",
        ),
        (
            "CREATE TABLE accessed (id INTEGER) USING heap",
            "access methods",
        ),
        (
            "CREATE SCHEMA owned AUTHORIZATION CURRENT_USER",
            "AUTHORIZATION",
        ),
        (
            "CREATE SCHEMA bundled CREATE TABLE child (id INTEGER)",
            "schema elements",
        ),
        ("CREATE TEMP VIEW temp_v AS SELECT 1", "TEMPORARY"),
        ("CREATE VIEW aliased(value) AS SELECT 1", "column aliases"),
        (
            "CREATE VIEW checked AS SELECT 1 WITH LOCAL CHECK OPTION",
            "CHECK OPTION",
        ),
        (
            "CREATE VIEW optioned_v WITH (security_barrier = true) AS SELECT 1",
            "options",
        ),
        (
            "CREATE MATERIALIZED VIEW materialized AS SELECT 1",
            "MATERIALIZED VIEW",
        ),
        ("CREATE TEMP TABLE temp_as AS SELECT 1", "TEMPORARY"),
        ("CREATE TABLE named(value) AS SELECT 1", "column-name lists"),
        (
            "CREATE TABLE no_data AS SELECT 1 WITH NO DATA",
            "WITH NO DATA",
        ),
        ("CREATE TEMP SEQUENCE temp_sequence", "TEMPORARY"),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn for_update_compiles_with_postgresql_lock_options() {
    let Statement::Select(select) = first("SELECT * FROM employees e FOR UPDATE OF e NOWAIT")
    else {
        panic!("expected SELECT");
    };
    assert_eq!(select.locking.len(), 1);
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForUpdate
    );
    assert_eq!(select.locking[0].wait, crate::ast::LockWait::NoWait);
    assert_eq!(select.locking[0].relations, ["e"]);

    let Statement::Select(select) = first("SELECT * FROM employees e FOR SHARE OF e SKIP LOCKED")
    else {
        panic!("expected SELECT");
    };
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForShare
    );
    assert_eq!(select.locking[0].wait, crate::ast::LockWait::SkipLocked);

    let Statement::Select(select) = first("SELECT * FROM employees FOR NO KEY UPDATE") else {
        panic!("expected SELECT");
    };
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForNoKeyUpdate
    );
    assert!(select.locking[0].relations.is_empty());

    let Statement::Select(select) = first("SELECT * FROM employees FOR KEY SHARE") else {
        panic!("expected SELECT");
    };
    assert_eq!(
        select.locking[0].strength,
        crate::ast::LockStrength::ForKeyShare
    );

    let Statement::Select(select) =
        first("SELECT * FROM (SELECT id FROM employees FOR SHARE) AS e FOR UPDATE OF e NOWAIT")
    else {
        panic!("expected SELECT");
    };
    let Some(crate::ast::FromClause::Subquery { body, .. }) = select.from else {
        panic!("expected derived table");
    };
    assert_eq!(body.locking.len(), 2);
    assert!(body.locking.iter().any(|clause| {
        clause.strength == crate::ast::LockStrength::ForShare
            && clause.wait == crate::ast::LockWait::Block
    }));
    assert!(body.locking.iter().any(|clause| {
        clause.strength == crate::ast::LockStrength::ForUpdate
            && clause.wait == crate::ast::LockWait::NoWait
            && clause.relations.is_empty()
    }));
}

#[test]
fn for_update_rejects_postgresql_illegal_shapes() {
    for (sql, expected) in [
        (
            "SELECT DISTINCT id FROM employees FOR UPDATE",
            "DISTINCT clause",
        ),
        (
            "SELECT department, count(*) FROM employees GROUP BY department FOR UPDATE",
            "GROUP BY clause",
        ),
        (
            "SELECT count(*) FROM employees FOR UPDATE",
            "aggregate functions",
        ),
        (
            "SELECT row_number() OVER (ORDER BY id) FROM employees FOR UPDATE",
            "window functions",
        ),
        (
            "SELECT id FROM employees ORDER BY row_number() OVER () FOR UPDATE",
            "window functions",
        ),
        (
            "SELECT id FROM employees UNION SELECT id FROM employees FOR UPDATE",
            "UNION/INTERSECT/EXCEPT",
        ),
        ("VALUES (1) FOR UPDATE", "VALUES"),
        (
            "SELECT * FROM generate_series(1, 3) AS g FOR UPDATE OF g",
            "function",
        ),
        (
            "SELECT * FROM employees e LEFT JOIN departments d ON e.department_id = d.id FOR UPDATE",
            "nullable side",
        ),
        (
            "SELECT count(*) FROM employees HAVING count(*) > 0 FOR UPDATE",
            "HAVING clause",
        ),
        (
            "SELECT * FROM (SELECT DISTINCT id FROM employees) AS e FOR UPDATE OF e",
            "DISTINCT clause",
        ),
    ] {
        let error = compile(sql).expect_err(sql);
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}");
        assert!(
            error.to_string().contains(expected),
            "unexpected error for {sql}: {error}"
        );
    }
    let missing = compile("SELECT * FROM employees e FOR UPDATE OF missing").unwrap_err();
    assert_eq!(missing.sqlstate(), Some("42P01"));
    let hidden = compile("SELECT * FROM employees e FOR UPDATE OF employees").unwrap_err();
    assert_eq!(hidden.sqlstate(), Some("42P01"));
    let hidden_function =
        compile("SELECT * FROM generate_series(1, 3) AS g FOR UPDATE OF generate_series")
            .unwrap_err();
    assert_eq!(hidden_function.sqlstate(), Some("42P01"));
    let cte =
        compile("WITH e AS (SELECT * FROM employees) SELECT * FROM e FOR UPDATE OF e").unwrap_err();
    assert_eq!(cte.sqlstate(), Some("0A000"));
    assert!(cte.to_string().contains("WITH query"));

    for sql in [
        "SELECT 1 FOR UPDATE",
        "SELECT * FROM generate_series(1, 3) AS g FOR UPDATE",
        "WITH e AS (SELECT * FROM employees) SELECT * FROM e FOR UPDATE",
        "SELECT * FROM (VALUES (1)) AS v(id) FOR UPDATE OF v",
        "SELECT * FROM (WITH e AS (SELECT * FROM employees) SELECT * FROM e) AS s FOR UPDATE OF s",
    ] {
        compile(sql).unwrap_or_else(|error| panic!("unexpected error for {sql}: {error}"));
    }
}

#[test]
fn repeated_lock_targets_and_clauses_compile_for_executor_merging() {
    let Statement::Select(select) =
        first("SELECT * FROM employees e FOR KEY SHARE OF e, e SKIP LOCKED FOR UPDATE OF e NOWAIT")
    else {
        panic!("expected SELECT");
    };
    assert_eq!(select.locking.len(), 2);
    assert_eq!(select.locking[0].relations, ["e", "e"]);
    assert_eq!(
        select.locking[1].strength,
        crate::ast::LockStrength::ForUpdate
    );
    assert_eq!(select.locking[1].wait, crate::ast::LockWait::NoWait);
}

#[test]
fn row_lock_outer_join_reduction_uses_known_function_strictness() {
    compile("SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE abs(b.id) > 0 FOR UPDATE OF b")
        .expect("ABS is strict and rejects the null-extended side");
    let non_strict = compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE coalesce(b.id, 1) > 0 FOR UPDATE OF b",
    )
    .unwrap_err();
    assert!(
        non_strict.to_string().contains("nullable side"),
        "{non_strict}"
    );
    compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE application_strict(b.id) > 0 FOR UPDATE OF b",
    )
    .expect("catalog-owned function strictness is deferred until engine binding");
}

#[test]
fn row_lock_outer_join_reduction_preserves_between_three_valued_logic() {
    let not_between = compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE a.id NOT BETWEEN b.low AND 10 FOR UPDATE OF b",
    )
    .unwrap_err();
    assert!(
        not_between.to_string().contains("nullable side"),
        "{not_between}"
    );
    compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE a.id BETWEEN 0 AND b.high FOR UPDATE OF b",
    )
    .expect("a nullable upper bound makes BETWEEN NULL or FALSE, never TRUE");
    compile(
        "SELECT a.id FROM a LEFT JOIN b ON a.id = b.id WHERE a.id BETWEEN SYMMETRIC 0 AND b.high FOR UPDATE OF b",
    )
    .expect("BETWEEN SYMMETRIC is strict in all three arguments");
}

#[test]
fn set_operation_locking_error_names_the_requested_strength() {
    let error =
        compile("SELECT id FROM employees UNION SELECT id FROM employees FOR SHARE").unwrap_err();
    assert!(error.to_string().contains("FOR SHARE"), "{error}");

    for sql in [
        "(SELECT id FROM employees FOR UPDATE) UNION SELECT id FROM employees",
        "SELECT id FROM employees UNION (SELECT id FROM employees FOR KEY SHARE)",
    ] {
        let error = compile(sql).expect_err(sql);
        assert_eq!(error.sqlstate(), Some("0A000"), "{sql}: {error}");
        assert!(
            error.to_string().contains("UNION/INTERSECT/EXCEPT"),
            "{sql}: {error}"
        );
    }

    compile(
        "SELECT id FROM (SELECT id FROM employees FOR UPDATE) AS locked UNION SELECT id FROM employees",
    )
    .expect("locking inside a derived table is not a direct set-operation operand lock");
}
