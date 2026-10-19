//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::assignment::AssignmentContext;
use crate::catalog::domain::{DomainCatalog, StoredDomain};
use std::cell::Cell;
use uqa_core::{RelationIdentity, TemporalValue};

struct Session<'a> {
    zone: &'static str,
    calls: Cell<usize>,
    checks: Cell<usize>,
    cancel: Option<&'a CancellationToken>,
}

impl Session<'_> {
    fn new(zone: &'static str) -> Self {
        Self {
            zone,
            calls: Cell::new(0),
            checks: Cell::new(0),
            cancel: None,
        }
    }
}

fn domain() -> StoredDomain {
    StoredDomain {
        object_id: [3; 16],
        oid: 16_384,
        array_oid: None,
        identity: RelationIdentity::new("public", "zoned"),
        owner: uqa_core::catalog_role::RoleIdentity::BOOTSTRAP,
        definition: crate::ast::CreateDomain {
            collation_oid: None,
            name: "public.zoned".into(),
            base: ColumnType::TimestampTz,
            collation: None,
            default: None,
            not_null: None,
            checks: vec![crate::ast::DomainCheck {
                name: Some("accepted".into()),
                catalog_identity: None,
                expression: crate::ast::Expr::Literal(Value::Bool(true)),
                validated: true,
            }],
        },
        array_name: None,
        usage_acl: None,
    }
}

impl EngineHook for Session<'_> {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }
    fn runtime_parameter(&self, name: &str) -> Result<Option<String>> {
        assert_eq!(name, "TimeZone");
        self.calls.set(self.calls.get() + 1);
        if let Some(token) = self.cancel {
            token.cancel();
        }
        Ok(Some(self.zone.into()))
    }
    fn resolve_type_name(&self, name: &str) -> std::result::Result<Option<ColumnType>, String> {
        Ok(if name == "public.zoned" || name == "domain#16384" {
            Some(domain().column_type())
        } else {
            ColumnType::from_sql_name(name).ok()
        })
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>> {
        crate::assignment::domain::cast_domain_value(self, value, source, target)
    }
}

impl DomainCatalog for Session<'_> {
    fn domain_by_oid(&self, oid: u32) -> Option<StoredDomain> {
        (oid == 16_384).then(domain)
    }
}

impl AssignmentContext for Session<'_> {
    fn evaluate_domain_check(
        &self,
        _: &crate::ast::Expr,
        row: &crate::ResultRow,
        _: &crate::RowSchema,
    ) -> Result<Value> {
        assert!(matches!(
            row.get("value"),
            Some(Value::Temporal(TemporalValue::TimestampTz { .. }))
        ));
        self.checks.set(self.checks.get() + 1);
        Ok(Value::Bool(true))
    }
}

fn timestamp(text: &str) -> Value {
    Value::Temporal(TemporalValue::parse_timestamp(text).unwrap())
}

fn zoned(micros: i64) -> Value {
    Value::Temporal(TemporalValue::TimestampTz { micros })
}

#[test]
fn zoned_casts_to_civil_values_match_postgresql_at_transitions_and_extrema() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("timezone.json")).unwrap();
    let cases = reference["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 99);
    for case in cases {
        let zone = match case["zone"].as_str().unwrap() {
            "UTC" => "UTC",
            "Asia/Seoul" => "Asia/Seoul",
            "America/New_York" => "America/New_York",
            _ => unreachable!(),
        };
        let session = Session::new(zone);
        let input = Value::Temporal(
            TemporalValue::parse_timestamp_tz(case["input"].as_str().unwrap()).unwrap(),
        );
        let target = case["target"].as_str().unwrap();
        let actual =
            cast_value_with_type_resolution(&input, Some("timestamptz"), target, Some(&session));
        if let Some(expected) = case["expected"].as_str() {
            let Value::Temporal(value) = actual.unwrap() else {
                panic!("temporal cast: {case}");
            };
            // A cast can retain the day before the date input boundary; output observes that physical day without reparsing it.
            assert_eq!(value.to_sql_string(), expected, "{case}");
        } else {
            let error = actual.unwrap_err();
            assert_eq!(
                error.sqlstate(),
                case["error"]["sqlstate"].as_str(),
                "{case}"
            );
            assert_eq!(
                error.to_string(),
                case["error"]["message"].as_str().unwrap(),
                "{case}"
            );
        }
        let finite = !matches!(&input, Value::Temporal(value) if value.infinity().is_some());
        assert_eq!(session.calls.get(), usize::from(finite), "{case}");
    }
}

#[test]
fn reverse_zone_casts_keep_array_shape_precision_and_cancellation() {
    let session = Session::new("Asia/Seoul");
    let input = zoned(1_704_164_645_123_600);
    let array = Value::Array(
        ArrayValue::with_lower_bounds(vec![input.clone(), Value::Null], vec![-2]).unwrap(),
    );
    let output = cast_value_with_type_resolution(
        &array,
        Some("timestamptz[]"),
        "timestamp(3)[]",
        Some(&session),
    )
    .unwrap();
    assert_eq!(
        output,
        Value::Array(
            ArrayValue::with_lower_bounds(
                vec![timestamp("2024-01-02 12:04:05.124"), Value::Null],
                vec![-2]
            )
            .unwrap()
        )
    );
    assert_eq!(session.calls.get(), 1);
    for target in ["timestamp", "date"] {
        for cancel_original in [true, false] {
            let budget = MemoryBudget::new(4096);
            let original = CancellationToken::new();
            let invoking = CancellationToken::new();
            let session = Session {
                cancel: Some(if cancel_original {
                    &original
                } else {
                    &invoking
                }),
                ..Session::new("Asia/Seoul")
            };
            let error = cast_value_with_type_resolution_with_control(
                &input,
                Some("timestamptz"),
                target,
                Some(&session),
                &ProductionControl::new(&budget, &original, &invoking),
            )
            .unwrap_err();
            assert_eq!(error.sqlstate(), Some("57014"));
            assert_eq!(session.calls.get(), 1);
            assert_eq!(budget.used(), 0);
        }
    }
}

