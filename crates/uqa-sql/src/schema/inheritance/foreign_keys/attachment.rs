//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrow the definition fields `PostgreSQL` compares when attaching a foreign key.

use crate::{
    ast::{ColumnDef, ForeignKey, ForeignKeyAction, ForeignKeyMatch, ForeignKeyRef},
    catalog::{
        work::{compare_text, ReferenceWork},
        CatalogWork,
    },
    SQLError,
};

#[derive(Clone, Copy)]
pub(crate) struct Attachment<'a> {
    reference: Reference<'a>,
    options: Options,
}

#[derive(Clone, Copy)]
struct Reference<'a> {
    local: &'a [String],
    target: &'a str,
    columns: &'a [String],
    period: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Options {
    enforced: bool,
    deferrable: bool,
    initially_deferred: bool,
    on_update: ForeignKeyAction,
    on_delete: ForeignKeyAction,
    match_type: ForeignKeyMatch,
}

impl<'a> Attachment<'a> {
    fn column(column: &'a ColumnDef, reference: &'a ForeignKeyRef) -> Self {
        Self {
            reference: Reference {
                local: std::slice::from_ref(&column.name),
                target: &reference.table,
                columns: reference.column.as_slice(),
                period: reference.period,
            },
            options: Options {
                enforced: reference.enforced,
                deferrable: reference.deferrable,
                initially_deferred: reference.initially_deferred,
                on_update: reference.on_update,
                on_delete: reference.on_delete,
                match_type: reference.match_type,
            },
        }
    }

    pub(super) fn table(key: &'a ForeignKey) -> Self {
        Self {
            reference: Reference {
                local: &key.local_columns,
                target: &key.ref_table,
                columns: &key.ref_columns,
                period: key.period,
            },
            options: Options {
                enforced: key.enforced,
                deferrable: key.deferrable,
                initially_deferred: key.initially_deferred,
                on_update: key.on_update,
                on_delete: key.on_delete,
                match_type: key.match_type,
            },
        }
    }

    pub(crate) async fn same_reference_with_work(
        self,
        other: Self,
        input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
    ) -> Result<bool, SQLError> {
        let left = self.reference;
        let right = other.reference;
        Ok(same_columns(left.local, right.local, input).await?
            && compare_text(input.work, left.target, right.target, input.control)
                .await?
                .is_eq()
            && same_columns(left.columns, right.columns, input).await?
            && left.period == right.period)
    }

    pub(crate) fn same_options(self, other: Self) -> bool {
        self.options == other.options
    }
}

pub(crate) async fn same_columns(
    left: &[String],
    right: &[String],
    input: &mut ReferenceWork<'_, '_, impl CatalogWork + Send>,
) -> Result<bool, SQLError> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left.iter().zip(right) {
        input.checkpoint().await?;
        if compare_text(input.work, left, right, input.control)
            .await?
            .is_ne()
        {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Catalog identities, names, validation flags, selected indexes and SET column subsets are not attachment-equivalence fields. Their independent validity remains with their native owners.
pub(crate) fn declarations<'a>(
    columns: &'a [ColumnDef],
    foreign_keys: &'a [ForeignKey],
) -> impl Iterator<Item = ([u8; 16], Attachment<'a>)> + 'a {
    declaration_slots(columns, foreign_keys).flatten()
}

pub(crate) fn declaration_slots<'a>(
    columns: &'a [ColumnDef],
    foreign_keys: &'a [ForeignKey],
) -> impl Iterator<Item = Option<([u8; 16], Attachment<'a>)>> + 'a {
    columns
        .iter()
        .map(|column| {
            let reference = column.references.as_ref()?;
            Some((reference.object_id?, Attachment::column(column, reference)))
        })
        .chain(
            foreign_keys
                .iter()
                .map(|key| Some((key.object_id?, Attachment::table(key)))),
        )
}
