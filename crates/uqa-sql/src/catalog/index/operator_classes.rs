//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Default B-tree capability of resolved SQL types, independently of the carrier's total order.

use crate::{ColumnType, SQLError};

mod btree;
pub use btree::{
    builtin_btree_operator_class, builtin_btree_operator_classes, default_btree_operator_class,
    BoundBTreeOperatorClass, BuiltinBTreeComparison, BuiltinBTreeOperatorClass,
};

/// Resolve default B-tree capability using the shared class identities. Domains use their base class; generic arrays and records do not require a scalar class for every element or field. UQA vectors and tensors retain their native ordering capability.
pub fn has_default_btree_operator_class(ty: &ColumnType) -> bool {
    match ty {
        ColumnType::Domain { base, .. } => has_default_btree_operator_class(base),
        ColumnType::Vector(_) | ColumnType::Tensor(_) => true,
        _ => default_btree_operator_class(ty).is_some(),
    }
}

/// Validate default B-tree selection while retaining the declared type's visible name in diagnostics, including a domain whose base has no class.
pub fn require_default_btree_operator_class(ty: &ColumnType) -> Result<(), SQLError> {
    if has_default_btree_operator_class(ty) {
        Ok(())
    } else {
        Err(missing_operator_class(&ty.display_name(), "btree"))
    }
}

pub(crate) fn missing_operator_class(type_name: &str, access_method: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42704".into(),
        message: format!(
            "data type {type_name} has no default operator class for access method \"{access_method}\""
        ),
        detail: None,
        hint: Some(
            "You must specify an operator class for the index or define a default operator class for the data type."
                .into(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domain(base: ColumnType) -> ColumnType {
        ColumnType::Domain {
            schema: "opclass299".into(),
            name: "json_domain".into(),
            oid: 80_001,
            array_oid: Some(80_002),
            collation_oid: None,
            base: Box::new(base),
        }
    }

    #[test]
    fn default_btree_classes_match_independent_pg18_key_declarations() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../tests/parity/pg18/default_btree_operator_class_oracle.expected.json"
        ))
        .unwrap();
        let _scope = crate::ast::TypeDisplayScope::enter(&["opclass299".into()]);
        for (id, ty) in [
            ("json_unique", ColumnType::Json),
            ("domain_unique", domain(ColumnType::Json)),
            ("xid_table", ColumnType::Xid),
            ("refcursor_table", ColumnType::RefCursor),
            ("aclitem_table", ColumnType::AclItem),
            ("jsonb_index", ColumnType::JsonB),
            ("array_index", ColumnType::Array(Box::new(ColumnType::Json))),
            ("composite_index", ColumnType::Record),
            ("int2vector_table", ColumnType::Int2Vector),
            ("oidvector_table", ColumnType::OidVector),
            ("alias_table", ColumnType::Regclass),
            ("character_table", ColumnType::InternalChar),
        ] {
            let expected = &reference["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["id"] == id)
                .unwrap()["error"];
            let actual = require_default_btree_operator_class(&ty)
                .err()
                .map(|error| {
                    serde_json::json!({
                        "sqlstate": error.sqlstate(),
                        "message": error.to_string(),
                        "detail": error.detail(),
                        "hint": error.hint(),
                    })
                });
            assert_eq!(serde_json::to_value(actual).unwrap(), *expected, "{id}");
        }
    }

    #[test]
    fn missing_base_class_keeps_the_visible_domain_identity() {
        let ty = domain(domain(ColumnType::Json));
        let _scope = crate::ast::TypeDisplayScope::enter(&["public".into()]);
        assert_eq!(
            require_default_btree_operator_class(&ty).unwrap_err().to_string(),
            "data type opclass299.json_domain has no default operator class for access method \"btree\""
        );
        for ty in [ColumnType::Vector(3), ColumnType::Tensor(3)] {
            require_default_btree_operator_class(&ty).unwrap();
        }
    }
}
