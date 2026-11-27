//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent `PostgreSQL` query contracts require declared operators before reading rows.

use super::*;
use crate::{
    ast::{RuleEvent, Statement},
    catalog::{events::RuleConditionBinding, work::inline, CatalogValidation},
    plan::ExpressionPlan,
};
use uqa_core::{
    memory::{MemoryBudget, ProductionControl},
    CancellationToken,
};

use crate::ast::CompositeTypeReference;
use crate::expr::composites::{CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor};
use std::sync::Arc;

struct Types(Arc<CompositeTypeDescriptor>);

fn pair() -> ColumnType {
    ColumnType::Composite(CompositeTypeReference {
        schema: "hidden".into(),
        name: "pair".into(),
        oid: 40_001,
        relation_oid: 40_002,
        array_oid: 40_003,
    })
}

impl FunctionTypeResolver for Types {
    fn format_type_name(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(match ty {
            ColumnType::Composite(_) => "hidden.pair".into(),
            ColumnType::Array(element) => format!("{}[]", self.format_type_name(element)?),
            _ => ty.sql_name(),
        })
    }

    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(match name {
            "pair" | "hidden.pair" => Some(pair()),
            "pair[]" => Some(ColumnType::Array(Box::new(pair()))),
            _ if name == pair().catalog_name() => Some(pair()),
            _ => None,
        })
    }

    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }

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

impl RoutineResolution for Types {}

impl Types {
    fn pair(second: ColumnType) -> Self {
        Self::with_attributes(vec![
            CompositeAttribute {
                collation_oid: None,
                name: "n".into(),
                ty: ColumnType::Integer,
                number: 1,
            },
            CompositeAttribute {
                collation_oid: None,
                name: "v".into(),
                ty: second,
                number: 2,
            },
        ])
    }

    fn with_attributes(attributes: Vec<CompositeAttribute>) -> Self {
        Self(Arc::new(CompositeTypeDescriptor {
            type_oid: 40_001,
            relation_oid: 40_002,
            dropped: Vec::new(),
            attributes,
        }))
    }
}

impl CompositeTypeCatalog for Types {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        assert_eq!(oid, self.0.type_oid);
        Ok(Some(self.0.clone()))
    }
}

fn analyze(sql: &str, catalog: &Types) -> Result<Vec<Option<ColumnType>>, SQLError> {
    let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
    infer_prepared_parameter_types(
        catalog,
        &plan,
        &[],
        &crate::binding::fixture::empty_binding_context(),
    )
}

#[test]
fn shared_query_outputs_preserve_named_composite_field_descriptors() {
    let catalog = Types::pair(ColumnType::Text);
    let binding = crate::binding::fixture::empty_binding_context();
    for sql in [
        "SELECT NULL::pair",
        "WITH r AS (SELECT NULL::pair AS v) SELECT v FROM r",
        "SELECT r.v FROM (SELECT NULL::pair AS v) r",
        "SELECT DISTINCT NULL::pair",
        "SELECT coalesce(NULL::pair,NULL::pair)",
    ] {
        let UnifiedPlan::Query(query) = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0))
        else {
            panic!("query")
        };
        let mut preparation = Preparation {
            bound_condition_queries: None,
            routines: &catalog,
            scope: &mut SchemaScope::for_analysis(&binding).unwrap(),
            parameters: ParameterTypes::new(&[]),
            schema_expression: None,
        };
        for output in [
            preparation.query(&query, None),
            crate::binding::analyze_query_plan_schema(&catalog, &query, &[], &binding, None),
            crate::binding::bind_query_plan_schema(&catalog, &query, &[], &binding, None),
        ] {
            let output = output.unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert_eq!(output.column_type(0), Some(&pair()), "{sql}");
            assert_eq!(
                output.record_fields(0).unwrap().as_ref(),
                [Some(ColumnType::Integer), Some(ColumnType::Text)],
                "{sql}"
            );
        }
    }
}

#[test]
fn empty_named_composite_queries_require_equality_from_every_live_field() {
    for sql in [
        "SELECT DISTINCT NULL::pair WHERE false",
        "SELECT v FROM (SELECT NULL::pair AS v WHERE false) t GROUP BY v",
        "SELECT DISTINCT ON (v) v FROM (SELECT NULL::pair AS v WHERE false) t",
        "SELECT count(DISTINCT NULL::pair) WHERE false",
        "SELECT count(*) OVER (PARTITION BY NULL::pair) WHERE false",
        "SELECT NULL::pair WHERE false UNION SELECT NULL::pair WHERE false",
        "SELECT NULL::pair WHERE false INTERSECT SELECT NULL::pair WHERE false",
        "SELECT NULL::pair WHERE false EXCEPT SELECT NULL::pair WHERE false",
    ] {
        let error = analyze(sql, &Types::pair(ColumnType::Json)).unwrap_err();
        assert_eq!(error.sqlstate(), Some("42883"), "{sql}: {error}");
        assert_eq!(
            error.to_string(),
            "could not identify an equality operator for type hidden.pair",
            "{sql}"
        );
        assert!(
            analyze(sql, &Types::pair(ColumnType::Text)).is_ok(),
            "{sql}"
        );
    }
    analyze(
        "SELECT NULL::pair WHERE false UNION ALL SELECT NULL::pair WHERE false",
        &Types::pair(ColumnType::Json),
    )
    .unwrap();
}

