//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::expr::{builtin_scalar_function_strictness, eval_builtin_function_call, EngineHook};
use std::cell::Cell;

#[derive(Default)]
struct SettingsHook {
    reads: Cell<usize>,
}

impl EngineHook for SettingsHook {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!("setting lookup does not allocate sequences")
    }

    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!("setting lookup does not read sequences")
    }

    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!("setting lookup does not assign sequences")
    }

    fn runtime_parameter(&self, name: &str) -> Result<Option<String>> {
        self.reads.set(self.reads.get() + 1);
        match name {
            "application_name" => Ok(Some("client".into())),
            "protected" => Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "permission denied".into(),
            }),
            _ => Ok(None),
        }
    }
}

#[test]
fn setting_lookup_distinguishes_missing_values_from_host_errors() {
    let host = SettingsHook::default();
    let ctx = EvalContext::new(None, &[]).with_engine(&host);
    let call = |args: Vec<Value>| {
        eval_builtin_function_call(
            "pg_catalog.current_setting",
            args.into_iter().map(|value| (None, value)).collect(),
            &ctx,
        )
    };
    assert_eq!(
        call(vec![Value::Str("application_name".into())]).unwrap(),
        Value::Str("client".into())
    );
    assert_eq!(
        call(vec![Value::Str("unknown".into()), Value::Bool(true)]).unwrap(),
        Value::Null
    );
    for args in [
        vec![Value::Str("unknown".into())],
        vec![Value::Str("unknown".into()), Value::Bool(false)],
    ] {
        assert_eq!(call(args).unwrap_err().sqlstate(), Some("42704"));
    }
    assert_eq!(
        call(vec![Value::Str("protected".into()), Value::Bool(true)])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
}

#[test]
fn strict_setting_calls_do_not_read_state_for_null_arguments() {
    let host = SettingsHook::default();
    let ctx = EvalContext::new(None, &[]).with_engine(&host);
    for args in [
        vec![Value::Null],
        vec![Value::Null, Value::Bool(false)],
        vec![Value::Str("unknown".into()), Value::Null],
    ] {
        assert_eq!(
            builtin_scalar_function_strictness("pg_catalog.current_setting", args.len()),
            Some(true)
        );
        assert_eq!(current_setting(&args, &ctx).unwrap(), Value::Null);
    }
    assert_eq!(host.reads.get(), 0);
}

#[test]
fn setting_lookup_requires_a_session_and_valid_arguments() {
    let ctx = EvalContext::new(None, &[]);
    assert!(matches!(
        current_setting(&[Value::Str("application_name".into())], &ctx),
        Err(SQLError::Unsupported(_))
    ));
    for args in [vec![], vec![Value::Null; 3]] {
        assert!(matches!(
            current_setting(&args, &ctx),
            Err(SQLError::BadArity { .. })
        ));
    }
    assert!(matches!(
        current_setting(&[Value::Int(1)], &ctx),
        Err(SQLError::TypeMismatch(_))
    ));
}
