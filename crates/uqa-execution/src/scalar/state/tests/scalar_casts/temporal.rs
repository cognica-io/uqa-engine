//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_core::TemporalValue;

struct TemporalCatalog {
    inner: ScalarCatalog,
    date_style: Cell<&'static str>,
    interval_style: Cell<&'static str>,
    zone: Cell<&'static str>,
    settings: Cell<usize>,
}

impl Default for TemporalCatalog {
    fn default() -> Self {
        Self {
            inner: ScalarCatalog::default(),
            date_style: Cell::new("ISO, MDY"),
            interval_style: Cell::new("postgres"),
            zone: Cell::new("UTC"),
            settings: Cell::new(0),
        }
    }
}

impl EngineHook for TemporalCatalog {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        self.inner.resolve_type_name(name)
    }
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        self.inner.cast_domain(value, source, target)
    }
    fn runtime_parameter(&self, name: &str) -> Result<Option<String>, SQLError> {
        self.settings.set(self.settings.get() + 1);
        Ok(match name {
            "DateStyle" => Some(self.date_style.get().into()),
            "IntervalStyle" => Some(self.interval_style.get().into()),
            "TimeZone" => Some(self.zone.get().into()),
            _ => None,
        })
    }
    fn temporal_interval_style(&self) -> Result<Option<uqa_core::TemporalIntervalStyle>, SQLError> {
        self.settings.set(self.settings.get() + 1);
        Ok(uqa_sql::semantics::parameters::intervalstyle::style(
            self.interval_style.get(),
        ))
    }
}

fn prepare(target: &str) -> PreparedExpressions<ScalarExpr> {
    PreparedExpressions::scalar(ScalarExpr::Cast {
        expr: Box::new(ScalarExpr::Column("v".into())),
        ty: target.into(),
        implicit: false,
    })
}

fn evaluate(
    prepared: &PreparedExpressions<ScalarExpr>,
    schema: &RowSchema,
    value: Value,
    catalog: &TemporalCatalog,
) -> Result<Value, SQLError> {
    let row =
        crate::OwnedPhysicalRow::new(schema.clone(), crate::PhysicalRow::from_values(vec![value]));
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(schema)
        .with_function_states(prepared.calls())
        .with_function_hook(catalog);
    eval_scalar(prepared, &context)
}

#[test]
fn temporal_row_casts_resolve_once_and_keep_live_input_settings() {
    let prepared = prepare("pg_catalog.date");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Text)]);
    let catalog = TemporalCatalog::default();
    CAST_TYPE_RESOLUTIONS.with(|count| count.set(0));
    for index in 0..1_000 {
        let (style, days) = if index % 2 == 0 {
            ("ISO, MDY", 18_263)
        } else {
            ("ISO, DMY", 18_293)
        };
        catalog.date_style.set(style);
        assert_eq!(
            evaluate(
                &prepared,
                &schema,
                Value::Str("01/02/2020".into()),
                &catalog
            )
            .unwrap(),
            Value::Temporal(TemporalValue::Date { days }),
        );
    }
    assert_eq!(CAST_TYPE_RESOLUTIONS.with(Cell::get), 1);
    assert_eq!(catalog.inner.resolutions.get(), 2);
    assert_eq!(catalog.inner.callbacks.get(), 1_000);
    assert_eq!(catalog.settings.get(), 1_000);
}

