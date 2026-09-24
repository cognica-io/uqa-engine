//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Result, Value};

pub(in crate::expr) fn jsonpath_match(args: &[Value]) -> Result<Value> {
    super::ordinary("jsonpath_match", args)
}

pub(in crate::expr) fn jsonpath_candidate(args: &[Value]) -> bool {
    matches!(args.get(1), Some(Value::Str(path)) if path.trim_start().starts_with('$'))
        && matches!(
            args.first(),
            Some(Value::Json(_) | Value::JsonB(_) | Value::Map(_) | Value::List(_))
        )
}
