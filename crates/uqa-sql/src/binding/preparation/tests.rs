//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, FunctionTypeResolver, RelationIdentity};
use std::collections::{BTreeMap, BTreeSet};

mod body_inputs;
mod defaults;
mod dependencies;

struct NoRoutines;
impl FunctionTypeResolver for NoRoutines {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl RoutineResolution for NoRoutines {}

#[test]
fn analyzer_table_functions_infer_text_parameters_in_queries_and_insert_sources() {
    let crate::Statement::CreateTable(table) =
        crate::compile("CREATE TABLE diagnostic_snapshot (analysis JSONB)")
            .unwrap()
            .remove(0)
    else {
        unreachable!()
    };
    let context = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "diagnostic_snapshot"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        resolution: crate::binding::fixture::resolution(
            vec!["public".into()],
            "pg_temp_fixture".into(),
        ),
        ctes: BTreeMap::new(),
        deferred_ctes: BTreeMap::new(),
        non_returning_ctes: BTreeSet::new(),
        scalar_subqueries: &[],
    };
    for (sql, count) in [
        ("SELECT * FROM analyze_text($1, $2)", 2),
        (
            "INSERT INTO diagnostic_snapshot SELECT analysis FROM analyze_text($1, $2)",
            2,
        ),
        ("SELECT * FROM create_analyzer($1, $2)", 2),
        ("SELECT * FROM drop_analyzer($1)", 1),
        ("SELECT * FROM set_table_analyzer($1, $2, $3)", 3),
        ("SELECT * FROM set_table_analyzer($1, $2, $3, $4)", 4),
        ("SELECT * FROM fts_index_stats($1)", 1),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let types =
            infer_prepared_parameter_types(&NoRoutines, &plan, &vec![None; count], &context)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(types, vec![Some(ColumnType::Text); count], "{sql}");
    }
}

fn assignment_context() -> BindingContext<'static> {
    let crate::Statement::CreateTable(table) = crate::compile(
        "CREATE TABLE assignment_target (id integer PRIMARY KEY, value integer[], legacy oidvector)",
    ).unwrap().remove(0) else { unreachable!() };
    BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "assignment_target"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        resolution: crate::binding::fixture::resolution(
            vec!["public".into()],
            "pg_temp_fixture".into(),
        ),
        ctes: BTreeMap::new(),
        deferred_ctes: BTreeMap::new(),
        non_returning_ctes: BTreeSet::new(),
        scalar_subqueries: &[],
    }
}

#[test]
fn text_match_query_parameters_infer_text_across_read_and_mutation_commands() {
    for sql in [
        "SELECT id FROM assignment_target WHERE text_match(_all,$1)",
        "UPDATE assignment_target SET id=id+1 WHERE text_match(_all,$1)",
        "DELETE FROM assignment_target WHERE text_match(_all,$1)",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let types =
            infer_prepared_parameter_types(&NoRoutines, &plan, &[None], &assignment_context())
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(types, vec![Some(ColumnType::Text)], "{sql}");
    }
}

#[test]
fn subscript_assignment_preparation_infers_bounds_and_element_or_slice_parameters() {
    for (sql, expected) in [
        ("UPDATE assignment_target SET value[$1]=$2", vec![ColumnType::Integer, ColumnType::Integer]),
        ("UPDATE assignment_target SET value[$1:$2]=$3", vec![ColumnType::Integer, ColumnType::Integer, ColumnType::Array(Box::new(ColumnType::Integer))]),
        ("INSERT INTO assignment_target (value[$1]) VALUES ($2)", vec![ColumnType::Integer, ColumnType::Integer]),
        ("INSERT INTO assignment_target (value[$1]) SELECT $2", vec![ColumnType::Integer, ColumnType::Integer]),
        ("INSERT INTO assignment_target (id) VALUES (1) ON CONFLICT(id) DO UPDATE SET value[$1]=$2", vec![ColumnType::Integer, ColumnType::Integer]),
        ("MERGE INTO assignment_target USING (VALUES (1)) AS s(id) ON true WHEN MATCHED THEN UPDATE SET value[$1]=$2", vec![ColumnType::Integer, ColumnType::Integer]),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let result = infer_prepared_parameter_types(&NoRoutines, &plan, &vec![None; expected.len()], &assignment_context())
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(result, expected.into_iter().map(Some).collect::<Vec<_>>(), "{sql}");
    }
}

