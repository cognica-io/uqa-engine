//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

struct TypedOutput(&'static str);

impl EngineHook for TypedOutput {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }

    fn resolve_regtype_output_value(&self, _: &ColumnType, _: i64) -> Result<Option<String>> {
        Err(SQLError::Routine {
            sqlstate: self.0.into(),
            message: "catalog read failed".into(),
        })
    }
}

struct LegacyOutput;

impl EngineHook for LegacyOutput {
    fn nextval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64> {
        unreachable!()
    }

    fn resolve_regtype_output(
        &self,
        _: &ColumnType,
        oid: i64,
    ) -> std::result::Result<Option<String>, String> {
        if oid == 1 {
            Ok(Some("legacy_relation".into()))
        } else {
            Err("legacy failure".into())
        }
    }
}

#[test]
fn regobject_output_preserves_typed_catalog_errors_in_scalars_and_arrays() {
    for state in ["40001", "57014", "53200"] {
        let hook = TypedOutput(state);
        let scalar = (Value::Int(1), ColumnType::Regclass);
        let array = (
            Value::Array(ArrayValue::with_lower_bounds(vec![Value::Int(1)], vec![3]).unwrap()),
            ColumnType::Array(Box::new(ColumnType::Regnamespace)),
        );
        for (value, ty) in [scalar, array] {
            let error = format_regtype_value(&value, &ty, Some(&hook)).unwrap_err();
            assert_eq!(error.sqlstate(), Some(state));
        }
    }
}

#[test]
fn regobject_output_preserves_legacy_hooks_and_skips_null_and_zero_values() {
    assert_eq!(
        format_regtype_value(&Value::Int(1), &ColumnType::Regclass, Some(&LegacyOutput)).unwrap(),
        Some("legacy_relation".into())
    );
    let error = format_regtype_value(&Value::Int(2), &ColumnType::Regclass, Some(&LegacyOutput))
        .unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "legacy failure"));
    for (value, expected) in [(Value::Null, None), (Value::Int(0), Some("-".into()))] {
        assert_eq!(
            format_regtype_value(
                &value,
                &ColumnType::Regnamespace,
                Some(&TypedOutput("40001"))
            )
            .unwrap(),
            expected
        );
    }
}
