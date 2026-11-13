//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AclRoleSpecification, CreateFunction, RoleSpecification, Statement};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::{json, Value};
use uqa_core::{memory::MemoryBudget, CancellationToken};

const STATEMENTS: &[&str] = &[
    "CREATE DOMAIN d AS integer DEFAULT 1 CHECK (VALUE > 0)",
    "ALTER DOMAIN d SET DEFAULT 2", "CREATE TYPE shell", "COMMENT ON TYPE shell IS 'x'",
    "CREATE TYPE mood AS ENUM ('a', 'b')", "CREATE TYPE pair AS (a int, b text)",
    "ALTER TYPE mood ADD VALUE 'c'", "ALTER TYPE pair RENAME TO pair2", "GRANT USAGE ON TYPE mood TO PUBLIC",
    "CREATE TABLE t (a integer GENERATED ALWAYS AS IDENTITY, b text DEFAULT 'x', CHECK (length(b) > 0), UNIQUE (a))",
    "CREATE TABLE IF NOT EXISTS t (a serial PRIMARY KEY, b int REFERENCES u(a), c int GENERATED ALWAYS AS (a + 1) STORED)",
    "CREATE INDEX idx ON t (a DESC NULLS FIRST) INCLUDE (b) WHERE a > 0", "ALTER INDEX idx RENAME TO idx2",
    "INSERT INTO t VALUES (1, 'x') RETURNING *", "SELECT sum(a) OVER () FROM t", "UPDATE t SET a = 2", "DELETE FROM t",
    "DROP TABLE t CASCADE", "ALTER TABLE t ADD COLUMN b int DEFAULT 2", "ALTER FOREIGN TABLE ft OWNER TO CURRENT_USER",
    "ALTER VIEW v RENAME TO v2", "CREATE VIEW v AS SELECT 1 AS a", "CREATE MATERIALIZED VIEW mv AS SELECT 1 WITH NO DATA",
    "REFRESH MATERIALIZED VIEW mv", "CREATE SCHEMA s AUTHORIZATION CURRENT_USER", "ALTER SCHEMA s OWNER TO SESSION_USER", "ALTER SCHEMA s RENAME TO s2",
    "NOTIFY channel, 'payload'", "LISTEN channel", "UNLISTEN *", "SET LOCAL search_path = public", "RESET search_path", "RESET ALL",
    "SET CONSTRAINTS ALL DEFERRED", "SHOW search_path", "DISCARD ALL", "LOAD 'module'", "EXPLAIN (ANALYZE, FORMAT JSON) SELECT 1",
    "ANALYZE t", "VACUUM (ANALYZE, VERBOSE) t(a)", "LOCK TABLE t IN ACCESS SHARE MODE", "TRUNCATE t RESTART IDENTITY CASCADE",
    "BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY", "DECLARE c SCROLL CURSOR FOR SELECT 1", "FETCH FORWARD 2 FROM c", "CLOSE c",
    "CREATE SEQUENCE seq AS bigint INCREMENT BY 2 CACHE 3", "ALTER SEQUENCE seq RESTART WITH 8",
    "CREATE TABLE t AS SELECT 1 AS a", "PREPARE p(int) AS SELECT $1", "EXECUTE p(2)", "DEALLOCATE p", "VALUES (1, 'x'), (2, 'y')",
    "CREATE FOREIGN DATA WRAPPER memory", "CREATE SERVER srv FOREIGN DATA WRAPPER memory OPTIONS (type 'memory')",
    "CREATE FOREIGN TABLE ft (a int) SERVER srv", "CREATE FOREIGN TABLE IF NOT EXISTS ft (a int) SERVER srv",
    "MERGE INTO t USING u ON t.a = u.a WHEN MATCHED THEN DELETE",
    "CREATE FUNCTION f(a int DEFAULT 1) RETURNS int LANGUAGE SQL IMMUTABLE STRICT AS 'SELECT a'",
    "CREATE FUNCTION f() RETURNS int LANGUAGE SQL BEGIN ATOMIC SELECT 1; END", "CREATE PROCEDURE p() LANGUAGE SQL AS 'SELECT 1'",
    "DROP FUNCTION f(int)", "ALTER FUNCTION f(int) VOLATILE", "ALTER FUNCTION f(int) OWNER TO CURRENT_USER", "ALTER FUNCTION f(int) RENAME TO g",
    "GRANT EXECUTE ON FUNCTION f(int) TO PUBLIC", "GRANT SELECT(a) ON t TO PUBLIC", "GRANT USAGE ON SEQUENCE seq TO PUBLIC",
    "GRANT CONNECT ON DATABASE db TO PUBLIC", "GRANT USAGE ON SCHEMA s TO PUBLIC", "GRANT reader TO writer WITH ADMIN OPTION",
    "CREATE ROLE reader LOGIN CREATEDB", "ALTER ROLE reader NOLOGIN", "ALTER ROLE reader RENAME TO r2", "DROP ROLE reader",
    "CREATE TRIGGER tr AFTER INSERT ON t FOR EACH ROW EXECUTE FUNCTION f()", "DROP TRIGGER tr ON t",
    "CREATE RULE r AS ON INSERT TO t DO ALSO NOTIFY channel", "DROP RULE r ON t", "DO $$BEGIN NULL; END$$", "CALL p()", ";",
];

