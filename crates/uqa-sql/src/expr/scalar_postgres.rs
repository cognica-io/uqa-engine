//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Extended `PostgreSQL` scalar and lowered operator built-ins.

use super::{
    allocation_error, compile_pg_regex, eval_between, eval_comparison_op, expect_str,
    json_contained_by, json_contains, nonnegative_usize, out_of_range, quote_ident, quote_literal,
    similar_to_regex, to_i64, value_to_string, values_equal, ArrayValue, BinaryOp, DecimalValue,
    Result, SQLError, Value, TO_BIN_INT4_FUNCTION, TO_BIN_INT8_FUNCTION, TO_HEX_INT4_FUNCTION,
    TO_HEX_INT8_FUNCTION, TO_OCT_INT4_FUNCTION, TO_OCT_INT8_FUNCTION,
};

pub(super) fn eval_postgres_functions(name: &str, args: &[Value]) -> Option<Result<Value>> {
    const NAMES: &[&str] = &[
        "factorial",
        "bit_length",
        "to_bin",
        TO_BIN_INT4_FUNCTION,
        TO_BIN_INT8_FUNCTION,
        "to_hex",
        TO_HEX_INT4_FUNCTION,
        TO_HEX_INT8_FUNCTION,
        "to_oct",
        TO_OCT_INT4_FUNCTION,
        TO_OCT_INT8_FUNCTION,
        "string_to_array",
        "string_to_table",
        "quote_ident",
        "quote_literal",
        "quote_nullable",
        "regexp_count",
        "regexp_instr",
        "regexp_like",
        "regexp_substr",
        "similar_to",
        "num_nulls",
        "num_nonnulls",
        "current_database",
        "current_catalog",
        "current_user",
        "session_user",
        "array_positions",
        "array_replace",
        "array_to_string",
        "array_fill",
        "trim_array",
        "array_sample",
        "array_overlap",
        "contains_op",
        "contained_by_op",
        "__array_subscripts",
        "__array_slices",
        "__subscript",
        "__slice",
        "__any_op",
        "__all_op",
        "__is_distinct",
        "__between_symmetric",
    ];
    if !NAMES.contains(&name) {
        return None;
    }
    Some((|| -> Result<Value> {
        match name {
            // -------------------------------------------------------------
            // PostgreSQL scalar surface: math, strings, arrays, operators
            // lowered to internal functions.
            // -------------------------------------------------------------
            "factorial" => {
                if args.len() != 1 {
                    return Err(SQLError::TypeMismatch("factorial takes 1 arg".into()));
                }
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let n = to_i64(&args[0])?;
                if n < 0 {
                    return Err(SQLError::Routine {
                        sqlstate: "2201F".into(),
                        message: "factorial of a negative number is undefined".into(),
                    });
                }
                let mut acc: i128 = 1;
                for k in 2..=n as i128 {
                    acc = acc.checked_mul(k).ok_or_else(|| out_of_range("numeric"))?;
                }
                if let Ok(small) = i64::try_from(acc) {
                    return Ok(Value::Int(small));
                }
                DecimalValue::parse(&acc.to_string())
                    .map(Value::Decimal)
                    .ok_or_else(|| out_of_range("numeric"))
            }
            "bit_length" => {
                let [value] = args else {
                    return Err(SQLError::TypeMismatch("bit_length takes 1 arg".into()));
                };
                let octets = match value {
                    Value::Null => return Ok(Value::Null),
                    Value::Str(text) => text.len(),
                    Value::FixedChar(text) => text.trim_end_matches(' ').len(),
                    Value::Bytes(bytes) => bytes.len(),
                    _ => {
                        return Err(SQLError::TypeMismatch(
                            "bit_length requires text or bytea".into(),
                        ));
                    }
                };
                Ok(Value::Int(octets as i64 * 8))
            }
            "to_bin" | TO_BIN_INT4_FUNCTION | TO_BIN_INT8_FUNCTION | "to_hex"
            | TO_HEX_INT4_FUNCTION | TO_HEX_INT8_FUNCTION | "to_oct" | TO_OCT_INT4_FUNCTION
            | TO_OCT_INT8_FUNCTION => {
                let [argument] = args else {
                    return Err(SQLError::TypeMismatch(format!("{name} takes 1 arg")));
                };
                if matches!(argument, Value::Null) {
                    return Ok(Value::Null);
                }
                let value = to_i64(argument)?;
                match name {
                    TO_BIN_INT4_FUNCTION => {
                        let value = i32::try_from(value).map_err(|_| out_of_range("integer"))?;
                        Ok(Value::Str(format!("{:b}", value as u32)))
                    }
                    TO_BIN_INT8_FUNCTION => Ok(Value::Str(format!("{:b}", value as u64))),
                    TO_HEX_INT4_FUNCTION => {
                        let value = i32::try_from(value).map_err(|_| out_of_range("integer"))?;
                        Ok(Value::Str(format!("{:x}", value as u32)))
                    }
                    TO_HEX_INT8_FUNCTION => Ok(Value::Str(format!("{:x}", value as u64))),
                    TO_OCT_INT4_FUNCTION => {
                        let value = i32::try_from(value).map_err(|_| out_of_range("integer"))?;
                        Ok(Value::Str(format!("{:o}", value as u32)))
                    }
                    TO_OCT_INT8_FUNCTION => Ok(Value::Str(format!("{:o}", value as u64))),
                    _ => Err(SQLError::Internal(format!(
                        "{name} reached runtime before its integer overload was bound"
                    ))),
                }
            }
            "string_to_array" | "string_to_table" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch(
                        "string_to_array takes 2-3 args".into(),
                    ));
                }
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let null_marker = args.get(2).filter(|v| !matches!(v, Value::Null));
                let mark = |part: &str| -> Value {
                    if let Some(marker) = null_marker {
                        if part == value_to_string(marker) {
                            return Value::Null;
                        }
                    }
                    Value::Str(part.to_string())
                };
                let items: Vec<Value> = match &args[1] {
                    // NULL separator: split into individual characters.
                    Value::Null => s.chars().map(|c| mark(&c.to_string())).collect(),
                    sep => {
                        let sep = value_to_string(sep);
                        if s.is_empty() {
                            Vec::new()
                        } else if sep.is_empty() {
                            vec![mark(&s)]
                        } else {
                            s.split(sep.as_str()).map(mark).collect()
                        }
                    }
                };
                ArrayValue::try_new(items)
                    .map(Value::Array)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid string_to_array result".into()))
            }
            "quote_ident" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Str(quote_ident(&expect_str(args, 0)?)))
            }
            "quote_literal" => {
                if matches!(args.first(), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                Ok(Value::Str(quote_literal(&expect_str(args, 0)?)))
            }
            "quote_nullable" => match args.first() {
                Some(Value::Null) | None => Ok(Value::Str("NULL".into())),
                Some(other) => Ok(Value::Str(quote_literal(&value_to_string(other)))),
            },
            "regexp_count" => {
                if args.len() < 2 || args.len() > 4 {
                    return Err(SQLError::TypeMismatch("regexp_count takes 2-4 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let start = positive_regex_parameter(args.get(2), 1, "start")?;
                let flags = args.get(3).map(value_to_string).unwrap_or_default();
                let re = compile_pg_regex(&pat, &flags, false)?;
                let Some((tail, _)) = regex_tail(&s, start) else {
                    return Ok(Value::Int(0));
                };
                Ok(Value::Int(re.find_iter(tail).count() as i64))
            }
            "regexp_instr" => {
                if args.len() < 2 || args.len() > 7 {
                    return Err(SQLError::TypeMismatch("regexp_instr takes 2-7 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let string = value_to_string(&args[0]);
                let pattern = value_to_string(&args[1]);
                let start = positive_regex_parameter(args.get(2), 1, "start")?;
                let occurrence = positive_regex_parameter(args.get(3), 1, "N")?;
                let end_option = args.get(4).map(to_i64).transpose()?.unwrap_or(0);
                if !matches!(end_option, 0 | 1) {
                    return Err(invalid_regex_parameter("endoption", end_option));
                }
                let flags = args.get(5).map(value_to_string).unwrap_or_default();
                let subexpression = nonnegative_regex_parameter(args.get(6), 0, "subexpr")?;
                let re = compile_pg_regex(&pattern, &flags, false)?;
                let Some((tail, base_chars)) = regex_tail(&string, start) else {
                    return Ok(Value::Int(0));
                };
                let Some(captures) = re.captures_iter(tail).nth(occurrence - 1) else {
                    return Ok(Value::Int(0));
                };
                let Some(selected) = captures.get(subexpression) else {
                    return Ok(Value::Int(0));
                };
                let byte_offset = if end_option == 0 {
                    selected.start()
                } else {
                    selected.end()
                };
                let position = base_chars
                    .checked_add(tail[..byte_offset].chars().count())
                    .and_then(|position| position.checked_add(1))
                    .ok_or_else(|| out_of_range("integer"))?;
                Ok(Value::Int(
                    i64::try_from(position).map_err(|_| out_of_range("integer"))?,
                ))
            }
            "regexp_like" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch("regexp_like takes 2-3 args".into()));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let pat = value_to_string(&args[1]);
                let flags = args.get(2).map(value_to_string).unwrap_or_default();
                let re = compile_pg_regex(&pat, &flags, false)?;
                Ok(Value::Bool(re.is_match(&s)))
            }
            "regexp_substr" => {
                if args.len() < 2 || args.len() > 6 {
                    return Err(SQLError::TypeMismatch(
                        "regexp_substr takes 2-6 args".into(),
                    ));
                }
                if args.iter().any(|arg| matches!(arg, Value::Null)) {
                    return Ok(Value::Null);
                }
                let string = value_to_string(&args[0]);
                let pattern = value_to_string(&args[1]);
                let start = positive_regex_parameter(args.get(2), 1, "start")?;
                let occurrence = positive_regex_parameter(args.get(3), 1, "N")?;
                let flags = args.get(4).map(value_to_string).unwrap_or_default();
                let subexpression = nonnegative_regex_parameter(args.get(5), 0, "subexpr")?;
                let re = compile_pg_regex(&pattern, &flags, false)?;
                let Some((tail, _)) = regex_tail(&string, start) else {
                    return Ok(Value::Null);
                };
                let Some(captures) = re.captures_iter(tail).nth(occurrence - 1) else {
                    return Ok(Value::Null);
                };
                Ok(captures
                    .get(subexpression)
                    .map(|matched| Value::Str(matched.as_str().to_string()))
                    .unwrap_or(Value::Null))
            }
            "similar_to" => {
                // SIMILAR TO: SQL regex anchored over the whole string.
                if !matches!(args.len(), 2 | 3) {
                    return Err(SQLError::TypeMismatch(
                        "similar_to takes 2 or 3 args".into(),
                    ));
                }
                if matches!(args[1], Value::Null) || matches!(args.get(2), Some(Value::Null)) {
                    return Ok(Value::Null);
                }
                let escape = args.get(2).map(value_to_string);
                let pat = similar_to_regex(&value_to_string(&args[1]), escape.as_deref())?;
                if matches!(args[0], Value::Null) {
                    return Ok(Value::Null);
                }
                let s = value_to_string(&args[0]);
                let re = compile_pg_regex(&pat, "", false)?;
                Ok(Value::Bool(re.is_match(&s)))
            }
            "num_nulls" => Ok(Value::Int(
                args.iter().filter(|v| matches!(v, Value::Null)).count() as i64,
            )),
            "num_nonnulls" => Ok(Value::Int(
                args.iter().filter(|v| !matches!(v, Value::Null)).count() as i64,
            )),
            // The engine has one database and one logical user identity; schema
            // identifiers are intercepted above because they are session-scoped.
            "current_database" | "current_catalog" => Ok(Value::Str("uqa".into())),
            "current_user" | "session_user" => Ok(Value::Str("uqa".into())),
            "array_positions" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "array_positions takes 2 args".into(),
                    ));
                }
                match &args[0] {
                    Value::Array(array) if array.dimensions().len() <= 1 => {
                        let lower = i64::from(array.lower_bound(0).unwrap_or(1));
                        let positions = array
                            .elements()
                            .iter()
                            .enumerate()
                            .filter(|(_, value)| *value == &args[1])
                            .map(|(index, _)| {
                                i64::try_from(index)
                                    .ok()
                                    .and_then(|index| lower.checked_add(index))
                                    .map(Value::Int)
                                    .ok_or_else(|| out_of_range("array position"))
                            })
                            .collect::<Result<Vec<_>>>()?;
                        ArrayValue::try_new(positions)
                            .map(Value::Array)
                            .ok_or_else(|| SQLError::TypeMismatch("invalid array result".into()))
                    }
                    Value::Array(_) => Err(SQLError::TypeMismatch(
                        "searching for elements in multidimensional arrays is not supported".into(),
                    )),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_positions: not an array {other:?}"
                    ))),
                }
            }
            "array_replace" => {
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("array_replace takes 3 args".into()));
                }
                match &args[0] {
                    Value::Array(array) => rebuild_array_value(
                        array,
                        replace_array_elements(array.elements(), &args[1], &args[2]),
                    ),
                    Value::Null => Ok(Value::Null),
                    other => Err(SQLError::TypeMismatch(format!(
                        "array_replace: not an array {other:?}"
                    ))),
                }
            }
            "array_to_string" => {
                if args.len() < 2 || args.len() > 3 {
                    return Err(SQLError::TypeMismatch(
                        "array_to_string takes 2-3 args".into(),
                    ));
                }
                let Value::Array(array) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch(format!(
                        "array_to_string: not an array {:?}",
                        args[0]
                    )));
                };
                if matches!(args[1], Value::Null) {
                    return Ok(Value::Null);
                }
                let sep = value_to_string(&args[1]);
                let null_text = args.get(2).filter(|v| !matches!(v, Value::Null));
                let mut flattened = Vec::new();
                flatten_array_elements(array.elements(), &mut flattened);
                let mut parts: Vec<String> = Vec::with_capacity(flattened.len());
                for item in flattened {
                    if matches!(item, Value::Null) {
                        if let Some(marker) = null_text {
                            parts.push(value_to_string(marker));
                        }
                        continue;
                    }
                    parts.push(value_to_string(item));
                }
                Ok(Value::Str(parts.join(&sep)))
            }
            "array_fill" => {
                if !(2..=3).contains(&args.len()) {
                    return Err(SQLError::TypeMismatch(
                        "array_fill takes 2 or 3 args".into(),
                    ));
                }
                if matches!(args[1], Value::Null)
                    || args
                        .get(2)
                        .is_some_and(|value| matches!(value, Value::Null))
                {
                    return Ok(Value::Null);
                }
                let Value::Array(dimensions_array) = &args[1] else {
                    return Err(SQLError::TypeMismatch(
                        "array_fill: dimensions must be an integer array".into(),
                    ));
                };
                if dimensions_array.dimensions().len() != 1 {
                    return Err(SQLError::TypeMismatch(
                        "array_fill: dimensions must be one-dimensional".into(),
                    ));
                }
                let dimensions = dimensions_array
                    .elements()
                    .iter()
                    .map(|dimension| nonnegative_usize(to_i64(dimension)?, "array_fill dimension"))
                    .collect::<Result<Vec<_>>>()?;
                if dimensions.is_empty() || dimensions.len() > 6 {
                    return Err(SQLError::TypeMismatch(
                        "array_fill requires between 1 and 6 dimensions".into(),
                    ));
                }
                let lower_bounds = match args.get(2) {
                    None => vec![1; dimensions.len()],
                    Some(Value::Array(bounds)) if bounds.dimensions().len() == 1 => bounds
                        .elements()
                        .iter()
                        .map(|bound| {
                            i32::try_from(to_i64(bound)?)
                                .map_err(|_| out_of_range("array lower bound"))
                        })
                        .collect::<Result<Vec<_>>>()?,
                    Some(_) => {
                        return Err(SQLError::TypeMismatch(
                            "array_fill: lower bounds must be a one-dimensional integer array"
                                .into(),
                        ))
                    }
                };
                if lower_bounds.len() != dimensions.len() {
                    return Err(SQLError::TypeMismatch(
                        "wrong number of array subscripts".into(),
                    ));
                }
                if dimensions.contains(&0) {
                    return ArrayValue::try_new(Vec::new())
                        .map(Value::Array)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid empty array".into()));
                }
                let elements = filled_array_elements(&args[0], &dimensions)?;
                ArrayValue::with_lower_bounds(elements, lower_bounds)
                    .map(Value::Array)
                    .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))
            }
            "trim_array" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("trim_array takes 2 args".into()));
                }
                let Value::Array(array) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("trim_array: not an array".into()));
                };
                let n = to_i64(&args[1])?;
                let n = usize::try_from(n).ok();
                if n.is_none_or(|n| n > array.elements().len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "2202E".into(),
                        message: format!(
                            "number of elements to trim must be between 0 and {}",
                            array.elements().len()
                        ),
                    });
                }
                let n = n.ok_or_else(|| out_of_range("array trim count"))?;
                rebuild_array_with_bounds(
                    array.elements()[..array.elements().len() - n].to_vec(),
                    vec![1; array.lower_bounds().len()],
                )
            }
            "array_sample" => {
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array_sample takes 2 args".into()));
                }
                let Value::Array(array) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("array_sample: not an array".into()));
                };
                let n = to_i64(&args[1])?;
                let n = usize::try_from(n).ok();
                if n.is_none_or(|n| n > array.elements().len()) {
                    return Err(SQLError::Routine {
                        sqlstate: "22023".into(),
                        message: format!(
                            "sample size must be between 0 and {}",
                            array.elements().len()
                        ),
                    });
                }
                let n = n.ok_or_else(|| out_of_range("array sample size"))?;
                let mut pool = array.elements().to_vec();
                let mut out = Vec::with_capacity(n);
                let mut seed = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.subsec_nanos() as u64 | 1)
                    .unwrap_or(1);
                for _ in 0..n {
                    seed = seed
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    let idx = (seed >> 33) as usize % pool.len();
                    out.push(pool.swap_remove(idx));
                }
                let mut lower_bounds = array.lower_bounds().to_vec();
                if let Some(lower_bound) = lower_bounds.first_mut() {
                    *lower_bound = 1;
                }
                rebuild_array_with_bounds(out, lower_bounds)
            }
            "array_overlap" => {
                // `&&` operator: true when the arrays share any non-null
                // element.
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("array overlap takes 2 args".into()));
                }
                match (&args[0], &args[1]) {
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    (Value::Array(a), Value::Array(b)) => {
                        let mut left = Vec::new();
                        let mut right = Vec::new();
                        flatten_array_elements(a.elements(), &mut left);
                        flatten_array_elements(b.elements(), &mut right);
                        Ok(Value::Bool(left.iter().any(|x| {
                            !matches!(x, Value::Null) && right.iter().any(|y| values_equal(x, y))
                        })))
                    }
                    _ => Err(SQLError::TypeMismatch(
                        "array overlap: both args must be arrays".into(),
                    )),
                }
            }
            "contains_op" | "contained_by_op" => containment_operator(name, args),
            "__array_subscripts" => {
                if args.len() < 2 {
                    return Err(SQLError::TypeMismatch(
                        "array subscripting requires at least one index".into(),
                    ));
                }
                if args.iter().any(|argument| matches!(argument, Value::Null)) {
                    return Ok(Value::Null);
                }
                let Value::Array(array) = &args[0] else {
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot subscript {:?}",
                        args[0]
                    )));
                };
                array_subscripts(array, &args[1..])
            }
            "__array_slices" => {
                if args.len() < 3 || args.len().is_multiple_of(2) {
                    return Err(SQLError::TypeMismatch(
                        "array slicing requires lower/upper bound pairs".into(),
                    ));
                }
                let Value::Array(array) = &args[0] else {
                    if matches!(args[0], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch(format!(
                        "cannot slice {:?}",
                        args[0]
                    )));
                };
                array_slices(array, &args[1..])
            }
            "__subscript" => {
                // 1-based array subscripting; out-of-range yields NULL.
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch("subscript takes 2 args".into()));
                }
                match (&args[0], &args[1]) {
                    (Value::Null, _) | (_, Value::Null) => Ok(Value::Null),
                    (Value::Array(array), index) => {
                        let index = to_i64(index)?;
                        let Some(lower) = array.lower_bound(0).map(i64::from) else {
                            return Ok(Value::Null);
                        };
                        let Some(offset) = index
                            .checked_sub(lower)
                            .and_then(|offset| usize::try_from(offset).ok())
                        else {
                            return Ok(Value::Null);
                        };
                        let Some(value) = array.elements().get(offset) else {
                            return Ok(Value::Null);
                        };
                        if array.dimensions().len() == 1 {
                            return Ok(value.clone());
                        }
                        let Value::List(elements) = value else {
                            return Err(SQLError::TypeMismatch(
                                "invalid multidimensional array".into(),
                            ));
                        };
                        ArrayValue::with_lower_bounds(
                            elements.clone(),
                            array.lower_bounds()[1..].to_vec(),
                        )
                        .map(Value::Array)
                        .ok_or_else(|| {
                            SQLError::TypeMismatch("invalid multidimensional array".into())
                        })
                    }
                    (Value::Map(map), key) => Ok(map
                        .get(&value_to_string(key))
                        .cloned()
                        .unwrap_or(Value::Null)),
                    (other, _) => Err(SQLError::TypeMismatch(format!(
                        "cannot subscript {other:?}"
                    ))),
                }
            }
            "__slice" => {
                // Array slice `arr[lo:hi]`; open bounds arrive as NULL and
                // clamp to the array, PostgreSQL-style.
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("slice takes 3 args".into()));
                }
                match &args[0] {
                    Value::Null => Ok(Value::Null),
                    Value::Array(array) => {
                        let Some(array_lower) = array.lower_bound(0).map(i64::from) else {
                            return ArrayValue::try_new(Vec::new())
                                .map(Value::Array)
                                .ok_or_else(|| {
                                    SQLError::TypeMismatch("invalid empty array".into())
                                });
                        };
                        let array_upper = array.upper_bound(0).ok_or_else(|| {
                            SQLError::TypeMismatch("invalid array dimensions".into())
                        })?;
                        let lo = match &args[1] {
                            Value::Null => array_lower,
                            other => to_i64(other)?,
                        }
                        .max(array_lower);
                        let hi = match &args[2] {
                            Value::Null => array_upper,
                            other => to_i64(other)?,
                        }
                        .min(array_upper);
                        if hi < lo || lo > array_upper {
                            return ArrayValue::try_new(Vec::new())
                                .map(Value::Array)
                                .ok_or_else(|| {
                                    SQLError::TypeMismatch("invalid empty array".into())
                                });
                        }
                        let start = usize::try_from(lo - array_lower)
                            .map_err(|_| out_of_range("array slice"))?;
                        let end = usize::try_from(hi - array_lower + 1)
                            .map_err(|_| out_of_range("array slice"))?;
                        let lower_bounds = vec![1; array.lower_bounds().len()];
                        ArrayValue::with_lower_bounds(
                            array.elements()[start..end].to_vec(),
                            lower_bounds,
                        )
                        .map(Value::Array)
                        .ok_or_else(|| SQLError::TypeMismatch("invalid array slice".into()))
                    }
                    other => Err(SQLError::TypeMismatch(format!("cannot slice {other:?}"))),
                }
            }
            "__any_op" | "__all_op" => {
                // `expr op ANY(array)` / `expr op ALL(array)` with Kleene
                // aggregation over the element comparisons.
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch("ANY/ALL takes 3 args".into()));
                }
                let op = match value_to_string(&args[2]).as_str() {
                    "=" => BinaryOp::Equal,
                    "<>" | "!=" => BinaryOp::NotEqual,
                    "<" => BinaryOp::Less,
                    "<=" => BinaryOp::LessEqual,
                    ">" => BinaryOp::Greater,
                    ">=" => BinaryOp::GreaterEqual,
                    other => {
                        return Err(SQLError::Unsupported(format!(
                            "operator `{other}` with ANY/ALL"
                        )));
                    }
                };
                let Value::Array(array) = &args[1] else {
                    if matches!(args[1], Value::Null) {
                        return Ok(Value::Null);
                    }
                    return Err(SQLError::TypeMismatch("ANY/ALL requires an array".into()));
                };
                let is_any = name == "__any_op";
                let mut saw_null = false;
                let mut items = Vec::new();
                flatten_array_elements(array.elements(), &mut items);
                for item in items {
                    match eval_comparison_op(op, &args[0], item)? {
                        Value::Bool(true) if is_any => return Ok(Value::Bool(true)),
                        Value::Bool(false) if !is_any => return Ok(Value::Bool(false)),
                        Value::Null => saw_null = true,
                        _ => {}
                    }
                }
                if saw_null {
                    return Ok(Value::Null);
                }
                Ok(Value::Bool(!is_any))
            }
            "__is_distinct" => {
                // IS DISTINCT FROM: null-safe inequality (never NULL).
                if args.len() != 2 {
                    return Err(SQLError::TypeMismatch(
                        "IS DISTINCT FROM takes 2 args".into(),
                    ));
                }
                let distinct = match (&args[0], &args[1]) {
                    (Value::Null, Value::Null) => false,
                    (Value::Null, _) | (_, Value::Null) => true,
                    (a, b) => !values_equal(a, b),
                };
                Ok(Value::Bool(distinct))
            }
            "__between_symmetric" => {
                // BETWEEN SYMMETRIC: PostgreSQL rewrites to
                // `(a >= x AND a <= y) OR (a >= y AND a <= x)` and the
                // three-valued OR of the two window tests.
                if args.len() != 3 {
                    return Err(SQLError::TypeMismatch(
                        "BETWEEN SYMMETRIC takes 3 args".into(),
                    ));
                }
                let forward = eval_between(&args[0], &args[1], &args[2])?;
                let backward = eval_between(&args[0], &args[2], &args[1])?;
                Ok(match (&forward, &backward) {
                    (Value::Bool(true), _) | (_, Value::Bool(true)) => Value::Bool(true),
                    (Value::Null, _) | (_, Value::Null) => Value::Null,
                    _ => Value::Bool(false),
                })
            }
            _ => unreachable!("function family membership was checked before dispatch"),
        }
    })())
}

