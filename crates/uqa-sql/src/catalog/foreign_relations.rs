//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` foreign catalog row descriptors, shared by relation and type projection.

use super::VirtualRelation;
use crate::ColumnType;

pub struct ForeignCatalogDescriptor {
    pub relation: VirtualRelation,
    pub row_type: u32,
    pub array_type: u32,
    pub required_columns: usize,
}

pub const FOREIGN_CATALOGS: &[ForeignCatalogDescriptor] = &[
    ForeignCatalogDescriptor {
        relation: VirtualRelation::PgForeignServer,
        row_type: 10_078,
        array_type: 10_077,
        required_columns: 4,
    },
    ForeignCatalogDescriptor {
        relation: VirtualRelation::PgForeignDataWrapper,
        row_type: 10_076,
        array_type: 10_075,
        required_columns: 5,
    },
    ForeignCatalogDescriptor {
        relation: VirtualRelation::PgForeignTable,
        row_type: 10_082,
        array_type: 10_081,
        required_columns: 2,
    },
];

pub(super) fn schema(relation: VirtualRelation) -> Vec<(String, ColumnType)> {
    use ColumnType::{AclItem, Name, Oid, Text};
    let array = |ty| ColumnType::Array(Box::new(ty));
    let columns = match relation {
        VirtualRelation::PgForeignDataWrapper => vec![
            ("oid", Oid),
            ("fdwname", Name),
            ("fdwowner", Oid),
            ("fdwhandler", Oid),
            ("fdwvalidator", Oid),
            ("fdwacl", array(AclItem)),
            ("fdwoptions", array(Text)),
        ],
        VirtualRelation::PgForeignServer => vec![
            ("oid", Oid),
            ("srvname", Name),
            ("srvowner", Oid),
            ("srvfdw", Oid),
            ("srvtype", Text),
            ("srvversion", Text),
            ("srvacl", array(AclItem)),
            ("srvoptions", array(Text)),
        ],
        VirtualRelation::PgForeignTable => vec![
            ("ftrelid", Oid),
            ("ftserver", Oid),
            ("ftoptions", array(Text)),
        ],
        _ => unreachable!("foreign catalog descriptor"),
    };
    columns
        .into_iter()
        .map(|(name, ty)| (name.into(), ty))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_catalog_descriptors_match_postgresql() {
        let reference: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/parity/pg18/foreign_catalog_oracle.expected.json"
        )))
        .unwrap();
        let expected = reference["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == "column_types")
            .unwrap()["results"][0]["rows"]
            .clone();
        let mut rows = Vec::new();
        for descriptor in FOREIGN_CATALOGS {
            assert!(descriptor.relation.accepts_row_lock());
            for (index, (name, ty)) in descriptor.relation.schema().into_iter().enumerate() {
                rows.push(vec![
                    descriptor.relation.oid().to_string(),
                    (index + 1).to_string(),
                    name,
                    crate::catalog::type_metadata::pg_type_oid(&ty).to_string(),
                    if index < descriptor.required_columns {
                        "t"
                    } else {
                        "f"
                    }
                    .to_owned(),
                    "f".into(),
                ]);
            }
        }
        assert_eq!(serde_json::json!(rows), expected);
    }
}