#[test]
fn typed_local_casts_obey_the_invoking_session_and_postgresql_gap_fold_rules() {
    for (zone, input, expected) in [
        (
            "Asia/Seoul",
            timestamp("2024-01-02 03:04:05"),
            1_704_132_245_000_000,
        ),
        (
            "UTC",
            timestamp("2024-01-02 03:04:05"),
            1_704_164_645_000_000,
        ),
        (
            "Asia/Seoul",
            Value::Temporal(TemporalValue::parse_date("2024-01-02").unwrap()),
            1_704_121_200_000_000,
        ),
        (
            "America/New_York",
            timestamp("2024-03-10 02:30:00"),
            1_710_055_800_000_000,
        ),
        (
            "America/New_York",
            timestamp("2024-11-03 01:30:00"),
            1_730_615_400_000_000,
        ),
    ] {
        let session = Session::new(zone);
        let actual =
            cast_value_with_type_resolution(&input, None, "timestamptz", Some(&session)).unwrap();
        assert_eq!(actual, zoned(expected), "{zone}: {input:?}");
        assert_eq!(session.calls.get(), 1);
    }
}

#[test]
fn temporal_assignment_domain_and_array_paths_reuse_session_conversion_once() {
    let input = timestamp("2024-01-02 03:04:05");
    let session = Session::new("Asia/Seoul");
    let expected = zoned(1_704_132_245_000_000);
    let convert = crate::assignment::conversion::convert_value_to_column_type_with_context;
    assert_eq!(
        convert(&session, input.clone(), &ColumnType::TimestampTz).unwrap(),
        expected
    );
    assert_eq!(session.calls.get(), 1);
    assert_eq!(
        convert(&session, input.clone(), &domain().column_type()).unwrap(),
        expected
    );
    assert_eq!((session.calls.get(), session.checks.get()), (2, 1));
    assert_eq!(
        cast_value_with_type_resolution(&input, Some("timestamp"), "public.zoned", Some(&session))
            .unwrap(),
        expected
    );
    assert_eq!((session.calls.get(), session.checks.get()), (3, 2));
    let array =
        Value::Array(ArrayValue::with_lower_bounds(vec![input, Value::Null], vec![-1]).unwrap());
    let converted = cast_value_with_type_resolution(
        &array,
        Some("timestamp[]"),
        "timestamptz[]",
        Some(&session),
    )
    .unwrap();
    assert_eq!(
        converted,
        Value::Array(ArrayValue::with_lower_bounds(vec![expected, Value::Null], vec![-1]).unwrap())
    );
    assert_eq!(session.calls.get(), 4);
}

#[test]
fn zone_casts_preserve_precision_nulls_and_already_zoned_values() {
    let session = Session::new("Asia/Seoul");
    let output = cast_value_with_type_resolution(
        &timestamp("2024-01-02 03:04:05.1236"),
        Some("timestamp"),
        "timestamp(3) with time zone",
        Some(&session),
    )
    .unwrap();
    assert_eq!(output, zoned(1_704_132_245_124_000));
    for value in [Value::Null, output] {
        assert_eq!(
            cast_value_with_type_resolution(&value, None, "timestamptz", Some(&session)).unwrap(),
            value
        );
    }
    assert_eq!(session.calls.get(), 1);
    let minimum = timestamp("4714-11-24 00:00:00 BC");
    let error =
        cast_value_with_type_resolution(&minimum, None, "timestamptz", Some(&session)).unwrap_err();
    assert_eq!(error.sqlstate(), Some("22008"));
}

#[test]
fn zone_callback_storage_and_cancellation_remain_in_the_production_scope() {
    let budget = MemoryBudget::new(4096);
    let original = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &original, &invoking);
    let input = timestamp("2024-01-02 03:04:05");
    let session = Session::new("Asia/Seoul");
    let output = cast_value_with_type_resolution_with_control(
        &input,
        None,
        "timestamptz",
        Some(&session),
        &control,
    )
    .unwrap();
    assert_eq!(*output, zoned(1_704_132_245_000_000));
    assert_eq!(budget.used(), 0);
    for cancel_original in [true, false] {
        let original = CancellationToken::new();
        let invoking = CancellationToken::new();
        let control = ProductionControl::new(&budget, &original, &invoking);
        let token = if cancel_original {
            &original
        } else {
            &invoking
        };
        let session = Session {
            cancel: Some(token),
            ..Session::new("Asia/Seoul")
        };
        let error = cast_value_with_type_resolution_with_control(
            &input,
            None,
            "timestamptz",
            Some(&session),
            &control,
        )
        .unwrap_err();
        assert_eq!(error.sqlstate(), Some("57014"));
        assert_eq!(session.calls.get(), 1);
        assert_eq!(budget.used(), 0);
    }
}