fn compare<T: Stored + DeserializeOwned + Serialize + std::fmt::Debug>(text: &str) -> bool {
    let expected = serde_json::from_str::<T>(text);
    let memory = MemoryBudget::new(64 << 20);
    let token = CancellationToken::new();
    let values = Values {
        memory: &memory,
        original: &token,
        invoking: &token,
    };
    let actual = (|| -> Result<uqa_core::memory::Produced<T>, InputError> {
        let mut input = Input::new(text, values.control());
        let token = input.next()?;
        let result = input.stored::<T>(token, &values)?;
        input.end()?;
        Ok(result.into())
    })();
    assert_eq!(
        actual.is_ok(),
        expected.is_ok(),
        "{text}: {actual:?} / {expected:?}"
    );
    let valid = expected.is_ok();
    if let (Ok(actual), Ok(expected)) = (actual, expected) {
        assert_eq!(
            serde_json::to_string(&*actual).unwrap(),
            serde_json::to_string(&expected).unwrap(),
            "{text}"
        );
    }
    assert_eq!(memory.used(), 0, "{text}");
    valid
}

#[test]
fn stored_statement_input_retains_queries_declarations_and_routine_bodies() {
    for sql in STATEMENTS {
        for statement in crate::compile(sql).unwrap_or_else(|error| panic!("{sql}: {error}")) {
            assert!(
                compare::<Statement>(&serde_json::to_string(&statement).unwrap()),
                "{sql}"
            );
        }
    }
}

#[test]
fn stored_statement_input_preserves_omissions_and_field_type_acceptance() {
    for sql in STATEMENTS {
        for statement in crate::compile(sql).unwrap() {
            let encoded = serde_json::to_value(statement).unwrap();
            super::super::tests::mutate(&encoded, &mut |value| {
                compare::<Statement>(&value.to_string());
            });
        }
    }
}

#[test]
fn stored_statement_input_preserves_tagged_and_legacy_roles() {
    for text in [
        r#""reader""#,
        r#""CURRENT_USER""#,
        r#""SESSION_USER""#,
        r#""PUBLIC""#,
        "null",
        "0",
        "[]",
        "{}",
        r#"{"kind":"named","name":"quoted"}"#,
        r#"{"kind":"current_user"}"#,
        r#"{"kind":"session_user","name":null}"#,
        r#"["current_user"]"#,
        r#"["current_user",null]"#,
        r#"["named","reader"]"#,
        r#"{"kind":0,"name":"reader"}"#,
        r#"{"kind":1}"#,
        r#"{"kind":"public"}"#,
        r#"["public",null]"#,
        r#"{"kind":"role","role":"reader"}"#,
        r#"{"kind":"role","role":{"kind":"named","name":"PUBLIC"}}"#,
        r#"{"name":"reader","kind":"named"}"#,
        r#"{"kind":"current_user","extra":null}"#,
        r#"{"kind":"public","role":"reader"}"#,
    ] {
        compare::<RoleSpecification>(text);
        compare::<AclRoleSpecification>(text);
    }
}

fn routine() -> Value {
    let Statement::CreateFunction(definition) =
        crate::compile("CREATE FUNCTION f() RETURNS int LANGUAGE SQL AS 'SELECT 1'")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    serde_json::to_value(definition).unwrap()
}