fn array_subscripts(array: &ArrayValue, indices: &[Value]) -> Result<Value> {
    if indices.len() != array.dimensions().len() || indices.is_empty() {
        return Ok(Value::Null);
    }
    let mut elements = array.elements();
    for (dimension, index) in indices.iter().enumerate() {
        let lower = i64::from(
            array
                .lower_bound(dimension)
                .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?,
        );
        let index = to_i64(index)?;
        let Some(offset) = index
            .checked_sub(lower)
            .and_then(|offset| usize::try_from(offset).ok())
        else {
            return Ok(Value::Null);
        };
        let Some(value) = elements.get(offset) else {
            return Ok(Value::Null);
        };
        if dimension + 1 == indices.len() {
            return Ok(value.clone());
        }
        let Value::List(nested) = value else {
            return Err(SQLError::TypeMismatch(
                "invalid multidimensional array".into(),
            ));
        };
        elements = nested;
    }
    Ok(Value::Null)
}

fn array_slices(array: &ArrayValue, bounds: &[Value]) -> Result<Value> {
    let supplied_dimensions = bounds.len() / 2;
    if supplied_dimensions > array.dimensions().len() || array.dimensions().is_empty() {
        return ArrayValue::try_new(Vec::new())
            .map(Value::Array)
            .ok_or_else(|| SQLError::TypeMismatch("invalid empty array".into()));
    }
    let mut ranges = Vec::with_capacity(array.dimensions().len());
    for dimension in 0..array.dimensions().len() {
        let array_lower = i64::from(
            array
                .lower_bound(dimension)
                .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?,
        );
        let array_upper = array
            .upper_bound(dimension)
            .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?;
        let (requested_lower, requested_upper) = if dimension < supplied_dimensions {
            let lower = match &bounds[dimension * 2] {
                Value::Null => array_lower,
                value => to_i64(value)?,
            };
            let upper = match &bounds[dimension * 2 + 1] {
                Value::Null => array_upper,
                value => to_i64(value)?,
            };
            (lower, upper)
        } else {
            (array_lower, array_upper)
        };
        let lower = requested_lower.max(array_lower);
        let upper = requested_upper.min(array_upper);
        if upper < lower {
            return ArrayValue::try_new(Vec::new())
                .map(Value::Array)
                .ok_or_else(|| SQLError::TypeMismatch("invalid empty array".into()));
        }
        ranges.push((lower, upper, array_lower));
    }
    let elements = slice_array_elements(array.elements(), &ranges, 0)?;
    rebuild_array_with_bounds(elements, vec![1; array.dimensions().len()])
}

