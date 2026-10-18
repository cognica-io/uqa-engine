//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The engine against an independent `PostgreSQL` 18.4 reference.
//!
//! `tests/fixtures/regex_oracle.input.json` lists each case's inputs and SQL; `regex_oracle.expected.json` is that SQL's result from `PostgreSQL` 18.4, captured with `tests/parity/pg18/capture_command_completion_oracle.py --rows --details --notices` in a disposable database. The SQL calls `regexp_match`, `regexp_instr`, `regexp_like` and the match operators, so each case is reproduced here with the compile flags, search start and match slots that `src/backend/utils/adt/regexp.c` passes to `pg_regcomp` and `pg_regexec` for those functions. The reference server runs with its default `max_stack_depth` of 2 MB, the engine stack budget the crate applies.

use serde_json::{json, Value};
use uqa_pg_regex::{
    CollationCtype, CollationLocale, MatchSpan, Regex, RegexCollation, RegexError, REG_ADVANCED,
    REG_ICASE, REG_NOSUB,
};

use crate::support::{code_points, collation, parse_re_flags, text, Budget};

/// The reference is compiled into the test binary, so archived test runs need no source tree.
const INPUT: &str = include_str!("fixtures/regex_oracle.input.json");
const EXPECTED: &str = include_str!("fixtures/regex_oracle.expected.json");

fn read_fixture(source: &str) -> Value {
    serde_json::from_str(source).expect("the fixture is JSON")
}

/// Every reference case with its `PostgreSQL` result, after checking that the capture belongs to these inputs.
fn cases() -> Vec<(Value, Value)> {
    let input = read_fixture(INPUT);
    let expected = read_fixture(EXPECTED);
    let version = expected["postgresql_version"].as_str().unwrap();
    assert!(version.starts_with("PostgreSQL 18.4 "), "{version}");
    let inputs = input["cases"].as_array().unwrap();
    let results = expected["cases"].as_array().unwrap();
    assert_eq!(inputs.len(), results.len());
    assert_eq!(inputs[0]["sql"], "SELECT version()");
    for (case, result) in inputs.iter().zip(results) {
        assert_eq!(case["sql"], result["sql"]);
    }
    inputs[1..]
        .iter()
        .cloned()
        .zip(results[1..].iter().cloned())
        .collect()
}

fn field<'a>(case: &'a Value, name: &str) -> &'a str {
    case[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case} has no {name}"))
}

fn literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

fn subject_sql(case: &Value) -> String {
    format!(
        "{} COLLATE \"{}\"",
        literal(field(case, "subject")),
        field(case, "collation")
    )
}

/// The SQL a case must carry, so that the inputs below are the ones `PostgreSQL` evaluated.
fn rendered_sql(case: &Value) -> String {
    match field(case, "kind") {
        "match" => {
            let subject = subject_sql(case);
            let pattern = literal(field(case, "pattern"));
            let flags = literal(field(case, "flags"));
            let start = case["start"].as_u64().unwrap();
            let probes = field(case, "pattern").matches('(').count();
            let spans: Vec<String> = (0..=probes)
                .flat_map(|subexpr| {
                    [0, 1].map(|end| {
                        format!(
                            "regexp_instr({subject}, {pattern}, {start}, 1, {end}, {flags}, {subexpr})"
                        )
                    })
                })
                .collect();
            format!(
                "SELECT json_build_object('groups', regexp_match({subject}, {pattern}, {flags}), 'spans', ARRAY[{}])::text",
                spans.join(", ")
            )
        }
        "operator" => format!(
            "SELECT {} {} {}",
            subject_sql(case),
            field(case, "operator"),
            literal(field(case, "pattern"))
        ),
        "members" => format!(
            "SELECT string_agg(to_hex(lo) || '-' || to_hex(hi), ',' ORDER BY lo) FROM (SELECT min(cp) AS lo, max(cp) AS hi FROM (SELECT cp, cp - row_number() OVER (ORDER BY cp) AS run FROM generate_series(1, 1114111) AS cp WHERE (cp < 55296 OR cp > 57343) AND regexp_like(chr(cp) COLLATE \"{}\", {}, {})) AS members GROUP BY run) AS runs",
            field(case, "collation"),
            literal(field(case, "pattern")),
            literal(field(case, "flags"))
        ),
        "repeated" => format!(
            "SELECT {} ~ (repeat({}, {count}) || {} || repeat({}, {count}))",
            subject_sql(case),
            literal(field(case, "opening")),
            literal(field(case, "core")),
            literal(field(case, "closing")),
            count = case["count"].as_u64().unwrap()
        ),
        other => panic!("unknown case kind {other}"),
    }
}

