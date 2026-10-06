//! PL/pgSQL metadata required by the execution-time preparation boundary.
use super::*;

fn compile(source: &str, mode: PlpgsqlCompileMode) -> crate::ParseOutcome<serde_json::Value> {
    parse_plpgsql_with_mode(
        source,
        None,
        crate::ParseOptions {
            standard_conforming_strings: false,
            ..crate::ParseOptions::default()
        },
        mode,
    )
}

#[test]
fn validator_and_runtime_compile_use_postgresqls_distinct_syntax_checks() {
    let source =
        r"CREATE FUNCTION f() RETURNS text LANGUAGE plpgsql AS $$BEGIN RETURN 'a\nb'; END$$";
    let validation = compile(source, PlpgsqlCompileMode::Validate);
    let runtime = compile(source, PlpgsqlCompileMode::Runtime);
    // PostgreSQL emits one warning while scanning the body and another in the
    // validator's syntax check. Runtime preparation emits the latter separately.
    assert_eq!(validation.diagnostics.len(), 2);
    assert_eq!(runtime.diagnostics.len(), 1);
    assert_eq!(validation.result.unwrap(), runtime.result.unwrap());
}

#[test]
fn block_initializers_retain_exact_datum_indices() {
    let parsed = compile(
        "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$DECLARE x int:=1; BEGIN DECLARE x text:='nested'; BEGIN NULL; END; END$$",
        PlpgsqlCompileMode::Validate,
    )
    .result
    .unwrap();
    let function = &parsed[0]["PLpgSQL_function"];
    let datums = function["datums"].as_array().unwrap();
    let block = &function["action"]["PLpgSQL_stmt_block"];
    let outer = block["initvarnos"].as_array().unwrap();
    let inner = block["body"][0]["PLpgSQL_stmt_block"]["initvarnos"]
        .as_array()
        .unwrap();
    assert_eq!(outer.len(), 1);
    assert_eq!(inner.len(), 1);
    assert_ne!(outer, inner);
    for index in [outer[0].as_u64().unwrap(), inner[0].as_u64().unwrap()] {
        assert_eq!(datums[index as usize]["PLpgSQL_var"]["refname"], "x");
    }
}

#[test]
fn runtime_does_not_validate_unreached_sql_but_keeps_its_text_and_mode() {
    let source = "CREATE FUNCTION f() RETURNS void LANGUAGE plpgsql AS $$BEGIN IF false THEN PERFORM 1+; END IF; END$$";
    assert!(compile(source, PlpgsqlCompileMode::Validate)
        .result
        .is_err());
    let runtime = compile(source, PlpgsqlCompileMode::Runtime).result.unwrap();
    let expression = &runtime[0]["PLpgSQL_function"]["action"]["PLpgSQL_stmt_block"]["body"][0]
        ["PLpgSQL_stmt_if"]["then_body"][0]["PLpgSQL_stmt_perform"]["expr"]["PLpgSQL_expr"];
    assert_eq!(expression["query"], "SELECT 1+");
    assert_eq!(expression["parseMode"], 0);
}

#[test]
fn runtime_compilation_requires_concrete_types_and_preserves_them() {
    let source = "CREATE FUNCTION f(x anyelement) RETURNS anyelement LANGUAGE plpgsql AS $$BEGIN RETURN x; END$$";
    assert!(compile(source, PlpgsqlCompileMode::Validate).result.is_ok());
    let error = compile(source, PlpgsqlCompileMode::Runtime)
        .result
        .unwrap_err();
    let crate::Error::ParseDiagnostic(error) = error else {
        panic!("expected PostgreSQL diagnostic");
    };
    assert_eq!(error.sqlstate, "0A000");
    assert_eq!(
        error.message,
        "could not determine actual argument type for polymorphic function \"f\""
    );
    for (declaration, expected) in [("integer", "int4"), ("text", "text")] {
        let specialized = source.replace("anyelement", declaration);
        let parsed = compile(&specialized, PlpgsqlCompileMode::Runtime)
            .result
            .unwrap();
        assert_eq!(
            parsed[0]["PLpgSQL_function"]["datums"][0]["PLpgSQL_var"]["datatype"]["PLpgSQL_type"]
                ["typname"],
            expected
        );
    }
}