#[test]
fn aggregate_distinct_requires_ordering_after_equality_with_its_own_detail() {
    let catalog = Types::pair(ColumnType::Xid);
    analyze("SELECT DISTINCT NULL::pair WHERE false", &catalog).unwrap();
    let error = analyze("SELECT count(DISTINCT NULL::pair) WHERE false", &catalog).unwrap_err();
    assert!(
        matches!(error, SQLError::Diagnostic { ref sqlstate, ref message, ref detail, hint: None }
        if sqlstate == "42883" && message == "could not identify an ordering operator for type hidden.pair"
        && detail.as_deref() == Some("Aggregates with DISTINCT must be able to sort their inputs.")),
        "{error:?}"
    );
}

#[test]
fn ordering_preserves_catalog_name_and_hint_for_nested_declared_types() {
    for (sql, name) in [
        (
            "SELECT NULL::pair AS v WHERE false ORDER BY v",
            "hidden.pair",
        ),
        (
            "SELECT NULL::pair[] AS v WHERE false ORDER BY v",
            "hidden.pair[]",
        ),
        (
            "SELECT count(*) OVER (ORDER BY NULL::pair) WHERE false",
            "hidden.pair",
        ),
    ] {
        let error = analyze(sql, &Types::pair(ColumnType::Json)).unwrap_err();
        assert!(
            matches!(error, SQLError::Diagnostic { ref sqlstate, ref message, detail: None, ref hint }
            if sqlstate == "42883" && message == &format!("could not identify an ordering operator for type {name}")
            && hint.as_deref() == Some("Use an explicit ordering operator or modify the query.")),
            "{sql}: {error:?}"
        );
    }
}