/// The pattern of a `repeated` case: `count` openings, the core and `count` closings.
#[cfg(not(target_family = "wasm"))]
fn repeated_pattern(case: &Value) -> String {
    let count = usize::try_from(case["count"].as_u64().unwrap()).unwrap();
    format!(
        "{}{}{}",
        field(case, "opening").repeat(count),
        field(case, "core"),
        field(case, "closing").repeat(count)
    )
}

/// A statement failure as `PostgreSQL` reports it.
#[derive(Debug, PartialEq)]
struct Failure {
    sqlstate: String,
    message: String,
}

/// `RE_compile_and_cache`'s report of a compilation failure.
fn compile_failure(error: RegexError) -> Failure {
    assert!(matches!(error, RegexError::Code(_)), "{error:?}");
    Failure {
        sqlstate: error.sqlstate().unwrap().into(),
        message: format!("invalid regular expression: {}", error.message()),
    }
}

/// `RE_wchar_execute`'s report of an execution failure.
fn execution_failure(error: RegexError) -> Failure {
    assert!(matches!(error, RegexError::Code(_)), "{error:?}");
    Failure {
        sqlstate: error.sqlstate().unwrap().into(),
        message: format!("regular expression failed: {}", error.message()),
    }
}

/// `regexp_match(subject, pattern, flags)`: one search from the start with every subexpression slot.
fn regexp_match(
    subject: &[u32],
    pattern: &str,
    flags: &str,
    collation: RegexCollation,
) -> Result<Value, Failure> {
    let mut budget = Budget::unlimited();
    let mut regex = Regex::compile(pattern, parse_re_flags(flags), collation, &mut budget)
        .map_err(compile_failure)?;
    let subexpressions = regex.subexpression_count();
    let mut spans = vec![MatchSpan::UNSET; subexpressions + 1];
    if !regex
        .exec(subject, 0, &mut spans)
        .map_err(execution_failure)?
    {
        return Ok(Value::Null);
    }
    let substring = |span: &MatchSpan| {
        span.range()
            .map_or(Value::Null, |range| Value::String(text(&subject[range])))
    };
    Ok(if subexpressions > 0 {
        Value::Array(spans[1..].iter().map(substring).collect())
    } else {
        Value::Array(vec![substring(&spans[0])])
    })
}

/// `regexp_instr(subject, pattern, start, 1, endoption, flags, subexpr)` for the first match.
fn regexp_instr(
    subject: &[u32],
    pattern: &str,
    start: usize,
    endoption: usize,
    flags: &str,
    subexpr: usize,
    collation: RegexCollation,
) -> Result<usize, Failure> {
    let mut compile_flags = parse_re_flags(flags);
    if subexpr == 0 {
        compile_flags |= REG_NOSUB;
    }
    let mut budget = Budget::unlimited();
    let mut regex =
        Regex::compile(pattern, compile_flags, collation, &mut budget).map_err(compile_failure)?;
    let subexpressions = regex.subexpression_count();
    let use_subpatterns = subexpr > 0 && subexpressions > 0;
    let patterns = if use_subpatterns { subexpressions } else { 1 };
    let mut spans = vec![
        MatchSpan::UNSET;
        if use_subpatterns {
            subexpressions + 1
        } else {
            1
        }
    ];
    if !regex
        .exec(subject, start - 1, &mut spans)
        .map_err(execution_failure)?
    {
        return Ok(0);
    }
    if subexpr > patterns {
        return Ok(0);
    }
    let span = if use_subpatterns {
        spans[subexpr]
    } else {
        spans[0]
    };
    let position = if endoption == 0 {
        span.start()
    } else {
        span.end()
    };
    Ok(position.map_or(0, |position| position + 1))
}