#[test]
fn invalid_partial_assignments_fail_during_preparation_even_without_input_rows() {
    for (sql, code, message) in [
        ("UPDATE assignment_target SET value[2]=ARRAY[9] WHERE false", "42804", "subscripted assignment to \"value\" requires type integer but expression is of type integer[]"),
        ("UPDATE assignment_target SET value[2:3]=9 WHERE false", "42804", "subscripted assignment to \"value\" requires type integer[] but expression is of type integer"),
        ("UPDATE assignment_target SET value[true]=9 WHERE false", "42804", "array subscript must have type integer"),
        ("UPDATE assignment_target SET value['bad']=9 WHERE false", "22P02", "invalid input syntax for type integer: \"bad\""),
        ("UPDATE assignment_target SET legacy[0]=9 WHERE false", "42846", "cannot cast type oid[] to oidvector"),
        ("UPDATE assignment_target SET value[1]=DEFAULT WHERE false", "0A000", "cannot set an array element to DEFAULT"),
        ("UPDATE assignment_target SET value=ARRAY[1],value[2]=9 WHERE false", "42601", "multiple assignments to same column \"value\""),
        ("INSERT INTO assignment_target (value,value[2]) VALUES (ARRAY[1],9)", "42701", "column \"value\" specified more than once"),
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let error = infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context()).unwrap_err();
        assert_eq!(error.sqlstate(), Some(code), "{sql}: {error}");
        assert_eq!(error.to_string(), message, "{sql}");
        if message.starts_with("subscripted assignment") {
            assert!(matches!(error, SQLError::Diagnostic { hint: Some(ref hint), .. } if hint == "You will need to rewrite or cast the expression."));
        }
    }
}

#[test]
fn partial_targets_can_repeat_without_changing_original_row_expression_binding() {
    for sql in [
        "UPDATE assignment_target SET value[1]=value[2],value[2]=value[1]",
        "INSERT INTO assignment_target (value[1],value[3]) VALUES (7,9)",
    ] {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        infer_prepared_parameter_types(&NoRoutines, &plan, &[], &assignment_context()).unwrap();
    }
}

#[test]
fn prepared_input_constants_keep_their_original_sites_and_selected_types() {
    let clock = 90_123_456_789;
    let _clock = crate::expr::TransactionClockScope::enter(clock);
    for (sql, expected) in [
        ("SELECT 'now'::timestamp, 'now'::text::timestamp", 1),
        ("SELECT now() = 'now', now() IN ('now'), now() BETWEEN 'now' AND 'now', CASE WHEN true THEN 'now' ELSE now() END, ARRAY['now', now()], COALESCE('now', now())", 7),
        ("WITH RECURSIVE second AS (SELECT * FROM first), first AS (SELECT 'now'::timestamp AS frozen, 'now'::text AS live) SELECT * FROM second", 1),
        ("SELECT (SELECT 'now'::timestamp), (SELECT 'now'::text)::timestamp", 1),
        ("SELECT 'now' UNION SELECT now()", 1),
        ("SELECT 'now'::timestamp AS frozen GROUP BY frozen", 1),
        ("SELECT count(*) GROUP BY 'now'::timestamp", 1),
        ("WITH RECURSIVE t(n) AS (VALUES (1) UNION ALL SELECT n+1 FROM t WHERE n<2) CYCLE n SET c TO 'yes' DEFAULT 'no' USING p SELECT n,c FROM t", 0),
        ("SELECT * FROM ROWS FROM (generate_series('now'::timestamp, 'now'::timestamp, '1 day'::interval)) AS series(t)", 3),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        read_prepared_inputs(&NoRoutines, &mut plan, &[], &assignment_context(), None)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        let mut constants = Vec::new();
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::TypedLiteral {
                value: uqa_core::Value::Temporal(value),
                ..
            } = expression
            {
                constants.push(value.clone());
            }
        });
        assert_eq!(constants.len(), expected, "{sql}: {constants:?}");
        for value in constants {
            match value {
                uqa_core::TemporalValue::Timestamp { micros }
                | uqa_core::TemporalValue::TimestampTz { micros } => {
                    assert_eq!(micros, clock, "{sql}");
                }
                uqa_core::TemporalValue::Interval { .. } => {}
                _ => panic!("unexpected constant in {sql}: {value:?}"),
            }
        }
    }
}