fn slice_array_elements(
    elements: &[Value],
    ranges: &[(i64, i64, i64)],
    dimension: usize,
) -> Result<Vec<Value>> {
    let (lower, upper, array_lower) = ranges[dimension];
    let start = usize::try_from(lower - array_lower).map_err(|_| out_of_range("array slice"))?;
    let end = usize::try_from(upper - array_lower + 1).map_err(|_| out_of_range("array slice"))?;
    let selected = elements
        .get(start..end)
        .ok_or_else(|| SQLError::TypeMismatch("invalid array dimensions".into()))?;
    if dimension + 1 == ranges.len() {
        return Ok(selected.to_vec());
    }
    selected
        .iter()
        .map(|value| {
            let Value::List(nested) = value else {
                return Err(SQLError::TypeMismatch(
                    "invalid multidimensional array".into(),
                ));
            };
            slice_array_elements(nested, ranges, dimension + 1).map(Value::List)
        })
        .collect()
}

fn rebuild_array_value(array: &ArrayValue, elements: Vec<Value>) -> Result<Value> {
    rebuild_array_with_bounds(elements, array.lower_bounds().to_vec())
}

fn rebuild_array_with_bounds(elements: Vec<Value>, lower_bounds: Vec<i32>) -> Result<Value> {
    let rebuilt = if elements.is_empty() {
        ArrayValue::try_new(elements)
    } else {
        ArrayValue::with_lower_bounds(elements, lower_bounds)
    };
    rebuilt
        .map(Value::Array)
        .ok_or_else(|| SQLError::TypeMismatch("array dimensions do not match".into()))
}

