//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared Cypher value, sorting, aggregate, arithmetic, and string helpers.

use super::{
    agtype, exact_i64_to_f64, usize_to_i64, CypherError, CypherExpr, OrderByItem, PathElement,
    PathPattern, ReturnItem, Value,
};

// ------------------------------------------------------------------
// Helpers
// ------------------------------------------------------------------

pub(super) fn validated_path_elements(value: &Value) -> Result<&[Value], CypherError> {
    let elements = agtype::path_elements(value)
        .ok_or_else(|| CypherError::Storage("path entity is missing its elements".into()))?;
    if elements.is_empty() || elements.len() % 2 == 0 {
        return Err(CypherError::Storage(format!(
            "path entity has invalid element count {}",
            elements.len()
        )));
    }
    for (index, element) in elements.iter().enumerate() {
        let expected = if index % 2 == 0 {
            agtype::EntityKind::Vertex
        } else {
            agtype::EntityKind::Edge
        };
        if agtype::entity_kind(element) != Some(expected) {
            return Err(CypherError::Storage(format!(
                "path entity element {index} is not a {}",
                if index % 2 == 0 {
                    "vertex"
                } else {
                    "relationship"
                }
            )));
        }
    }
    Ok(elements)
}

/// Variables declared by a set of path patterns (node, relationship,
/// and path variables), used to pad OPTIONAL MATCH misses with nulls.
pub(super) fn pattern_variables(patterns: &[PathPattern]) -> Vec<String> {
    let mut vars = Vec::new();
    for pattern in patterns {
        if let Some(v) = &pattern.variable {
            vars.push(v.clone());
        }
        for element in &pattern.elements {
            match element {
                PathElement::Node(np) => {
                    if let Some(v) = &np.variable {
                        vars.push(v.clone());
                    }
                }
                PathElement::Rel(rp) => {
                    if let Some(v) = &rp.variable {
                        vars.push(v.clone());
                    }
                }
            }
        }
    }
    vars
}

pub(super) fn null_or_bool(lhs: &Value, rhs: &Value, result: bool) -> Value {
    if *lhs == Value::Null || *rhs == Value::Null {
        Value::Null
    } else {
        Value::Bool(result)
    }
}

pub(super) fn sort_keyed<R>(keyed: &mut [(Vec<Value>, R)], order: &[OrderByItem]) {
    keyed.sort_by(|a, b| {
        for (i, (av, bv)) in a.0.iter().zip(b.0.iter()).enumerate() {
            let cmp = agtype::cmp(av, bv);
            let cmp = if order.get(i).is_some_and(|o| !o.ascending) {
                cmp.reverse()
            } else {
                cmp
            };
            if cmp != std::cmp::Ordering::Equal {
                return cmp;
            }
        }
        std::cmp::Ordering::Equal
    });
}

pub(super) fn return_label(item: &ReturnItem, position: usize) -> String {
    if let Some(alias) = &item.alias {
        return alias.clone();
    }
    match &item.expr {
        CypherExpr::Variable(v) => v.name.clone(),
        CypherExpr::PropertyAccess(p) => format!("{}.{}", p.variable, p.keys.join(".")),
        CypherExpr::FunctionCall(f) => f.name.clone(),
        _ => format!("expr_{position}"),
    }
}

pub(super) fn is_aggregate(expr: &CypherExpr) -> bool {
    if let CypherExpr::FunctionCall(fc) = expr {
        is_aggregate_name(&fc.name)
    } else {
        false
    }
}

pub(super) fn is_aggregate_name(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "count" | "sum" | "avg" | "min" | "max" | "collect"
    )
}

pub(super) fn number_as_f64(v: &Value) -> Result<Option<f64>, CypherError> {
    match v {
        Value::Int(n) => exact_i64_to_f64(*n, "integer operand").map(Some),
        Value::Float(f) => Ok(Some(*f)),
        Value::Decimal(d) => d.to_f64().map(Some).ok_or_else(|| {
            CypherError::TypeError(format!(
                "numeric value {d:?} cannot be represented as a float"
            ))
        }),
        _ => Ok(None),
    }
}