fn evaluate_match(case: &Value) -> Result<Value, Failure> {
    let subject = code_points(field(case, "subject"));
    let pattern = field(case, "pattern");
    let flags = field(case, "flags");
    let collation = collation(field(case, "collation"));
    let start = usize::try_from(case["start"].as_u64().unwrap()).unwrap();
    let groups = regexp_match(&subject, pattern, flags, collation)?;
    let mut spans = Vec::new();
    for subexpr in 0..=pattern.matches('(').count() {
        for endoption in [0, 1] {
            spans.push(regexp_instr(
                &subject, pattern, start, endoption, flags, subexpr, collation,
            )?);
        }
    }
    Ok(json!({ "groups": groups, "spans": spans }))
}

/// `textregexeq` and its relatives: `RE_compile_and_execute` with no match slots, hence `REG_NOSUB`.
fn evaluate_operator(case: &Value) -> Result<&'static str, Failure> {
    operator_result(case, field(case, "operator"), field(case, "pattern"))
}

/// `subject operator pattern` for a case's subject and collation, with the hooks applying no limit of their own.
fn operator_result(case: &Value, operator: &str, pattern: &str) -> Result<&'static str, Failure> {
    let (flags, negated) = match operator {
        "~" => (REG_ADVANCED, false),
        "~*" => (REG_ADVANCED | REG_ICASE, false),
        "!~" => (REG_ADVANCED, true),
        "!~*" => (REG_ADVANCED | REG_ICASE, true),
        other => panic!("unknown operator {other}"),
    };
    let mut budget = Budget::unlimited();
    let mut regex = Regex::compile(
        pattern,
        flags | REG_NOSUB,
        collation(field(case, "collation")),
        &mut budget,
    )
    .map_err(compile_failure)?;
    let matched = regex
        .exec(&code_points(field(case, "subject")), 0, &mut [])
        .map_err(execution_failure)?;
    Ok(if matched == negated { "f" } else { "t" })
}

/// The code points `regexp_like(chr(cp), pattern, flags)` accepts, as hexadecimal runs. Each class pattern matches exactly one code point, so successive searches over all code points visit every member once.
fn class_members(case: &Value) -> Option<String> {
    let mut budget = Budget::unlimited();
    let mut regex = Regex::compile(
        field(case, "pattern"),
        parse_re_flags(field(case, "flags")) | REG_NOSUB,
        collation(field(case, "collation")),
        &mut budget,
    )
    .expect("class patterns compile");
    let mut runs: Vec<(u32, u32)> = Vec::new();
    for (first, last) in [(1u32, 0xD7FF), (0xE000, 0x10_FFFF)] {
        let input: Vec<u32> = (first..=last).collect();
        let mut spans = [MatchSpan::UNSET];
        let mut start = 0;
        while start < input.len() && regex.exec(&input, start, &mut spans).unwrap() {
            let range = spans[0].range().unwrap();
            assert_eq!(range.len(), 1, "{}", field(case, "name"));
            let member = input[range.start];
            match runs.last_mut() {
                Some((_, high)) if *high + 1 == member => *high = member,
                _ => runs.push((member, member)),
            }
            start = range.end;
        }
    }
    drop(regex);
    assert_eq!(budget.live, 0);
    (!runs.is_empty()).then(|| {
        runs.iter()
            .map(|(low, high)| format!("{low:x}-{high:x}"))
            .collect::<Vec<_>>()
            .join(",")
    })
}

fn expected_failure(result: &Value) -> Option<Failure> {
    let error = &result["error"];
    (!error.is_null()).then(|| Failure {
        sqlstate: error["sqlstate"].as_str().unwrap().into(),
        message: error["message"].as_str().unwrap().into(),
    })
}

