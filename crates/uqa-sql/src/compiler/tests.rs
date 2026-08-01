use super::*;
use crate::ast::{ColumnType, FromClause, TableKeyConstraintKind};

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
            "SELECT department, count(*) FROM employees GROUP BY DISTINCT department",
            "GROUP BY DISTINCT",
        ),
        (
            "SELECT row_number() OVER named_window FROM employees WINDOW named_window AS (ORDER BY id)",
            "named WINDOW",
        ),
        (
            "SELECT * FROM employees ORDER BY id FETCH FIRST 1 ROW WITH TIES",
            "WITH TIES",
        ),
        ("SELECT * FROM employees FOR UPDATE", "row-locking"),
    ] {
        let error = compile(sql).expect_err(sql);
        assert!(
            matches!(&error, SQLError::Unsupported(message) if message.contains(expected)),
            "unexpected error for {sql}: {error}"
        );
    }
}

#[test]
fn unsupported_from_forms_fail_instead_of_becoming_cross_joins() {
    for (sql, expected) in [
        (
            "SELECT * FROM left_table NATURAL JOIN right_table",
            "NATURAL JOIN",
        ),
        (
            "SELECT * FROM left_table JOIN right_table USING (id)",
            "JOIN USING",
        ),
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
