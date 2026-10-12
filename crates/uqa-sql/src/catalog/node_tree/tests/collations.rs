//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` references for collation type/name validation and parameter inference.

use super::{expressions, Routines};
use crate::ast::{FunctionBinding, FunctionDispatch};
use crate::{ColumnType, FunctionTypeResolver, RowSchema, SQLError, Statement};

struct Catalog;

impl FunctionTypeResolver for Catalog {
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
impl crate::routines::RoutineResolution for Catalog {}

#[derive(Debug, serde::Deserialize)]
struct Diagnostic {
    sqlstate: String,
    message: String,
    detail: Option<String>,
    hint: Option<String>,
}

impl Diagnostic {
    fn assert(&self, error: &SQLError, sql: &str) {
        assert_eq!(
            error.sqlstate(),
            Some(self.sqlstate.as_str()),
            "{sql}: {error}"
        );
        assert_eq!(error.to_string(), self.message, "{sql}");
        assert_eq!(error.detail(), self.detail.as_deref(), "{sql}");
        assert_eq!(error.hint(), self.hint.as_deref(), "{sql}");
    }
}

#[test]
fn collation_catalog_diagnostics_match_postgresql() {
    #[derive(serde::Deserialize)]
    struct Case {
        base: String,
        check: String,
        error: Diagnostic,
    }
    let cases: Vec<Case> =
        serde_json::from_str(include_str!("collation_admission_errors.json")).unwrap();
    for case in cases {
        let sql = format!("CREATE DOMAIN d AS {} CHECK({})", case.base, case.check);
        let Statement::CreateDomain(domain) = crate::compile(&sql).unwrap().remove(0) else {
            panic!("domain declaration");
        };
        let base = ColumnType::from_sql_name(&case.base).unwrap();
        let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: Some(&Catalog),
            routines: &Routines,
        };
        let error = context.check(&domain.checks[0].expression).unwrap_err();
        case.error.assert(&error, &sql);
    }
}

#[test]
fn collated_parameters_follow_postgresql_ordered_type_inference() {
    #[derive(serde::Deserialize)]
    struct Case {
        sql: String,
        declared: Vec<Option<String>>,
        parameter_types: Option<Vec<String>>,
        error: Option<Diagnostic>,
    }
    let cases: Vec<Case> = serde_json::from_str(include_str!("collation_parameters.json")).unwrap();
    for case in cases {
        let context = crate::binding::fixture::empty_binding_context();
        let mut plan =
            crate::plan::UnifiedPlan::lower(crate::compile(&case.sql).unwrap().remove(0));
        let declared = case
            .declared
            .iter()
            .map(|name| {
                name.as_ref()
                    .map(|name| ColumnType::from_sql_name(name).unwrap())
            })
            .collect::<Vec<_>>();
        let inferred =
            crate::binding::infer_prepared_parameter_types(&Catalog, &plan, &declared, &context);
        let prepared =
            crate::binding::read_prepared_inputs(&Catalog, &mut plan, &declared, &context, None)
                .map(|prepared| prepared.parameter_types);
        for result in [inferred, prepared] {
            if let Some(error) = &case.error {
                error.assert(&result.unwrap_err(), &case.sql);
            } else {
                let expected = case
                    .parameter_types
                    .as_ref()
                    .unwrap()
                    .iter()
                    .map(|name| Some(ColumnType::from_sql_name(name).unwrap()))
                    .collect::<Vec<_>>();
                assert_eq!(
                    result.unwrap_or_else(|error| panic!("{}: {error}", case.sql)),
                    expected,
                    "{}",
                    case.sql
                );
            }
        }
    }
}

#[test]
fn collate_syntax_and_routine_names_have_distinct_identities() {
    for (sql, syntax) in [
        ("SELECT 'x' COLLATE pg_catalog.\"C\"", true),
        ("SELECT public.\"COLLATE\"('x', 'C')", false),
        ("SELECT \"COLLATE\"('x', 'C')", false),
    ] {
        let Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("SELECT");
        };
        let expression = &query.projections[0].expr;
        let clause = crate::catalog::collations::expression::ast_clause(expression).unwrap();
        assert_eq!(clause.is_some(), syntax, "{sql}");
        let lowered = crate::plan::ExpressionPlan::lower(expression.clone());
        let clause =
            crate::catalog::collations::expression::scalar_clause(&lowered.scalar).unwrap();
        assert_eq!(clause.is_some(), syntax, "{sql}");
        if let Some(clause) = clause {
            assert_eq!(clause.name, "pg_catalog.\"C\"");
            assert_eq!(clause.oid, None);
        }
    }
}

#[test]
fn retained_collation_identity_is_not_reselected_from_its_old_name() {
    use crate::catalog::collations::expression::select;
    let binding = FunctionBinding::dispatched(FunctionDispatch::Collate { oid: Some(950) });
    let roundtrip: FunctionBinding =
        serde_json::from_slice(&serde_json::to_vec(&binding).unwrap()).unwrap();
    assert_eq!(roundtrip, binding);
    assert_eq!(
        select("missing.old_name", Some(950), Some(&ColumnType::Text), None).unwrap(),
        950
    );
    assert_eq!(
        select("\"C\"", Some(0), Some(&ColumnType::Text), None)
            .unwrap_err()
            .sqlstate(),
        Some("XX000")
    );
    assert_eq!(
        select("\"C\"", Some(950), Some(&ColumnType::Integer), None)
            .unwrap_err()
            .sqlstate(),
        Some("42804")
    );
}

#[test]
fn nonvalidating_introspection_preserves_collation_errors_for_analysis() {
    for sql in ["SELECT 1 COLLATE \"C\"", "SELECT 'x' COLLATE missing"] {
        let Statement::Select(query) = crate::compile(sql).unwrap().remove(0) else {
            panic!("SELECT");
        };
        let original = crate::plan::ExpressionPlan::lower(query.projections[0].expr.clone()).scalar;
        let bound = crate::type_resolution::bind_type_introspection(
            original.clone(),
            &RowSchema::default(),
            &[],
        );
        assert_eq!(bound, original, "{sql}");
        assert!(
            crate::type_resolution::common_context_expression_type(
                &bound,
                &RowSchema::default(),
                &[],
                None
            )
            .is_err(),
            "{sql}"
        );
    }
}