#[test]
fn prepared_assignments_read_constants_without_freezing_runtime_text_or_parameters() {
    let crate::Statement::CreateTable(table) = crate::compile(
        "CREATE TABLE clock_target (id integer PRIMARY KEY, stamp timestamp, stamps timestamp[])",
    )
    .unwrap()
    .remove(0) else {
        unreachable!()
    };
    let context = BindingContext {
        catalog: crate::binding::fixture::catalog(BTreeMap::from([(
            RelationIdentity::new("public", "clock_target"),
            crate::binding::fixture::table_definition(table.columns),
        )])),
        ..assignment_context()
    };
    let _clock = crate::expr::TransactionClockScope::enter(90_123_456_789);
    for (sql, expected) in [
        ("INSERT INTO clock_target(stamp) VALUES ('now')", 1),
        ("INSERT INTO clock_target(stamp) SELECT 'now'", 1),
        ("INSERT INTO clock_target(id, stamp) VALUES (1, 'now') ON CONFLICT(id) DO UPDATE SET stamp = 'now'", 2),
        ("UPDATE clock_target SET stamp = 'now'", 1),
        ("UPDATE clock_target SET stamps[1] = 'now'", 1),
        ("MERGE INTO clock_target USING (VALUES (1)) AS s(id) ON clock_target.id = s.id WHEN MATCHED THEN UPDATE SET stamp = 'now' WHEN NOT MATCHED THEN INSERT(stamp) VALUES ('now')", 2),
        ("UPDATE clock_target SET stamp = 'now'::text::timestamp", 0),
        ("UPDATE clock_target SET stamp = $1::timestamp", 0),
    ] {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        let declared = if sql.contains("$1") { vec![Some(ColumnType::Text)] } else { vec![] };
        assert_eq!(
            read_prepared_inputs(&NoRoutines, &mut plan, &declared, &context, None)
                .unwrap_or_else(|error| panic!("{sql}: {error}"))
                .parameter_types,
            declared
        );
        let mut count = 0;
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::TypedLiteral {
                value: uqa_core::Value::Temporal(uqa_core::TemporalValue::Timestamp { micros }),
                ..
            } = expression {
                assert_eq!(*micros, 90_123_456_789, "{sql}");
                count += 1;
            }
        });
        assert_eq!(count, expected, "{sql}");
    }
}

#[test]
fn domain_input_constants_use_the_base_type_without_erasing_parameter_identity() {
    let _clock = crate::expr::TransactionClockScope::enter(90_123_456_789);
    for (base, text) in [
        (ColumnType::TimestampPrecision(3), "now"),
        (ColumnType::Int2Vector, "1 2"),
        (ColumnType::OidVector, "1 2"),
    ] {
        let domain = ColumnType::Domain {
            schema: "public".into(),
            name: "input_domain".into(),
            oid: 16385,
            array_oid: Some(16386),
            base: Box::new(base.clone()),
        };
        let mut plan = UnifiedPlan::lower(
            crate::compile(&format!("SELECT '{text}'"))
                .unwrap()
                .remove(0),
        );
        let mut parameters = ParameterTypes::with_input_constants(&[None], None, None);
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::Literal(uqa_core::Value::Str(text)) = expression {
                let text = text.clone();
                let mut observed = ExpressionType::unknown_literal(expression, text);
                parameters.coerce_unknown(&mut observed, &domain).unwrap();
                assert_eq!(observed.ty, Some(domain.clone()));
            }
        });
        let mut parameter = parameters.reference(1).unwrap();
        parameters.coerce_unknown(&mut parameter, &domain).unwrap();
        parameters.take_input_constants().apply(&mut plan).unwrap();
        assert_eq!(parameters.finish().unwrap(), [Some(domain)]);
        let mut constants = 0;
        plan.rewrite_scalar_expressions(&mut |expression| {
            if let ScalarExpr::TypedLiteral {
                value, bound_type, ..
            } = expression
            {
                assert_eq!(*bound_type, Some(base.without_type_modifiers()));
                if matches!(base, ColumnType::TimestampPrecision(_)) {
                    assert_eq!(
                        *value,
                        uqa_core::Value::Temporal(uqa_core::TemporalValue::Timestamp {
                            micros: 90_123_456_789
                        })
                    );
                }
                constants += 1;
            }
        });
        assert_eq!(constants, 1);
    }
}