fn replace_array_elements(elements: &[Value], from: &Value, to: &Value) -> Vec<Value> {
    elements
        .iter()
        .map(|value| match value {
            Value::List(nested) => Value::List(replace_array_elements(nested, from, to)),
            value if value == from => to.clone(),
            value => value.clone(),
        })
        .collect()
}

fn flatten_array_elements<'a>(elements: &'a [Value], output: &mut Vec<&'a Value>) {
    for element in elements {
        if let Value::List(nested) = element {
            flatten_array_elements(nested, output);
        } else {
            output.push(element);
        }
    }
}

fn containment_operator(name: &str, args: &[Value]) -> Result<Value> {
    if args.len() != 2 {
        return Err(SQLError::TypeMismatch(format!(
            "containment operator takes 2 args, got {}",
            args.len()
        )));
    }
    if matches!(args[0], Value::Null) || matches!(args[1], Value::Null) {
        return Ok(Value::Null);
    }
    let (left, right) = if name == "contains_op" {
        (&args[0], &args[1])
    } else {
        (&args[1], &args[0])
    };
    match (left, right) {
        (Value::Array(left), Value::Array(right)) => {
            let mut left_elements = Vec::new();
            let mut right_elements = Vec::new();
            flatten_array_elements(left.elements(), &mut left_elements);
            flatten_array_elements(right.elements(), &mut right_elements);
            Ok(Value::Bool(right_elements.iter().all(|right| {
                !matches!(right, Value::Null)
                    && left_elements.iter().any(|left| values_equal(left, right))
            })))
        }
        (Value::JsonB(_), Value::JsonB(_) | Value::Str(_)) | (Value::Str(_), Value::JsonB(_)) => {
            if name == "contains_op" {
                json_contains(args)
            } else {
                json_contained_by(args)
            }
        }
        _ => Err(SQLError::TypeMismatch(format!(
            "containment operator requires two arrays or two jsonb values, got {:?} and {:?}",
            args[0], args[1]
        ))),
    }
}

