//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

const DOMAIN_CHECK: &str = "{OPEXPR :opno 521 :opfuncid 147 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({COERCETODOMAINVALUE :typeId 23 :typeMod -1 :collation 0 :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 0 0 0 0 0 0 0 0 ]}) :location -1}";

#[test]
fn reads_postgresql_domain_check_with_typed_value_and_machine_width_datum() {
    let value = parse(DOMAIN_CHECK).unwrap();
    assert_eq!(value.to_string(), DOMAIN_CHECK);
    let Field::Node(node) = value else {
        panic!("not an expression node")
    };
    assert_eq!(node.integer("opno").unwrap(), 521);
    assert!(!node.boolean("opretset").unwrap());
    let Field::List(arguments) = node.field("args").unwrap() else {
        panic!("missing operands")
    };
    let Field::Node(value) = &arguments[0] else {
        panic!("missing domain value")
    };
    assert_eq!(value.kind, "COERCETODOMAINVALUE");
    assert_eq!(value.integer("typeId").unwrap(), 23);
    let Field::Node(constant) = &arguments[1] else {
        panic!("missing constant")
    };
    assert_eq!(
        constant.field("constvalue").unwrap(),
        &Field::Datum {
            length: 4,
            bytes: vec![0; 8]
        }
    );
}

#[test]
fn preserves_escaped_strings_nulls_lists_and_platform_datum_bytes() {
    for value in [
        "",
        "<>",
        "123",
        "+.5",
        "a b",
        "a\nb",
        "a\\b",
        "(한){글}",
        "\"quoted",
    ] {
        let field = Field::String(value.into());
        assert_eq!(parse(&field.to_string()).unwrap(), field);
    }
    for source in [
        "<>",
        "(b)",
        "(i 1 -2)",
        "(o 23 4294967295)",
        "4 [ 16 0 0 0 ]",
    ] {
        assert_eq!(parse(source).unwrap().to_string(), source);
    }
    let bytes = Field::Datum {
        length: 3,
        bytes: vec![237, 149, 156],
    };
    assert_eq!(parse("3 [ -19 -107 -100 ]").unwrap(), bytes);
    assert_eq!(parse("3 [ 237 149 156 ]").unwrap(), bytes);
    assert_eq!(parse(&bytes.to_string()).unwrap(), bytes);
}

#[test]
fn rejects_truncated_ambiguous_and_excessively_nested_node_trees() {
    for input in [
        "",
        "{CONST",
        "{CONST :x}",
        "{CONST :x 1 :x 2}",
        "{CONST :x (1}",
        "4 [ 0",
        "4 [ 256 ]",
        "4 [ -129 ]",
        "unfinished\\",
    ] {
        assert!(parse(input).is_err(), "{input}");
    }
    assert!(parse("<> <>").is_err());
    let deep = format!("{}<>{}", "(".repeat(258), ")".repeat(258));
    assert_eq!(parse(&deep).unwrap_err().sqlstate(), Some("54001"));
}

struct Routines;

impl deparse::ExpressionNames for Routines {
    fn column(&self, attribute: i64) -> Result<String, SQLError> {
        match attribute {
            1 => Ok("Mixed Case".into()),
            2 => Ok("txt".into()),
            _ => Err(super::invalid("unknown attribute")),
        }
    }

    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        assert_eq!(oid, 1317);
        Ok(vec!["length".into()])
    }

    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        if oid == 1043 {
            return Ok(if modifier < 0 {
                "character varying".into()
            } else {
                format!("character varying({})", modifier - 4)
            });
        }
        if oid == 1700 && modifier >= 0 {
            let modifier = modifier - 4;
            return Ok(format!("numeric({},{})", modifier >> 16, modifier & 0x7ff));
        }
        match oid {
            1082 => return Ok("date".into()),
            1083 => return Ok("time without time zone".into()),
            1114 => return Ok("timestamp without time zone".into()),
            1266 => return Ok("time with time zone".into()),
            _ => {}
        }
        Ok(crate::catalog::type_metadata::catalog_type_name(oid).into())
    }
}

