//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` foreign catalog row descriptors, shared by relation and type projection.

use super::{
    schema_production::CatalogColumnType::{self, Array, Inline},
    VirtualRelation,
};
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

pub(super) fn visit_columns(
    relation: VirtualRelation,
    visit: &mut dyn FnMut(&'static str, CatalogColumnType),
) {
    use ColumnType::{AclItem, Name, Oid, Text};
    macro_rules! columns {
        ($($name:literal => $ty:expr),* $(,)?) => {{ $(visit($name, $ty);)* }};
    }
    match relation {
        VirtualRelation::PgForeignDataWrapper => columns![
            "oid" => Inline(Oid), "fdwname" => Inline(Name), "fdwowner" => Inline(Oid),
            "fdwhandler" => Inline(Oid), "fdwvalidator" => Inline(Oid),
            "fdwacl" => Array(AclItem), "fdwoptions" => Array(Text),
        ],
        VirtualRelation::PgForeignServer => columns![
            "oid" => Inline(Oid), "srvname" => Inline(Name), "srvowner" => Inline(Oid),
            "srvfdw" => Inline(Oid), "srvtype" => Inline(Text), "srvversion" => Inline(Text),
            "srvacl" => Array(AclItem), "srvoptions" => Array(Text),
        ],
        VirtualRelation::PgForeignTable => columns![
            "ftrelid" => Inline(Oid), "ftserver" => Inline(Oid), "ftoptions" => Array(Text),
        ],
        _ => unreachable!("foreign catalog descriptor"),
    }
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