fn filled_array_elements(value: &Value, dimensions: &[usize]) -> Result<Vec<Value>> {
    let length = dimensions[0];
    let mut values = Vec::new();
    values
        .try_reserve_exact(length)
        .map_err(|_| allocation_error("array_fill"))?;
    if dimensions.len() == 1 {
        values.resize(length, value.clone());
        return Ok(values);
    }
    let nested = filled_array_elements(value, &dimensions[1..])?;
    values.resize(length, Value::List(nested));
    Ok(values)
}

fn invalid_regex_parameter(name: &str, value: i64) -> SQLError {
    SQLError::Routine {
        sqlstate: "22023".into(),
        message: format!("invalid value for parameter \"{name}\": {value}"),
    }
}

fn positive_regex_parameter(value: Option<&Value>, default: usize, name: &str) -> Result<usize> {
    let value = value.map(to_i64).transpose()?.unwrap_or(default as i64);
    if value <= 0 {
        return Err(invalid_regex_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

fn nonnegative_regex_parameter(value: Option<&Value>, default: usize, name: &str) -> Result<usize> {
    let value = value.map(to_i64).transpose()?.unwrap_or(default as i64);
    if value < 0 {
        return Err(invalid_regex_parameter(name, value));
    }
    Ok(usize::try_from(value).unwrap_or(usize::MAX))
}

fn regex_tail(string: &str, start: usize) -> Option<(&str, usize)> {
    let base_chars = start.checked_sub(1)?;
    if base_chars == 0 {
        return Some((string, 0));
    }
    let byte_index = string
        .char_indices()
        .nth(base_chars)
        .map(|(index, _)| index)?;
    Some((&string[byte_index..], base_chars))
}
