use super::{JSON, JS_MAX_SAFE_INTEGER};

pub(super) fn req_str(args: &JSON, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(JSON::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("missing string argument `{key}`"))
}

pub(super) fn opt_str(args: &JSON, key: &str) -> Result<Option<String>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(str::to_string)
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a string")),
    }
}

pub(super) fn req_str_list(args: &JSON, key: &str) -> Result<Vec<String>, String> {
    args.get(key)
        .and_then(JSON::as_array)
        .map(|items| {
            items
                .iter()
                .map(|item| {
                    item.as_str()
                        .map(str::to_string)
                        .ok_or_else(|| format!("`{key}` must contain strings"))
                })
                .collect()
        })
        .ok_or_else(|| format!("missing list argument `{key}`"))?
}

pub(super) fn req_u64(args: &JSON, key: &str) -> Result<u64, String> {
    let value = args
        .get(key)
        .and_then(JSON::as_u64)
        .ok_or_else(|| format!("missing non-negative integer argument `{key}`"))?;
    if value > JS_MAX_SAFE_INTEGER {
        return Err(format!(
            "`{key}` exceeds JavaScript's maximum safe integer ({JS_MAX_SAFE_INTEGER})"
        ));
    }
    Ok(value)
}

pub(super) fn req_u32(args: &JSON, key: &str) -> Result<u32, String> {
    u32::try_from(req_u64(args, key)?)
        .map_err(|_| format!("`{key}` exceeds the maximum 32-bit unsigned integer"))
}

pub(super) fn req_usize(args: &JSON, key: &str) -> Result<usize, String> {
    usize::try_from(req_u64(args, key)?)
        .map_err(|_| format!("`{key}` exceeds this WebAssembly build's addressable range"))
}

pub(super) fn opt_u64(args: &JSON, key: &str) -> Result<Option<u64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_u64()
                .ok_or_else(|| format!("`{key}` must be a non-negative integer"))?;
            if value > JS_MAX_SAFE_INTEGER {
                return Err(format!(
                    "`{key}` exceeds JavaScript's maximum safe integer ({JS_MAX_SAFE_INTEGER})"
                ));
            }
            Ok(Some(value))
        }
    }
}

pub(super) fn opt_usize(args: &JSON, key: &str) -> Result<Option<usize>, String> {
    opt_u64(args, key)?
        .map(|value| {
            usize::try_from(value)
                .map_err(|_| format!("`{key}` exceeds this WebAssembly build's addressable range"))
        })
        .transpose()
}

pub(super) fn opt_i64(args: &JSON, key: &str) -> Result<Option<i64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => {
            let value = value
                .as_i64()
                .ok_or_else(|| format!("`{key}` must be an integer"))?;
            if value.unsigned_abs() > JS_MAX_SAFE_INTEGER {
                return Err(format!(
                    "`{key}` exceeds JavaScript's safe integer range: {value}"
                ));
            }
            Ok(Some(value))
        }
    }
}

pub(super) fn req_f64(args: &JSON, key: &str) -> Result<f64, String> {
    args.get(key)
        .and_then(JSON::as_f64)
        .ok_or_else(|| format!("missing number argument `{key}`"))
}

pub(super) fn opt_f64(args: &JSON, key: &str) -> Result<Option<f64>, String> {
    match args.get(key) {
        None | Some(JSON::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .map(Some)
            .ok_or_else(|| format!("`{key}` must be a number")),
    }
}

pub(super) fn req_f32_list(args: &JSON, key: &str) -> Result<Vec<f32>, String> {
    f32_list(
        args.get(key)
            .ok_or_else(|| format!("missing vector argument `{key}`"))?,
        key,
    )
}

pub(super) fn req_f32_rows(args: &JSON, key: &str) -> Result<Vec<Vec<f32>>, String> {
    args.get(key)
        .and_then(JSON::as_array)
        .ok_or_else(|| format!("missing tensor argument `{key}`"))?
        .iter()
        .map(|row| f32_list(row, key))
        .collect()
}

pub(super) fn f32_list(value: &JSON, key: &str) -> Result<Vec<f32>, String> {
    value
        .as_array()
        .ok_or_else(|| format!("`{key}` must be an array of numbers"))?
        .iter()
        .map(|item| {
            let number = item
                .as_f64()
                .ok_or_else(|| format!("`{key}` must contain numbers"))?;
            f32_from_f64(number, key)
        })
        .collect()
}

pub(super) fn f32_from_f64(value: f64, context: &str) -> Result<f32, String> {
    if !value.is_finite() {
        return Err(format!("`{context}` must be finite, got {value}"));
    }
    if value < f64::from(f32::MIN) || value > f64::from(f32::MAX) {
        return Err(format!("`{context}` is outside the f32 range: {value}"));
    }
    Ok(value as f32)
}

pub(super) fn binary_label(value: u64) -> Result<u8, String> {
    match value {
        0 | 1 => u8::try_from(value).map_err(|_| "label exceeds the u8 bridge".to_string()),
        _ => Err("label must be 0 or 1".to_string()),
    }
}

pub(super) fn req_labels(args: &JSON) -> Result<Vec<u8>, String> {
    args.get("labels")
        .and_then(JSON::as_array)
        .ok_or("missing `labels` array")?
        .iter()
        .map(|label| {
            binary_label(
                label
                    .as_u64()
                    .ok_or_else(|| "labels must contain only 0 or 1".to_string())?,
            )
        })
        .collect()
}
