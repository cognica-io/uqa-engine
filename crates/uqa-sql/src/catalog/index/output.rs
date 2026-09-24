//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Unique-key diagnostics use the default B-tree operator class input type, which can differ from the declared column type.

use crate::{expr::EngineHook, result::format_postgres_text, ColumnType, SQLError};
use uqa_core::Value;

pub fn format_key_value(
    value: &Value,
    ty: &ColumnType,
    resolver: Option<&dyn EngineHook>,
) -> Result<String, SQLError> {
    match ty {
        ColumnType::Domain { base, .. } => format_key_value(value, base, resolver),
        ColumnType::Regproc
        | ColumnType::Regprocedure
        | ColumnType::Regclass
        | ColumnType::Regnamespace
        | ColumnType::Regrole
        | ColumnType::Regtype => format_postgres_text(value, &ColumnType::Oid, resolver),
        ColumnType::Int2Vector => format_postgres_text(
            value,
            &ColumnType::Array(Box::new(ColumnType::SmallInteger)),
            resolver,
        ),
        _ => format_postgres_text(value, ty, resolver),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uqa_core::{LegacyVectorKind, LegacyVectorValue};

    #[test]
    fn default_btree_input_types_preserve_alias_and_vector_output() {
        assert_eq!(
            format_key_value(&Value::Int(42), &ColumnType::Regclass, None).unwrap(),
            "42"
        );
        for (kind, ty, expected) in [
            (
                LegacyVectorKind::SmallInteger,
                ColumnType::Int2Vector,
                "[0:1]={1,2}",
            ),
            (LegacyVectorKind::Oid, ColumnType::OidVector, "1 2"),
        ] {
            let value = Value::LegacyVector(
                LegacyVectorValue::try_new(kind, vec![Value::Int(1), Value::Int(2)]).unwrap(),
            );
            assert_eq!(format_key_value(&value, &ty, None).unwrap(), expected);
        }
    }
}
