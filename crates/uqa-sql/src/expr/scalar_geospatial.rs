//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Point, distance, containment, and temporal-overlap built-ins.

use super::conversion::to_f64_with_control;
use super::{point_xy, Result, SQLError, Value};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(super) fn eval_geospatial_functions_with_control(
    name: &str,
    args: &[Value],
    control: &ProductionControl<'_>,
) -> Option<Result<Produced<Value>>> {
    const NAMES: &[&str] = &[
        "point",
        "st_distance",
        "st_within",
        "st_dwithin",
        "overlaps",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Produced<Value>> {
        control.check()?;
        let value = match name {
            // -------------------------------------------------------------
            // Geospatial primitives (point, distance, within, dwithin)
            // -------------------------------------------------------------
            "point" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("point takes 2 args".into()));
                }
                let x = to_f64_with_control(&args[0], control)?;
                let y = to_f64_with_control(&args[1], control)?;
                let mut values = ProductionVec::new(*control);
                for value in [x, y] {
                    values.push_produced(
                        control.finish(Value::Float(value), control.empty_reservation())?,
                    )?;
                }
                let (values, memory) = values.finish()?.into_parts();
                return Ok(control.finish(Value::List(values), memory)?);
            }
            "st_distance" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("st_distance takes 2 args".into()));
                }
                let (x1, y1) = point_xy(&args[0], control)?;
                let (x2, y2) = point_xy(&args[1], control)?;
                Value::Float(((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt())
            }
            "st_within" | "st_dwithin" => {
                // `st_dwithin` uses the Euclidean radius semantics supported by
                // this scalar evaluator. Polygon containment is handled by the
                // spatial operator layer rather than this value-only function.
                if args.len() < 2 {
                    return Err(SQLError::TypeMismatch(format!("{name} takes 2-3 args")));
                }
                let (x1, y1) = point_xy(&args[0], control)?;
                let (x2, y2) = point_xy(&args[1], control)?;
                let d = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
                let radius = if args.len() == 3 {
                    to_f64_with_control(&args[2], control)?
                } else {
                    0.0
                };
                Value::Bool(d <= radius)
            }
            "overlaps" => return super::time::overlaps(args, None, control),
            _ => unreachable!("function family membership was checked before dispatch"),
        };
        Ok(control.finish(value, control.empty_reservation())?)
    })())
}

#[cfg(test)]
mod tests;
