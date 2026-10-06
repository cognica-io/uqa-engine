//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::ast::{AutoIncrement, AutoIncrementOwner, ColumnType, PartitionBound, TableHierarchy};
use crate::semantics::partition::PartitionCatalog;
use crate::SQLError;

struct Partitions;
impl PartitionCatalog for Partitions {
    fn try_table_hierarchy(&self, table: &str) -> Result<TableHierarchy, String> {
        Ok(if table == "other.child" {
            TableHierarchy {
                parents: vec!["public.parent".into()],
                partition_bound: Some(PartitionBound::Default),
                ..TableHierarchy::default()
            }
        } else {
            TableHierarchy::default()
        })
    }
    fn direct_hierarchy_children(&self, _: &str) -> Result<Vec<String>, SQLError> {
        unreachable!()
    }
    fn try_resolve_table_name(&self, _: &str) -> Result<Option<String>, String> {
        unreachable!()
    }
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        unreachable!()
    }
    fn try_table_object_id(&self, _: &str) -> Result<Option<[u8; 16]>, String> {
        unreachable!()
    }
}

fn identity_column() -> ColumnDef {
    let mut column = ColumnDef::nullable("id", ColumnType::Integer);
    column.auto_increment = Some(AutoIncrement {
        sequence: Some("public.parent_id_seq".into()),
        owner: Some(AutoIncrementOwner {
            table: "public.parent".into(),
            column: "id".into(),
        }),
        ..AutoIncrement::identity_by_default()
    });
    column
}

#[test]
fn identity_defaults_require_the_column_or_partition_ancestor_to_own_the_sequence() {
    let column = identity_column();
    for table in ["public.parent", "other.child"] {
        assert_eq!(
            identity_column_sequence(&Partitions, table, &column)
                .unwrap()
                .as_deref(),
            Some("public.parent_id_seq")
        );
    }
    for table in ["other.parent", "public.unrelated"] {
        let error = identity_column_sequence(&Partitions, table, &column).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
        assert_eq!(error.to_string(), "no owned sequence found");
    }
}

#[test]
fn explicitly_named_sequences_link_in_their_own_schema() {
    let mut column = identity_column();
    super::super::implicit::apply_implicit_sequence_metadata(
        "public.parent",
        &mut column,
        "other.explicit_seq".into(),
    )
    .unwrap();
    let owner = column
        .auto_increment
        .as_ref()
        .unwrap()
        .owner
        .as_ref()
        .unwrap();
    assert_eq!(owner.table, "other.parent");
    assert_eq!(owner.column, "id");
    assert!(identity_column_sequence(&Partitions, "public.parent", &column).is_err());
    assert_eq!(
        identity_column_sequence(&Partitions, "other.parent", &column)
            .unwrap()
            .as_deref(),
        Some("other.explicit_seq")
    );
}
