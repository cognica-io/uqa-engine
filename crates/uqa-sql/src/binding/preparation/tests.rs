//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{ast::FunctionBinding, FunctionTypeResolver, RelationIdentity};
use std::collections::{BTreeMap, BTreeSet};

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