#[test]
fn temporal_descriptors_keep_live_zone_date_output_and_interval_input() {
    let catalog = TemporalCatalog::default();
    let prepared = prepare("pg_catalog.date");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::TimestampTz)]);
    for (zone, days) in [
        ("UTC", 18_262),
        ("America/Los_Angeles", 18_261),
        ("UTC", 18_262),
    ] {
        catalog.zone.set(zone);
        assert_eq!(
            evaluate(
                &prepared,
                &schema,
                Value::Temporal(TemporalValue::TimestampTz {
                    micros: 1_577_838_600_123_456,
                }),
                &catalog,
            )
            .unwrap(),
            Value::Temporal(TemporalValue::Date { days }),
        );
    }
    assert_eq!(catalog.inner.resolutions.get(), 2);
    let prepared = prepare("pg_catalog.varchar(20)");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Date)]);
    for (style, output) in [
        ("SQL, DMY", "02/01/2020"),
        ("German, DMY", "02.01.2020"),
        ("SQL, DMY", "02/01/2020"),
    ] {
        catalog.date_style.set(style);
        assert_eq!(
            evaluate(
                &prepared,
                &schema,
                Value::Temporal(TemporalValue::Date { days: 18_263 }),
                &catalog,
            )
            .unwrap(),
            Value::Str(output.into()),
        );
    }
    assert_eq!(catalog.inner.resolutions.get(), 4);
    let prepared = prepare("pg_catalog.interval");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Text)]);
    for (style, micros) in [
        ("postgres", 7_384_000_000),
        ("sql_standard", -7_384_000_000),
        ("postgres", 7_384_000_000),
    ] {
        catalog.interval_style.set(style);
        assert_eq!(
            evaluate(
                &prepared,
                &schema,
                Value::Str("-1 2:03:04".into()),
                &catalog,
            )
            .unwrap(),
            Value::Temporal(TemporalValue::Interval {
                months: 0,
                days: -1,
                micros,
            }),
        );
    }
    assert_eq!(catalog.inner.resolutions.get(), 6);
    assert_eq!(catalog.inner.callbacks.get(), 9);
    assert_eq!(catalog.settings.get(), 9);
}

#[test]
fn temporal_descriptor_failures_preserve_callbacks_and_current_allowance() {
    use uqa_core::{
        memory::{MemoryBudget, ProductionControl},
        CancellationToken,
    };
    let prepared = prepare("pg_catalog.date");
    let schema = RowSchema::with_types(vec!["v".into()], vec![Some(ColumnType::Text)]);
    let catalog = TemporalCatalog::default();
    assert_eq!(
        evaluate(
            &prepared,
            &schema,
            Value::Str("2020-01-02".into()),
            &catalog
        )
        .unwrap(),
        Value::Temporal(TemporalValue::Date { days: 18_263 }),
    );
    assert_eq!(
        evaluate(
            &prepared,
            &schema,
            Value::Str("not a date".into()),
            &catalog
        )
        .unwrap_err()
        .sqlstate(),
        Some("22007"),
    );
    assert_eq!(catalog.inner.resolutions.get(), 2);
    catalog.inner.fail.set(true);
    assert_eq!(
        evaluate(
            &prepared,
            &schema,
            Value::Str("not a date".into()),
            &catalog
        )
        .unwrap_err()
        .sqlstate(),
        Some("23514"),
    );
    let row = crate::OwnedPhysicalRow::new(
        schema.clone(),
        crate::PhysicalRow::from_values(vec![Value::Str("2020-01-02".into())]),
    );
    let context = ScalarEvalContext::from_row_lookup(&row, &[])
        .with_row_schema(&schema)
        .with_function_states(prepared.calls())
        .with_function_hook(&catalog);
    let budget = MemoryBudget::new(0);
    let token = CancellationToken::new();
    let invoking = CancellationToken::new();
    let control = ProductionControl::new(&budget, &token, &invoking);
    let callbacks = catalog.inner.callbacks.get();
    assert_eq!(
        crate::scalar::evaluator::eval_scalar_inner(&prepared, &context, &control)
            .unwrap_err()
            .sqlstate(),
        Some("53200"),
        "input and live-setting admission still precede the domain callback"
    );
    assert_eq!(catalog.inner.callbacks.get(), callbacks);
    let settings = catalog.settings.get();
    invoking.cancel();
    assert_eq!(
        crate::scalar::evaluator::eval_scalar_inner(&prepared, &context, &control)
            .unwrap_err()
            .sqlstate(),
        Some("57014"),
    );
    assert_eq!(catalog.settings.get(), settings);
    assert_eq!(budget.used(), 0);
}