#[test]
fn typed_check_nodes_and_sql_match_postgresql() {
    #[derive(serde::Deserialize)]
    struct Fixture {
        base: String,
        check: String,
        conbin: String,
        sql: String,
        pretty: String,
    }
    // PostgreSQL 18.4 reference; fixtures retain expected expressions, not machine reports.
    let fixtures: Vec<Fixture> = serde_json::from_str(include_str!("tests/checks.json")).unwrap();
    let mut failures = Vec::new();
    for fixture in fixtures {
        let base = if fixture.base == "d_base" {
            crate::ColumnType::Domain {
                schema: "public".into(),
                name: "d_base".into(),
                oid: 16385,
                base: Box::new(crate::ColumnType::Integer),
            }
        } else {
            crate::ColumnType::from_sql_name(&fixture.base).unwrap()
        };
        let statement = format!(
            "CREATE DOMAIN d AS {} CHECK({})",
            fixture.base, fixture.check
        );
        let crate::Statement::CreateDomain(domain) = crate::compile(&statement).unwrap().remove(0)
        else {
            panic!("domain declaration")
        };
        let schema = crate::RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: None,
            routines: &Routines,
        };
        match context.check(&domain.checks[0].expression) {
            Ok(node) => {
                let actual = Field::Node(node);
                let expected = parse(&fixture.conbin).unwrap();
                if actual != expected {
                    failures.push(format!(
                        "{}: actual {actual}\nexpected {expected}",
                        fixture.check
                    ));
                }
                for (pretty, expected) in [(false, fixture.sql), (true, fixture.pretty)] {
                    let output = deparse::expression(&actual, &Routines, pretty);
                    if output.as_ref().ok() != Some(&expected) {
                        failures.push(format!(
                            "{} (pretty={pretty}): {output:?}, expected {expected}",
                            fixture.check
                        ));
                    }
                }
            }
            Err(error) => failures.push(format!("{}: {error}", fixture.check)),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

impl expressions::ExpressionRoutines for Routines {
    fn resolve(
        &self,
        name: &str,
        _: Option<&crate::ast::FunctionBinding>,
        arguments: &[Option<crate::ColumnType>],
    ) -> Result<expressions::RoutineIdentity, crate::SQLError> {
        assert_eq!(name, "length");
        assert_eq!(arguments, &[Some(crate::ColumnType::Text)]);
        Ok(expressions::RoutineIdentity {
            oid: 1317,
            argument_types: vec![crate::ColumnType::Text],
            result_type: crate::ColumnType::Integer,
        })
    }
}

fn domain_expression(sql: &str, base: crate::ColumnType) -> String {
    let crate::Statement::CreateDomain(domain) = crate::compile(sql).unwrap().remove(0) else {
        panic!("expected a domain declaration")
    };
    let schema = crate::RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
    expressions::ExpressionContext {
        schema: &schema,
        domain_value: Some(&base),
        types: None,
        routines: &Routines,
    }
    .check(&domain.checks[0].expression)
    .unwrap()
    .to_string()
}

#[test]
fn domain_integer_check_matches_postgresql_typed_tree() {
    assert_eq!(
        domain_expression(
            "CREATE DOMAIN d AS int CHECK(VALUE>0)",
            crate::ColumnType::Integer
        ),
        DOMAIN_CHECK
    );
    let tree = parse(DOMAIN_CHECK).unwrap();
    assert_eq!(
        deparse::expression(&tree, &Routines, false).unwrap(),
        "(VALUE > 0)"
    );
    assert_eq!(
        deparse::expression(&tree, &Routines, true).unwrap(),
        "VALUE > 0"
    );
}

#[test]
fn nested_domain_value_retains_its_oid_before_base_type_coercion() {
    let base = crate::ColumnType::Domain {
        schema: "public".into(),
        name: "d".into(),
        oid: 16385,
        base: Box::new(crate::ColumnType::Integer),
    };
    let actual = domain_expression("CREATE DOMAIN nested AS d CHECK(VALUE<100)", base);
    let expected = "{OPEXPR :opno 97 :opfuncid 66 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({RELABELTYPE :arg {COERCETODOMAINVALUE :typeId 16385 :typeMod -1 :collation 0 :location -1} :resulttype 23 :resulttypmod -1 :resultcollid 0 :relabelformat 2 :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 100 0 0 0 0 0 0 0 ]}) :location -1}";
    assert_eq!(actual, expected);
    let tree = parse(&actual).unwrap();
    assert_eq!(
        deparse::expression(&tree, &Routines, false).unwrap(),
        "((VALUE)::integer < 100)"
    );
    assert_eq!(
        deparse::expression(&tree, &Routines, true).unwrap(),
        "VALUE::integer < 100"
    );
}

#[test]
fn function_node_retains_declared_argument_identity_and_input_collation() {
    let actual = domain_expression(
        "CREATE DOMAIN d AS text CHECK(length(VALUE)>0)",
        crate::ColumnType::Text,
    );
    let expected = "{OPEXPR :opno 521 :opfuncid 147 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 0 :args ({FUNCEXPR :funcid 1317 :funcresulttype 23 :funcretset false :funcvariadic false :funcformat 0 :funccollid 0 :inputcollid 100 :args ({COERCETODOMAINVALUE :typeId 25 :typeMod -1 :collation 100 :location -1}) :location -1} {CONST :consttype 23 :consttypmod -1 :constcollid 0 :constlen 4 :constbyval true :constisnull false :location -1 :constvalue 4 [ 0 0 0 0 0 0 0 0 ]}) :location -1}";
    assert_eq!(actual, expected);
    let tree = parse(&actual).unwrap();
    assert_eq!(
        deparse::expression(&tree, &Routines, false).unwrap(),
        "(length(VALUE) > 0)"
    );
    assert_eq!(
        deparse::expression(&tree, &Routines, true).unwrap(),
        "length(VALUE) > 0"
    );
}

#[test]
fn table_checks_bind_original_attribute_ordinals_and_unknown_text_literals() {
    let crate::Statement::CreateTable(table) = crate::compile(
        "CREATE TABLE t(\"Mixed Case\" int CHECK(\"Mixed Case\">0), txt text CHECK(txt<>''))",
    )
    .unwrap()
    .remove(0) else {
        panic!("table declaration")
    };
    let schema = crate::RowSchema::with_types(
        table
            .columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        table
            .columns
            .iter()
            .map(|column| Some(column.ty.clone()))
            .collect(),
    );
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: None,
        types: None,
        routines: &Routines,
    };
    let integer = context
        .check(table.columns[0].check.as_ref().unwrap())
        .unwrap()
        .to_string();
    assert_eq!(integer, DOMAIN_CHECK.replace("{COERCETODOMAINVALUE :typeId 23 :typeMod -1 :collation 0 :location -1}", "{VAR :varno 1 :varattno 1 :vartype 23 :vartypmod -1 :varcollid 0 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 1 :location -1}"));
    let text = context
        .check(table.columns[1].check.as_ref().unwrap())
        .unwrap()
        .to_string();
    let expected = "{OPEXPR :opno 531 :opfuncid 157 :opresulttype 16 :opretset false :opcollid 0 :inputcollid 100 :args ({VAR :varno 1 :varattno 2 :vartype 25 :vartypmod -1 :varcollid 100 :varnullingrels (b) :varlevelsup 0 :varreturningtype 0 :varnosyn 1 :varattnosyn 2 :location -1} {CONST :consttype 25 :consttypmod -1 :constcollid 100 :constlen -1 :constbyval false :constisnull false :location -1 :constvalue 4 [ 16 0 0 0 ]}) :location -1}";
    assert_eq!(text, expected);
    let tree = parse(&text).unwrap();
    assert_eq!(
        deparse::expression(&tree, &Routines, false).unwrap(),
        "(txt <> ''::text)"
    );
    let tree = parse(&integer).unwrap();
    assert_eq!(
        deparse::expression(&tree, &Routines, false).unwrap(),
        "(\"Mixed Case\" > 0)"
    );
}

#[test]
fn column_nodes_use_bound_qualifiers_and_reject_unrelated_relations() {
    let schema = crate::RowSchema::with_qualified_types(
        "source",
        vec!["flag".into()],
        vec![Some(crate::ColumnType::Boolean)],
    );
    let context = expressions::ExpressionContext {
        schema: &schema,
        domain_value: None,
        types: None,
        routines: &Routines,
    };
    let actual = context
        .check(&crate::ast::Expr::qualified_column("source", "flag"))
        .unwrap();
    assert_eq!(actual.kind, "VAR");
    assert_eq!(actual.integer("varattno").unwrap(), 1);
    assert!(context
        .check(&crate::ast::Expr::qualified_column("unrelated", "flag"))
        .is_err());
}

#[test]
fn rejects_invalid_numeric_headers_digits_and_varlena_lengths() {
    for bytes in [
        vec![],
        vec![0],
        vec![0, 0],
        vec![0, 0xe0],
        vec![0, 0xc0, 0, 0],
        vec![0, 0x80, 0x10, 0x27],
    ] {
        assert!(values::numeric::decode(&bytes).is_err(), "{bytes:?}");
    }
    assert!(values::varlena_payload(4, &[17, 0, 0, 0]).is_err());
    assert!(values::varlena_payload(5, &[16, 0, 0, 0, 0]).is_err());
}

#[test]
fn rejects_unknown_function_format_tags() {
    let argument = values::constant(&uqa_core::Value::Int(1), &crate::ColumnType::Integer).unwrap();
    let expression = Node::new(
        "FUNCEXPR",
        [
            ("funcformat", 99.into()),
            ("funcresulttype", 23.into()),
            ("args", Field::List(vec![argument.into()])),
        ],
    );
    assert_eq!(
        deparse::expression(&expression.into(), &Routines, false)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
}
