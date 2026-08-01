//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Cypher scalar-function evaluation.

use super::{
    agtype, domain_float_fn, exact_i64_to_f64, float_fn, is_aggregate_name, nonnegative_i64_to_u64,
    nonnegative_i64_to_usize, string_fn, trunc_f64_to_i64, unsupported_argument, usize_to_i64,
    validated_path_elements, BindingRow, CypherError, CypherExecutor, FunctionCall, GraphStore,
    Value,
};

impl<G: GraphStore> CypherExecutor<'_, G> {
    #[allow(clippy::too_many_lines)]
    pub(super) fn eval_function(
        &self,
        fc: &FunctionCall,
        row: &BindingRow,
    ) -> Result<Value, CypherError> {
        // Aggregates are handled in the RETURN/WITH path.
        if is_aggregate_name(&fc.name) {
            return Err(CypherError::Unsupported(format!(
                "aggregate {} outside of RETURN/WITH",
                fc.name
            )));
        }
        let name = fc.name.to_lowercase();
        // `exists(n.prop)` needs the unevaluated property expression.
        if name == "exists" {
            let value = match fc.args.first() {
                Some(arg) => self.eval(arg, row)?,
                None => Value::Null,
            };
            return Ok(Value::Bool(value != Value::Null));
        }
        let args: Vec<Value> = fc
            .args
            .iter()
            .map(|a| self.eval(a, row))
            .collect::<Result<_, _>>()?;
        let arg = args.first();
        match name.as_str() {
            "id" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => agtype::entity_id(v).map(Value::Int).ok_or_else(|| {
                    CypherError::TypeError("id() argument must be a vertex, edge or null".into())
                }),
            },
            "label" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => agtype::entity_label(v)
                    .map(|label| Value::Str(label.to_string()))
                    .ok_or_else(|| {
                        CypherError::TypeError(
                            "label() argument must resolve to an edge or vertex".into(),
                        )
                    }),
            },
            "labels" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Vertex) => {
                    Ok(Value::List(vec![Value::Str(
                        agtype::entity_label(v).unwrap_or_default().to_string(),
                    )]))
                }
                Some(_) => Err(CypherError::TypeError(
                    "labels() argument must be a vertex".into(),
                )),
            },
            "type" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Edge) => Ok(
                    Value::Str(agtype::entity_label(v).unwrap_or_default().to_string()),
                ),
                Some(_) => Err(CypherError::TypeError(
                    "type() argument must be an edge or null".into(),
                )),
            },
            "keys" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => {
                    let map = agtype::entity_properties(v).cloned().or_else(|| match v {
                        Value::Map(m) => Some(m.clone()),
                        _ => None,
                    });
                    match map {
                        Some(map) => {
                            let mut keys: Vec<&String> = map.keys().collect();
                            keys.sort_by(|a, b| agtype::jsonb_key_cmp(a, b));
                            Ok(Value::List(
                                keys.into_iter().map(|k| Value::Str(k.clone())).collect(),
                            ))
                        }
                        None => Err(CypherError::TypeError(
                            "keys() argument must be a vertex, edge, map, or null".into(),
                        )),
                    }
                }
            },
            "properties" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) => match agtype::entity_properties(v) {
                    Some(props) => Ok(Value::Map(props.clone())),
                    None => match v {
                        Value::Map(m) => Ok(Value::Map(m.clone())),
                        _ => Err(CypherError::TypeError(
                            "properties() argument must be a vertex, an edge or null".into(),
                        )),
                    },
                },
            },
            "startnode" | "endnode" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Edge) => {
                    let id = if name == "startnode" {
                        agtype::edge_start_id(v)
                    } else {
                        agtype::edge_end_id(v)
                    };
                    let Some(id) = id else {
                        return Err(CypherError::Storage(
                            "edge entity is missing a valid endpoint id".into(),
                        ));
                    };
                    let id = nonnegative_i64_to_u64(id, "edge endpoint id")?;
                    match self.store.get_vertex(id) {
                        Some(vertex) => Ok(agtype::vertex_to_value(vertex)?),
                        None => Ok(Value::Null),
                    }
                }
                Some(_) => Err(CypherError::TypeError(format!(
                    "{}() argument must be an edge or null",
                    if name == "startnode" {
                        "startNode"
                    } else {
                        "endNode"
                    }
                ))),
            },
            "length" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Path) => {
                    let elements = validated_path_elements(v)?;
                    Ok(Value::Int(usize_to_i64(
                        (elements.len() - 1) / 2,
                        "path length",
                    )?))
                }
                Some(Value::List(_) | Value::Map(_)) => Err(CypherError::TypeError(
                    "length() argument must resolve to a scalar".into(),
                )),
                Some(_) => Err(CypherError::TypeError(
                    "length() argument must resolve to a path or null".into(),
                )),
            },
            "size" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => Ok(Value::Int(usize_to_i64(items.len(), "list size")?)),
                // AGE's size() counts string BYTES, not characters.
                Some(Value::Str(s)) => Ok(Value::Int(usize_to_i64(s.len(), "string byte size")?)),
                Some(_) => Err(CypherError::TypeError("size() unsupported argument".into())),
            },
            "coalesce" => {
                for v in &args {
                    if *v != Value::Null {
                        return Ok(v.clone());
                    }
                }
                Ok(Value::Null)
            }
            "head" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => Ok(items.first().cloned().unwrap_or(Value::Null)),
                Some(_) => Err(CypherError::TypeError(
                    "head() argument must resolve to a list or null".into(),
                )),
            },
            "last" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => Ok(items.last().cloned().unwrap_or(Value::Null)),
                Some(_) => Err(CypherError::TypeError(
                    "last() argument must resolve to a list or null".into(),
                )),
            },
            "tail" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::List(items)) => {
                    Ok(Value::List(items.iter().skip(1).cloned().collect()))
                }
                Some(_) => Err(CypherError::TypeError(
                    "tail() argument must resolve to a list or null".into(),
                )),
            },
            "reverse" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Str(s)) => Ok(Value::Str(s.chars().rev().collect())),
                Some(Value::List(items)) => Ok(Value::List(items.iter().rev().cloned().collect())),
                Some(v) => Err(unsupported_argument("reverse", v)),
            },
            "toupper" => string_fn(arg, "toUpper", str::to_uppercase),
            "tolower" => string_fn(arg, "toLower", str::to_lowercase),
            "trim" => string_fn(arg, "trim", |s| s.trim().to_string()),
            "ltrim" => string_fn(arg, "lTrim", |s| s.trim_start().to_string()),
            "rtrim" => string_fn(arg, "rTrim", |s| s.trim_end().to_string()),
            "left" | "right" => {
                let (Some(first), second) = (args.first(), args.get(1)) else {
                    return Err(unsupported_argument(&name, &Value::Null));
                };
                match (first, second) {
                    (Value::Null, _) => Ok(Value::Null),
                    (Value::Str(s), Some(Value::Int(n))) => {
                        if *n < 0 {
                            return Err(CypherError::TypeError(format!(
                                "{name}() negative values are not supported for length"
                            )));
                        }
                        let n = nonnegative_i64_to_usize(*n, &format!("{name} length"))?;
                        let chars: Vec<char> = s.chars().collect();
                        let taken: String = if name == "left" {
                            chars.iter().take(n).collect()
                        } else {
                            let skip = chars.len().saturating_sub(n);
                            chars.iter().skip(skip).collect()
                        };
                        Ok(Value::Str(taken))
                    }
                    (v, _) => Err(unsupported_argument(&name, v)),
                }
            }
            "substring" => {
                let Some(first) = args.first() else {
                    return Err(unsupported_argument("substring", &Value::Null));
                };
                match first {
                    Value::Null => Ok(Value::Null),
                    Value::Str(s) => {
                        let start = match args.get(1) {
                            Some(Value::Int(n)) => *n,
                            Some(Value::Null) | None => return Ok(Value::Null),
                            Some(v) => return Err(unsupported_argument("substring", v)),
                        };
                        let count = match args.get(2) {
                            Some(Value::Int(n)) => Some(*n),
                            None => None,
                            Some(Value::Null) => return Ok(Value::Null),
                            Some(v) => return Err(unsupported_argument("substring", v)),
                        };
                        if start < 0 || count.is_some_and(|c| c < 0) {
                            return Err(CypherError::TypeError(
                                "substring() negative values are not supported for offset or length"
                                    .into(),
                            ));
                        }
                        let chars: Vec<char> = s.chars().collect();
                        let start = nonnegative_i64_to_usize(start, "substring offset")?;
                        let out: String = match count {
                            Some(c) => chars
                                .iter()
                                .skip(start)
                                .take(nonnegative_i64_to_usize(c, "substring length")?)
                                .collect(),
                            None => chars.iter().skip(start).collect(),
                        };
                        Ok(Value::Str(out))
                    }
                    v => Err(unsupported_argument("substring", v)),
                }
            }
            "split" => match (args.first(), args.get(1)) {
                (Some(Value::Null) | None, _) | (_, Some(Value::Null) | None) => Ok(Value::Null),
                (Some(Value::Str(s)), Some(Value::Str(sep))) => Ok(Value::List(
                    s.split(sep.as_str())
                        .map(|part| Value::Str(part.to_string()))
                        .collect(),
                )),
                (Some(v), _) => Err(unsupported_argument("split", v)),
            },
            "replace" => match (args.first(), args.get(1), args.get(2)) {
                (Some(Value::Null) | None, _, _)
                | (_, Some(Value::Null), _)
                | (_, _, Some(Value::Null)) => Ok(Value::Null),
                (Some(Value::Str(s)), Some(Value::Str(from)), Some(Value::Str(to))) => {
                    Ok(Value::Str(s.replace(from.as_str(), to.as_str())))
                }
                (Some(v), _, _) => Err(unsupported_argument("replace", v)),
            },
            "tostring" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Str(s)) => Ok(Value::Str(s.clone())),
                Some(Value::Int(n)) => Ok(Value::Str(n.to_string())),
                // AGE's toString uses raw float8out (no `.0` suffix).
                Some(Value::Float(f)) => Ok(Value::Str(agtype::format_float_pg(*f))),
                Some(Value::Bool(b)) => Ok(Value::Str(b.to_string())),
                Some(Value::Decimal(d)) => Ok(Value::Str(d.to_sql_string())),
                Some(v) => Err(unsupported_argument("toString", v)),
            },
            "tointeger" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Int(*n)),
                // toInteger truncates toward zero (AGE: toInteger(-4.9) = -4).
                Some(Value::Float(f)) => Ok(Value::Int(trunc_f64_to_i64(*f, "toInteger input")?)),
                Some(Value::Str(s)) => {
                    let input = s.trim();
                    if let Ok(value) = input.parse::<i64>() {
                        Ok(Value::Int(value))
                    } else if let Ok(value) = input.parse::<f64>() {
                        Ok(Value::Int(trunc_f64_to_i64(value, "toInteger input")?))
                    } else {
                        Ok(Value::Null)
                    }
                }
                Some(v) => Err(unsupported_argument("toInteger", v)),
            },
            "tofloat" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Float(exact_i64_to_f64(*n, "toFloat input")?)),
                Some(Value::Float(f)) => Ok(Value::Float(*f)),
                Some(Value::Str(s)) => {
                    Ok(s.trim().parse::<f64>().map_or(Value::Null, Value::Float))
                }
                Some(v) => Err(unsupported_argument("toFloat", v)),
            },
            "toboolean" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Bool(b)) => Ok(Value::Bool(*b)),
                Some(Value::Int(n)) => Ok(Value::Bool(*n != 0)),
                Some(Value::Str(s)) => Ok(match s.to_lowercase().as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => Value::Null,
                }),
                Some(v) => Err(unsupported_argument("toBoolean", v)),
            },
            "abs" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Int(n.wrapping_abs())),
                Some(Value::Float(f)) => Ok(Value::Float(f.abs())),
                Some(v) => Err(unsupported_argument("abs", v)),
            },
            "sign" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(Value::Int(n)) => Ok(Value::Int(n.signum())),
                Some(Value::Float(f)) => Ok(Value::Int(if *f > 0.0 {
                    1
                } else if *f < 0.0 {
                    -1
                } else {
                    0
                })),
                Some(v) => Err(unsupported_argument("sign", v)),
            },
            "ceil" => float_fn(arg, "ceil", f64::ceil),
            "floor" => float_fn(arg, "floor", f64::floor),
            "round" => float_fn(arg, "round", f64::round),
            "sqrt" => domain_float_fn(arg, "sqrt", |f| (f >= 0.0).then(|| f.sqrt())),
            "log" => domain_float_fn(arg, "log", |f| (f > 0.0).then(|| f.ln())),
            "log10" => domain_float_fn(arg, "log10", |f| (f > 0.0).then(|| f.log10())),
            "exp" => float_fn(arg, "exp", f64::exp),
            "e" => Ok(Value::Float(std::f64::consts::E)),
            "pi" => Ok(Value::Float(std::f64::consts::PI)),
            "range" => {
                let (start, end) = match (args.first(), args.get(1)) {
                    (Some(Value::Int(a)), Some(Value::Int(b))) => (*a, *b),
                    _ => {
                        return Err(CypherError::TypeError(
                            "range() unsupported argument type".into(),
                        ));
                    }
                };
                let step = match args.get(2) {
                    Some(Value::Int(s)) => *s,
                    None => 1,
                    Some(_) => {
                        return Err(CypherError::TypeError(
                            "range() unsupported argument type".into(),
                        ));
                    }
                };
                if step == 0 {
                    return Err(CypherError::TypeError(
                        "range(): step cannot be zero".into(),
                    ));
                }
                let mut out = Vec::new();
                let mut current = start;
                // range() is end-INCLUSIVE in AGE.
                while (step > 0 && current <= end) || (step < 0 && current >= end) {
                    out.push(Value::Int(current));
                    if current == end {
                        break;
                    }
                    let Some(next) = current.checked_add(step) else {
                        break;
                    };
                    current = next;
                }
                Ok(Value::List(out))
            }
            "timestamp" => {
                let duration = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(|error| {
                        CypherError::Storage(format!("system clock precedes Unix epoch: {error}"))
                    })?;
                let ms = i64::try_from(duration.as_millis()).map_err(|_| {
                    CypherError::TypeError("timestamp exceeds agtype integer range".into())
                })?;
                Ok(Value::Int(ms))
            }
            "nodes" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Path) => {
                    let elements = validated_path_elements(v)?;
                    Ok(Value::List(elements.iter().step_by(2).cloned().collect()))
                }
                Some(_) => Err(CypherError::TypeError(
                    "nodes() argument must be a path".into(),
                )),
            },
            "relationships" => match arg {
                Some(Value::Null) | None => Ok(Value::Null),
                Some(v) if agtype::entity_kind(v) == Some(agtype::EntityKind::Path) => {
                    let elements = validated_path_elements(v)?;
                    Ok(Value::List(
                        elements.iter().skip(1).step_by(2).cloned().collect(),
                    ))
                }
                Some(_) => Err(CypherError::TypeError(
                    "relationships() argument must be a path".into(),
                )),
            },
            other => Err(CypherError::Unsupported(format!("function {other}"))),
        }
    }
}