#[test]
fn anonymous_record_capabilities_do_not_inspect_unreached_field_types() {
    for sql in [
        "SELECT DISTINCT ROW(1, '{}'::json) WHERE false",
        "SELECT ROW(1, '{}'::json) AS v WHERE false GROUP BY v",
        "SELECT ROW(1, '{}'::json) AS v WHERE false ORDER BY v",
        "SELECT count(DISTINCT ROW(1, '{}'::json)) WHERE false",
    ] {
        analyze(sql, &Types::pair(ColumnType::Json))
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
}

#[test]
fn hashing_and_ordering_require_independent_declared_field_support() {
    use crate::type_resolution::{
        type_supports_hashing, type_supports_ordering, ComparisonKeySupport,
    };
    for ty in [
        ColumnType::Record,
        ColumnType::Array(Box::new(ColumnType::Record)),
    ] {
        assert!(!type_supports_hashing(&ty, &Types::pair(ColumnType::Text)).unwrap());
        assert!(type_supports_ordering(&ty, &Types::pair(ColumnType::Text)).unwrap());
    }
    assert!(type_supports_hashing(&pair(), &Types::pair(ColumnType::Xid)).unwrap());
    assert!(!type_supports_ordering(&pair(), &Types::pair(ColumnType::Xid)).unwrap());
    assert!(!type_supports_hashing(&pair(), &Types::pair(ColumnType::Json)).unwrap());
    let mut support = ComparisonKeySupport::default();
    support
        .include(&ColumnType::Record, &Types::pair(ColumnType::Xid))
        .unwrap();
    support
        .include(&pair(), &Types::pair(ColumnType::Xid))
        .unwrap();
    let error = support.require_implementation("GROUP BY").unwrap_err();
    assert_eq!(error.sqlstate(), Some("0A000"));
    assert_eq!(error.to_string(), "could not implement GROUP BY");
}

#[test]
fn row_distinct_binds_each_field_before_reading_rows() {
    let catalog = Types::pair(ColumnType::Json);
    let analyze = |sql: &str| {
        let mut plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        crate::binding::composite_inputs::retain_composite_inputs(
            &catalog,
            &mut plan,
            &[],
            &crate::binding::fixture::empty_binding_context(),
        )
    };
    for (sql, state) in [
        ("SELECT ROW(1) IS DISTINCT FROM ROW(1,2)", "42601"),
        (
            "SELECT ROW(1,'{}'::json)::pair IS DISTINCT FROM ROW(2,'{}'::json)::pair WHERE false",
            "42883",
        ),
    ] {
        let error = analyze(sql).unwrap_err();
        assert_eq!(error.sqlstate(), Some(state), "{sql}: {error}");
    }
    analyze("SELECT ROW(1::smallint,NULL::integer) IS DISTINCT FROM ROW(1::bigint,NULL::integer)")
        .unwrap();
}

fn stored_rule_cast_windows(width: usize, bad: Option<usize>, stop: Option<usize>) -> usize {
    let memory = MemoryBudget::new(2 << 20);
    let cancel = CancellationToken::new();
    let control = ProductionControl::new(&memory, &cancel, &cancel);
    let names: Vec<_> = (0..width).map(|n| format!("field_{n}")).collect();
    let integer = ColumnType::Integer;
    let row = RowSchema::with_column_metadata_with_control(
        None,
        names
            .iter()
            .map(|name| (name.as_str(), Some(&integer), None)),
        &control,
    )
    .unwrap();
    let types = Types::with_attributes(
        names
            .iter()
            .enumerate()
            .map(|(index, name)| CompositeAttribute {
                name: name.clone(),
                number: i16::try_from(index + 1).unwrap(),
                ty: if bad == Some(index) {
                    ColumnType::Date
                } else {
                    ColumnType::Integer
                },
                collation_oid: None,
            })
            .collect(),
    );
    let binding = RuleConditionBinding::for_event(&names, RuleEvent::Update);
    let schema = inline(binding.captured_row_schema_with_work(&row, &control, &mut ())).unwrap();
    let rows = ["old", "new"].map(|side| {
        inline(binding.current_whole_row_with_work(side, "hidden.pair", &row, &control, &mut ()))
            .unwrap()
    });
    drop(row);
    let Statement::CreateRule(rule) = crate::compile(
        "CREATE RULE saved AS ON UPDATE TO hidden.pair WHERE OLD IS NOT NULL DO NOTHING",
    )
    .unwrap()
    .remove(0) else {
        panic!("rule")
    };
    let condition = ExpressionPlan::lower(rule.condition.unwrap());
    let owner = control.capture();
    let mut reader = CatalogValidation::new(&control, |mut work| async move {
        let control = owner.control();
        crate::binding::validate_stored_rule_condition_with_work(
            &types,
            &condition,
            &schema,
            crate::binding::fixture::empty_binding_context().copy_with_control(&control)?,
            rows,
            &control,
            &mut work,
        )
        .await
    })
    .unwrap();
    let mut calls = 0;
    let error = loop {
        if stop == Some(calls) {
            cancel.cancel();
        }
        calls += 1;
        match reader.advance(&control, 1) {
            Ok(Some(next)) => reader = next,
            Ok(None) => break None,
            Err(error) => break Some(error),
        }
    };
    if stop.is_some() {
        assert_eq!(error.unwrap().sqlstate(), Some("57014"));
    } else if let Some(index) = bad {
        let error = error.expect("incompatible field");
        assert_eq!(error.sqlstate(), Some("42846"), "{error:?}");
        assert_eq!(
            error.detail(),
            Some(format!("Cannot cast type integer to date in column {}.", index + 1).as_str())
        );
    } else {
        assert!(error.is_none(), "{error:?}");
    }
    assert_eq!(memory.used(), 0);
    calls
}

#[test]
fn stored_rule_whole_row_cast_visits_each_declared_field_before_reporting_its_error() {
    const WIDTH: usize = 64;
    let first = stored_rule_cast_windows(WIDTH, Some(0), None);
    let last = stored_rule_cast_windows(WIDTH, Some(WIDTH - 1), None);
    assert!(last >= first + WIDTH - 1, "first {first}, last {last}");
    assert!(stored_rule_cast_windows(WIDTH, None, None) > last);
    stored_rule_cast_windows(WIDTH, Some(WIDTH - 1), Some(first + (last - first) / 2));
}

#[test]
fn distinct_predicates_infer_equality_operand_types() {
    let context = assignment_context();
    let infer = |sql: &str, declared: &[Option<ColumnType>]| {
        let plan = UnifiedPlan::lower(crate::compile(sql).unwrap().remove(0));
        infer_prepared_parameter_types(&NoRoutines, &plan, declared, &context)
    };
    for (sql, expected) in [
        ("SELECT id FROM assignment_target WHERE id IS NOT DISTINCT FROM $1", vec![ColumnType::Integer]),
        ("UPDATE assignment_target SET id=$1 WHERE $2 IS DISTINCT FROM value", vec![ColumnType::Integer, ColumnType::Array(Box::new(ColumnType::Integer))]),
        ("DELETE FROM assignment_target WHERE EXISTS (SELECT 1 WHERE assignment_target.id IS NOT DISTINCT FROM $1)", vec![ColumnType::Integer]),
        ("SELECT $1 IS DISTINCT FROM $2", vec![ColumnType::Text, ColumnType::Text]),
        ("DELETE FROM assignment_target WHERE ROW(id, value) IS NOT DISTINCT FROM ROW($1, $2)", vec![ColumnType::Integer, ColumnType::Array(Box::new(ColumnType::Integer))]),
    ] {
        let types = infer(sql, &vec![None; expected.len()])
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(types, expected.into_iter().map(Some).collect::<Vec<_>>(), "{sql}");
    }
    for (sql, declared, expected) in [
        (
            "DELETE FROM assignment_target WHERE id IS DISTINCT FROM $1",
            Some(ColumnType::Boolean),
            "42883",
        ),
        (
            "DELETE FROM assignment_target WHERE $1 IS DISTINCT FROM NULL",
            None,
            "42P18",
        ),
    ] {
        let error = infer(sql, &[declared]).unwrap_err();
        assert_eq!(error.sqlstate(), Some(expected), "{sql}: {error}");
    }
}
