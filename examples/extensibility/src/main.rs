//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Two ways to add behaviour to the engine: Rust callbacks registered on the
//! session, and PL/pgSQL routines stored in the catalog.
//!
//! Run with: cargo run -p example-extensibility

use uqa_core::Value;
use uqa_engine::{Engine, SQLAggregateState, SQLFunctionOptions, SQLFunctionVolatility};
use uqa_sql::SQLError;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let engine = Engine::new();
    engine.sql(
        "CREATE TABLE readings (id INTEGER PRIMARY KEY, sensor TEXT, celsius FLOAT)",
        &[],
    )?;
    engine.sql(
        "INSERT INTO readings (id, sensor, celsius) VALUES \
         (1, 'north', 21.5), (2, 'north', 23.0), (3, 'south', 18.25), \
         (4, 'south', 19.75), (5, 'south', 31.0)",
        &[],
    )?;

    // A scalar function is any Fn(&[Value]) -> Result<Value, SQLError>.
    // Declaring it immutable lets the optimizer fold and reorder calls; a
    // function that reads outside state must not claim that.
    engine.register_scalar_function_with_options(
        "to_fahrenheit",
        SQLFunctionOptions::read_only(SQLFunctionVolatility::Immutable),
        |args: &[Value]| match args.first() {
            Some(Value::Float(celsius)) => Ok(Value::Float(celsius * 9.0 / 5.0 + 32.0)),
            Some(Value::Int(celsius)) => Ok(Value::Float(*celsius as f64 * 9.0 / 5.0 + 32.0)),
            other => Err(SQLError::TypeMismatch(format!(
                "to_fahrenheit expects a number, got {other:?}"
            ))),
        },
    )?;

    println!("Scalar function in a projection:");
    let result = engine.sql(
        "SELECT sensor, celsius, to_fahrenheit(celsius) AS fahrenheit \
           FROM readings ORDER BY id",
        &[],
    )?;
    for row in &result.rows {
        println!(
            "  {:?} {:?} -> {:?}",
            row.get("sensor").unwrap_or(&Value::Null),
            row.get("celsius").unwrap_or(&Value::Null),
            row.get("fahrenheit").unwrap_or(&Value::Null)
        );
    }

    // An aggregate is a factory for per-group state. The engine calls
    // `observe` once per row in the group and `finish` once at the end, so the
    // state type owns whatever accumulator the aggregate needs.
    engine.register_aggregate_function("celsius_range", RangeState::default)?;

    println!("\nCustom aggregate grouped by sensor:");
    let result = engine.sql(
        "SELECT sensor, celsius_range(celsius) AS spread \
           FROM readings GROUP BY sensor ORDER BY sensor",
        &[],
    )?;
    for row in &result.rows {
        println!(
            "  {:?} spread={:?}",
            row.get("sensor").unwrap_or(&Value::Null),
            row.get("spread").unwrap_or(&Value::Null)
        );
    }

    // PL/pgSQL routines live in the catalog rather than the process, so they
    // survive reopen and are available to every session on the database.
    engine.sql(
        "CREATE FUNCTION classify(temp FLOAT) RETURNS TEXT AS $$
         BEGIN
             IF temp IS NULL THEN
                 RETURN 'unknown';
             ELSIF temp > 30.0 THEN
                 RETURN 'hot';
             ELSIF temp > 20.0 THEN
                 RETURN 'warm';
             ELSE
                 RETURN 'cool';
             END IF;
         END;
         $$ LANGUAGE plpgsql",
        &[],
    )?;

    println!("\nPL/pgSQL routine with branching:");
    let result = engine.sql(
        "SELECT sensor, celsius, classify(celsius) AS band \
           FROM readings ORDER BY id",
        &[],
    )?;
    for row in &result.rows {
        println!(
            "  {:?} {:?} -> {:?}",
            row.get("sensor").unwrap_or(&Value::Null),
            row.get("celsius").unwrap_or(&Value::Null),
            row.get("band").unwrap_or(&Value::Null)
        );
    }

    // Rust callbacks and SQL routines compose freely; neither knows the other
    // is not built in.
    println!("\nRust callback and SQL routine in one statement:");
    let result = engine.sql(
        "SELECT sensor, to_fahrenheit(celsius) AS fahrenheit, classify(celsius) AS band \
           FROM readings WHERE classify(celsius) <> 'cool' ORDER BY id",
        &[],
    )?;
    for row in &result.rows {
        println!(
            "  {:?} {:?} {:?}",
            row.get("sensor").unwrap_or(&Value::Null),
            row.get("fahrenheit").unwrap_or(&Value::Null),
            row.get("band").unwrap_or(&Value::Null)
        );
    }

    Ok(())
}

/// Per-group accumulator returning `max - min` over the observed numbers.
#[derive(Default)]
struct RangeState {
    min: Option<f64>,
    max: Option<f64>,
}

impl SQLAggregateState for RangeState {
    fn observe(&mut self, args: &[Value]) -> Result<(), SQLError> {
        let value = match args.first() {
            Some(Value::Float(number)) => *number,
            Some(Value::Int(number)) => *number as f64,
            // Nulls are skipped rather than poisoning the group, matching how
            // the built-in aggregates treat missing input.
            Some(Value::Null) | None => return Ok(()),
            other => {
                return Err(SQLError::TypeMismatch(format!(
                    "celsius_range expects a number, got {other:?}"
                )))
            }
        };
        self.min = Some(self.min.map_or(value, |current: f64| current.min(value)));
        self.max = Some(self.max.map_or(value, |current: f64| current.max(value)));
        Ok(())
    }

    fn finish(&self) -> Result<Value, SQLError> {
        match (self.min, self.max) {
            (Some(min), Some(max)) => Ok(Value::Float(max - min)),
            _ => Ok(Value::Null),
        }
    }
}