/// min / max over non-null values in agtype order, preserving the
/// original value type. Empty input yields null.
pub(super) fn aggregate_extreme(values: &[Value], want_min: bool) -> Value {
    let mut best: Option<&Value> = None;
    for v in values {
        let replace = match best {
            None => true,
            Some(current) => {
                let cmp = agtype::cmp(v, current);
                if want_min {
                    cmp == std::cmp::Ordering::Less
                } else {
                    cmp == std::cmp::Ordering::Greater
                }
            }
        };
        if replace {
            best = Some(v);
        }
    }
    best.cloned().unwrap_or(Value::Null)
}

/// sum keeps integer typing while every input is an integer (AGE:
/// `sum([1,2,3])` = 6, `sum([1,2.5])` = 3.5); empty input yields null.
pub(super) fn aggregate_sum(values: &[Value]) -> Result<Value, CypherError> {
    if values.is_empty() {
        return Ok(Value::Null);
    }
    if values.iter().all(|value| matches!(value, Value::Int(_))) {
        let mut sum = 0_i64;
        for value in values {
            let Value::Int(integer) = value else {
                return Err(CypherError::Storage(
                    "integer aggregate validation became inconsistent".into(),
                ));
            };
            sum = sum.wrapping_add(*integer);
        }
        return Ok(Value::Int(sum));
    }

    let mut sum = 0.0;
    for value in values {
        sum += number_as_f64(value)?
            .ok_or_else(|| CypherError::TypeError("arguments must resolve to a number".into()))?;
    }
    Ok(Value::Float(sum))
}

pub(super) fn aggregate_avg(values: &[Value]) -> Result<Value, CypherError> {
    if values.is_empty() {
        return Ok(Value::Null);
    }
    let mut total = 0.0;
    for v in values {
        total += number_as_f64(v)?
            .ok_or_else(|| CypherError::TypeError("arguments must resolve to a number".into()))?;
    }
    let count = exact_i64_to_f64(
        usize_to_i64(values.len(), "average count")?,
        "average count",
    )?;
    Ok(Value::Float(total / count))
}

/// Concatenation contribution of a scalar joined to a string with `+`.
/// AGE quirk (verified): booleans contribute an empty string.
pub(super) fn concat_fragment(v: &Value) -> Option<String> {
    match v {
        Value::Str(s) => Some(s.clone()),
        Value::Int(n) => Some(n.to_string()),
        Value::Float(f) => Some(agtype::format_float_pg(*f)),
        Value::Bool(_) => Some(String::new()),
        _ => None,
    }
}

pub(super) fn agtype_add(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    match (lhs, rhs) {
        (Value::Int(a), Value::Int(b)) => Ok(Value::Int(a.wrapping_add(*b))),
        (Value::List(a), Value::List(b)) => {
            let mut out = a.clone();
            out.extend(b.iter().cloned());
            Ok(Value::List(out))
        }
        // `[1, 2] + 3` appends, `3 + [1, 2]` prepends.
        (Value::List(a), b) => {
            let mut out = a.clone();
            out.push(b.clone());
            Ok(Value::List(out))
        }
        (a, Value::List(b)) => {
            let mut out = vec![a.clone()];
            out.extend(b.iter().cloned());
            Ok(Value::List(out))
        }
        (Value::Map(a), Value::Map(b)) => {
            let mut out = a.clone();
            for (k, v) in b {
                out.insert(k.clone(), v.clone());
            }
            Ok(Value::Map(out))
        }
        (Value::Str(_), _) | (_, Value::Str(_)) => {
            match (concat_fragment(lhs), concat_fragment(rhs)) {
                (Some(a), Some(b)) => Ok(Value::Str(format!("{a}{b}"))),
                _ => Err(CypherError::TypeError(
                    "Invalid input parameter types for agtype_add".into(),
                )),
            }
        }
        _ => match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
            (Some(a), Some(b)) => Ok(Value::Float(a + b)),
            _ => Err(CypherError::TypeError(
                "Invalid input parameter types for agtype_add".into(),
            )),
        },
    }
}

pub(super) fn numeric_op(
    lhs: &Value,
    rhs: &Value,
    age_name: &str,
    f_int: impl Fn(i64, i64) -> i64,
    f_float: impl Fn(f64, f64) -> f64,
) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        return Ok(Value::Int(f_int(*a, *b)));
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        (Some(a), Some(b)) => Ok(Value::Float(f_float(a, b))),
        _ => Err(CypherError::TypeError(format!(
            "Invalid input parameter types for {age_name}"
        ))),
    }
}