#[test]
fn stored_statement_input_preserves_flattened_authority_and_estimates() {
    for field in ["cost", "rows", "owner", "security_definer", "leakproof"] {
        for text in [
            "null",
            "0",
            "-0",
            "-1",
            "1.25",
            "1e3",
            "1e400",
            "18446744073709551616",
            r#""NaN""#,
            r#""Infinity""#,
            r#""-Infinity""#,
            r#""""#,
            r#""owner""#,
            "[]",
            "{}",
            "true",
            r#"{"oid":40000,"object_id":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16]}"#,
        ] {
            let mut value = routine();
            value.as_object_mut().unwrap().remove(field);
            let encoded = value.to_string();
            let text = format!("{},\"{field}\":{text}}}", &encoded[..encoded.len() - 1]);
            compare::<CreateFunction>(&text);
        }
    }
    let mut definition = routine();
    definition["body"] = json!({"Statements": []});
    for sql in STATEMENTS {
        for statement in crate::compile(sql).unwrap() {
            definition["body"]["Statements"] = json!([statement]);
            assert!(compare::<CreateFunction>(&definition.to_string()));
        }
    }
    for text in [
        r#"{"Explain":{"body":"Empty","analyze":true,"verbose":false,"format":"json"}}"#,
        r#"{"Explain":{"body":"Empty","options":[],"format":"ignored"}}"#,
        r#"{"Explain":{"body":"Empty","options":[],"format":3}}"#,
        r#"{"Explain":["Empty"]}"#,
        r#"{"Explain":{"body":"Empty","extra":"\ud800"}}"#,
    ] {
        compare::<Statement>(text);
    }
}

#[test]
fn stored_statement_input_releases_admitted_declarations_on_failure() {
    for sql in [
        STATEMENTS[9],
        STATEMENTS[37],
        STATEMENTS[58],
        "CREATE ROLE reader LOGIN CREATEDB",
    ] {
        let statement = crate::compile(sql).unwrap().remove(0);
        let encoded = serde_json::to_string(&statement).unwrap();
        let memory = MemoryBudget::new(64 << 20);
        let token = CancellationToken::new();
        let mut output = None;
        let measured = allocation_counter::measure(|| {
            output = Some(Statement::from_stored_json(&encoded, &memory, &token, &token).unwrap());
        });
        assert_eq!(
            usize::try_from(measured.bytes_current).unwrap(),
            memory.used(),
            "{sql}"
        );
        assert!(
            usize::try_from(measured.bytes_max).unwrap() <= memory.peak(),
            "{sql}"
        );
        let peak = memory.peak();
        drop(output);
        assert_eq!(memory.used(), 0);
        for limit in (0..peak).step_by((peak / 16).max(1)).chain([peak - 1]) {
            let memory = MemoryBudget::new(limit);
            assert_eq!(
                Statement::from_stored_json(&encoded, &memory, &token, &token)
                    .unwrap_err()
                    .sqlstate(),
                Some("53200")
            );
            assert_eq!(memory.used(), 0);
        }
    }
    let mut definition = routine();
    definition["body"] = json!({"Source": "s".repeat(65536)});
    let encoded = definition.to_string();
    for original_cancelled in [true, false] {
        let memory = MemoryBudget::new(64 << 20);
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let cancel = if original_cancelled {
            &original
        } else {
            &invoking
        };
        let result = crate::ast::stored_input::test_control::cancel_after_allocation(
            &memory,
            cancel,
            16384,
            || CreateFunction::from_stored_json(&encoded, &memory, &original, &invoking),
        );
        assert_eq!(result.unwrap_err().sqlstate(), Some("57014"));
        assert_eq!(memory.used(), 0);
    }
}

#[test]
fn stored_statement_input_preserves_positional_and_nested_statement_forms() {
    for text in [
        r#""Empty""#,
        r#"{"Empty":null}"#,
        r#"{"Values":[[]]}"#,
        r#"{"Values":[]}"#,
        r#"{"Notify":["channel","payload"]}"#,
        r#"{"Notify":{"channel":"a","channel":"b","payload":"x"}}"#,
        r#"{"CreateForeignTableIfNotExists":{"name":"t","server_name":"s","definition_sql":"CREATE FOREIGN TABLE t(a int) SERVER s"}}"#,
        r#"{"AlterSequence":{"name":"s","restart":-0}}"#,
        r#"{"AlterSequence":{"name":"s","restart":{"With":9}}}"#,
        r#"{"AlterForeignTable":{"name":"t","if_exists":false,"drop_columns":[]}}"#,
        r#"{"AlterForeignTable":{"name":"t","if_exists":false,"owner":"r","rename_to":"u"}}"#,
    ] {
        compare::<Statement>(text);
    }
    let mut nested = r#""Empty""#.to_owned();
    let mut accepted = 0;
    for _ in 0..70 {
        accepted += usize::from(compare::<Statement>(&nested));
        nested = format!(r#"{{"Explain":{{"body":{nested}}}}}"#);
    }
    assert!(accepted > 50 && accepted < 70);
    assert!(!compare::<CreateFunction>("[]"));
}
