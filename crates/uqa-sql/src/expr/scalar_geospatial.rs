//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Point, distance, containment, and temporal-overlap built-ins.

use super::{parse_timestamp, point_xy, to_f64, value_to_string, Result, SQLError, Value};

pub(super) fn eval_geospatial_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
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
    Some((|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // Geospatial primitives (point, distance, within, dwithin)
            // -------------------------------------------------------------
            "point" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("point takes 2 args".into()));
                }
                let x = to_f64(&args[0])?;
                let y = to_f64(&args[1])?;
                Ok(Value::List(vec![Value::Float(x), Value::Float(y)]))
            }
            "st_distance" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("st_distance takes 2 args".into()));
                }
                let (x1, y1) = point_xy(&args[0])?;
                let (x2, y2) = point_xy(&args[1])?;
                Ok(Value::Float(((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt()))
            }
            "st_within" | "st_dwithin" => {
                // `st_dwithin` uses the Euclidean radius semantics supported by
                // this scalar evaluator. Polygon containment is handled by the
                // spatial operator layer rather than this value-only function.
                if args.len() < 2 {
                    return Err(SQLError::TypeMismatch(format!("{name} takes 2-3 args")));
                }
                let (x1, y1) = point_xy(&args[0])?;
                let (x2, y2) = point_xy(&args[1])?;
                let d = ((x2 - x1).powi(2) + (y2 - y1).powi(2)).sqrt();
                let radius = if args.len() == 3 {
                    to_f64(&args[2])?
                } else {
                    0.0
                };
                Ok(Value::Bool(d <= radius))
            }
            "overlaps" => {
                if args.len() != 4 {
                    return Err(SQLError::TypeMismatch(
                        "overlaps takes 4 args (start1, end1, start2, end2)".into(),
                    ));
                }
                let s1 = parse_timestamp(&value_to_string(&args[0]))?;
                let e1 = parse_timestamp(&value_to_string(&args[1]))?;
                let s2 = parse_timestamp(&value_to_string(&args[2]))?;
                let e2 = parse_timestamp(&value_to_string(&args[3]))?;
                Ok(Value::Bool(s1 < e2 && s2 < e1))
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}