pub(super) fn agtype_div(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        if *b == 0 {
            return Err(CypherError::TypeError("division by zero".into()));
        }
        return Ok(Value::Int(a.wrapping_div(*b)));
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        (Some(a), Some(b)) => {
            if b == 0.0 {
                return Err(CypherError::TypeError("division by zero".into()));
            }
            Ok(Value::Float(a / b))
        }
        _ => Err(CypherError::TypeError(
            "Invalid input parameter types for agtype_div".into(),
        )),
    }
}

pub(super) fn agtype_mod(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    if let (Value::Int(a), Value::Int(b)) = (lhs, rhs) {
        // AGE quirk (verified on 1.6.0): integer modulo by zero
        // returns the dividend instead of raising.
        if *b == 0 {
            return Ok(Value::Int(*a));
        }
        return Ok(Value::Int(a.wrapping_rem(*b)));
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        // fmod semantics: sign follows the dividend; x % 0.0 = NaN.
        (Some(a), Some(b)) => Ok(Value::Float(a % b)),
        _ => Err(CypherError::TypeError(
            "Invalid input parameter types for agtype_mod".into(),
        )),
    }
}

pub(super) fn agtype_pow(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    if *lhs == Value::Null || *rhs == Value::Null {
        return Ok(Value::Null);
    }
    match (number_as_f64(lhs)?, number_as_f64(rhs)?) {
        // `^` ALWAYS yields a float in AGE (2^2 = 4.0).
        (Some(a), Some(b)) => Ok(Value::Float(a.powf(b))),
        _ => Err(CypherError::TypeError(
            "Invalid input parameter types for agtype_pow".into(),
        )),
    }
}

/// STARTS WITH / ENDS WITH / CONTAINS: null propagates, non-string
/// operands compare false (verified: `'abc' STARTS WITH 1` = false).
pub(super) fn str_predicate(lhs: &Value, rhs: &Value, f: impl Fn(&str, &str) -> bool) -> Value {
    match (lhs, rhs) {
        (Value::Null, _) | (_, Value::Null) => Value::Null,
        (Value::Str(a), Value::Str(b)) => Value::Bool(f(a, b)),
        _ => Value::Bool(false),
    }
}

/// `=~` is an UNANCHORED regular-expression search in AGE
/// (`PostgreSQL` `~` semantics): `'abc' =~ 'b'` is true. Non-string
/// operands (including null) yield null.
pub(super) fn regex_match(lhs: &Value, rhs: &Value) -> Result<Value, CypherError> {
    match (lhs, rhs) {
        (Value::Str(a), Value::Str(pattern)) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| CypherError::TypeError(format!("invalid regular expression: {e}")))?;
            Ok(Value::Bool(re.is_match(a)))
        }
        _ => Ok(Value::Null),
    }
}

pub(super) fn unsupported_argument(function: &str, value: &Value) -> CypherError {
    CypherError::TypeError(format!(
        "{function}() unsupported argument agtype {}",
        agtype::agtype_type_ordinal(value)
    ))
}

pub(super) fn string_fn(
    arg: Option<&Value>,
    name: &str,
    f: impl Fn(&str) -> String,
) -> Result<Value, CypherError> {
    match arg {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(Value::Str(s)) => Ok(Value::Str(f(s))),
        Some(v) => Err(unsupported_argument(name, v)),
    }
}

/// Numeric function that always yields a float (AGE: `ceil(2)` = 2.0).
pub(super) fn float_fn(
    arg: Option<&Value>,
    name: &str,
    f: impl Fn(f64) -> f64,
) -> Result<Value, CypherError> {
    match arg {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(v) => match number_as_f64(v)? {
            Some(x) => Ok(Value::Float(f(x))),
            None => Err(unsupported_argument(name, v)),
        },
    }
}

/// Numeric function with a restricted domain; out-of-domain inputs
/// return null (AGE: `sqrt(-1)` = null, `log(0)` = null).
pub(super) fn domain_float_fn(
    arg: Option<&Value>,
    name: &str,
    f: impl Fn(f64) -> Option<f64>,
) -> Result<Value, CypherError> {
    match arg {
        Some(Value::Null) | None => Ok(Value::Null),
        Some(v) => match number_as_f64(v)? {
            Some(x) => Ok(f(x).map_or(Value::Null, Value::Float)),
            None => Err(unsupported_argument(name, v)),
        },
    }
}