fn expected_value(result: &Value) -> Option<&str> {
    let rows = result["results"][0]["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    rows[0][0].as_str()
}

#[test]
fn reference_sql_matches_the_recorded_inputs() {
    let cases = cases();
    let mut rendered = 0;
    for (case, _) in &cases {
        // Collation errors and their setup are written as SQL directly.
        if matches!(field(case, "kind"), "collation" | "setup") {
            continue;
        }
        assert_eq!(
            field(case, "sql"),
            rendered_sql(case),
            "{}",
            field(case, "name")
        );
        rendered += 1;
    }
    assert_eq!(rendered, 277);
}

#[test]
fn collation_selection_errors_equal_postgresql() {
    let mut compared = 0;
    for (case, result) in cases() {
        match field(&case, "kind") {
            "setup" => assert!(expected_failure(&result).is_none()),
            "collation" => {
                let ctype = match field(&case, "ctype") {
                    "c" => CollationCtype::C,
                    "icu" => CollationCtype::Icu,
                    other => panic!("unknown ctype {other}"),
                };
                let locale = CollationLocale {
                    deterministic: case["deterministic"].as_bool().unwrap(),
                    ctype,
                };
                let oid = u32::try_from(case["oid"].as_u64().unwrap()).unwrap();
                let error = RegexCollation::from_locale(oid, locale).unwrap_err();
                let expected = &result["error"];
                assert_eq!(error.sqlstate(), expected["sqlstate"].as_str());
                assert_eq!(
                    error.message().as_str(),
                    expected["message"].as_str().unwrap()
                );
                assert_eq!(error.hint(), expected["hint"].as_str());
                compared += 1;
            }
            _ => {}
        }
    }
    assert_eq!(compared, 2);
}

#[test]
fn matches_spans_and_diagnostics_equal_postgresql() {
    let mut compared = 0;
    for (case, result) in cases() {
        let name = field(&case, "name");
        match field(&case, "kind") {
            "match" => {
                match (evaluate_match(&case), expected_failure(&result)) {
                    (Err(failure), Some(expected)) => assert_eq!(failure, expected, "{name}"),
                    (Ok(value), None) => {
                        let expected: Value =
                            serde_json::from_str(expected_value(&result).unwrap()).unwrap();
                        assert_eq!(value, expected, "{name}");
                    }
                    (actual, expected) => {
                        panic!("{name}: engine {actual:?}, PostgreSQL {expected:?}")
                    }
                }
                compared += 1;
            }
            "operator" => {
                assert!(expected_failure(&result).is_none(), "{name}");
                let actual = evaluate_operator(&case).unwrap();
                assert_eq!(Some(actual), expected_value(&result), "{name}");
                compared += 1;
            }
            _ => {}
        }
    }
    assert_eq!(compared, 207);
}

#[test]
fn class_members_over_every_code_point_equal_postgresql() {
    let cases: Vec<(Value, Value)> = cases()
        .into_iter()
        .filter(|(case, _)| field(case, "kind") == "members")
        .collect();
    assert_eq!(cases.len(), 64);
    let verify = |group: &[(Value, Value)]| {
        for (case, result) in group {
            assert!(expected_failure(result).is_none());
            assert_eq!(
                class_members(case).as_deref(),
                expected_value(result),
                "{}",
                field(case, "name")
            );
        }
    };
    #[cfg(target_family = "wasm")]
    verify(&cases);
    #[cfg(not(target_family = "wasm"))]
    {
        let threads = std::thread::available_parallelism().map_or(4, usize::from);
        let chunk = cases.len().div_ceil(threads);
        // Native collations also exercise independent per-thread locale state.
        std::thread::scope(|scope| {
            for group in cases.chunks(chunk) {
                scope.spawn(move || verify(group));
            }
        });
    }
}

/// Patterns whose recursion approaches `PostgreSQL`'s default `max_stack_depth` of 2 MB, which `PostgreSQL` accepts below it and rejects beyond it as too complex. The hooks apply no limit, so the engine's own budget decides. WebAssembly runs the engine on a call stack that these depths would exhaust, so the comparison runs on native targets only.
#[cfg(not(target_family = "wasm"))]
#[test]
fn stack_depth_limits_equal_postgresql() {
    let mut compared = 0;
    for (case, result) in cases() {
        if field(&case, "kind") != "repeated" {
            continue;
        }
        let name = field(&case, "name");
        match (
            operator_result(&case, "~", &repeated_pattern(&case)),
            expected_failure(&result),
        ) {
            (Err(failure), Some(expected)) => assert_eq!(failure, expected, "{name}"),
            (Ok(value), None) => assert_eq!(Some(value), expected_value(&result), "{name}"),
            (actual, expected) => panic!("{name}: engine {actual:?}, PostgreSQL {expected:?}"),
        }
        compared += 1;
    }
    assert_eq!(compared, 6);
}
