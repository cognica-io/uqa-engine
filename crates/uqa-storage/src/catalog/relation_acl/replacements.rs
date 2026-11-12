//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent replacements preserve original physical order within each relation.

use crate::{
    catalog::{relation_acl::RelationAclTuple, table_input::CatalogTableAcl},
    read_control::StorageReadControl,
    RelationIdentity, RelationSecurityRow, StorageBackendError, StorageBackendResult,
};
use uqa_core::{
    catalog_acl::TablePrivileges,
    catalog_role::BoundAclEntry,
    image_io::AsyncImageInput,
    memory::{Budgeted, BudgetedVec, MemoryReservation, OwnedMap, ProductionControl},
};

type Entry = BoundAclEntry<TablePrivileges>;
type Columns = OwnedMap<String, Vec<Entry>>;

struct Replacement {
    column: Option<String>,
    tuple: RelationAclTuple,
}

pub(crate) struct Records(OwnedMap<RelationIdentity, BudgetedVec<Budgeted<Replacement>>>);

impl Records {
    pub(crate) fn new() -> Self {
        Self(OwnedMap::new())
    }

    pub(crate) fn insert(
        &mut self,
        row: Budgeted<CatalogTableAcl>,
        control: &StorageReadControl,
    ) -> StorageBackendResult<()> {
        let (
            CatalogTableAcl {
                relation,
                column,
                tuple,
            },
            mut memory,
        ) = row.into_parts();
        let key_bytes = relation.schema.capacity() + relation.name.capacity();
        let key = Budgeted::new(relation, memory.split(key_bytes));
        let replacement = Budgeted::new(Replacement { column, tuple }, memory);
        if let Some(records) = self.0.get_mut(&*key) {
            records.push(replacement)?;
        } else {
            let mut records = BudgetedVec::new(control.memory());
            records.push(replacement)?;
            self.0.insert_admitted_key(key, records)?;
        }
        Ok(())
    }

    pub(crate) async fn apply(
        &self,
        relation: &RelationIdentity,
        security: &mut RelationSecurityRow,
        memory: &mut MemoryReservation,
        progress: &mut AsyncImageInput<'_>,
    ) -> StorageBackendResult<()> {
        progress.checkpoint().await?;
        let Some(records) = self.0.get(relation) else {
            return Ok(());
        };
        if !matches!(security, RelationSecurityRow::Bound(_)) {
            return Err(StorageBackendError::Other(
                "ACL tuple requires captured relation ownership".into(),
            ));
        }
        for replacement in records.iter() {
            progress.checkpoint().await?;
            apply(security, memory, replacement, progress).await?;
        }
        Ok(())
    }
}

async fn copy(
    acl: Option<&[Entry]>,
    progress: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<Budgeted<Option<Vec<Entry>>>> {
    let Some(acl) = acl else {
        return Ok(Budgeted::new(None, progress.memory.empty_reservation()));
    };
    let mut values = BudgetedVec::new(progress.memory);
    values.reserve(acl.len())?;
    for entries in acl.chunks(64) {
        progress.checkpoint().await?;
        for entry in entries {
            values.push(entry.clone())?;
        }
    }
    let (values, memory) = values.into_parts();
    Ok(Budgeted::new(Some(values), memory))
}

async fn apply(
    authority: &mut RelationSecurityRow,
    memory: &mut MemoryReservation,
    replacement: &Replacement,
    progress: &mut AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    if replacement.column.is_some() && replacement.tuple.acl.is_none() {
        return Err(StorageBackendError::Other(
            "attribute ACL tuple has a null ACL".into(),
        ));
    }
    let copied = copy(replacement.tuple.acl.as_deref(), progress).await?;
    let RelationSecurityRow::Bound(security) = authority else {
        unreachable!()
    };
    if let Some(column) = &replacement.column {
        let (copied, payload) = copied.into_parts();
        let copied = Budgeted::new(copied.expect("column ACL"), payload);
        replace_column(&mut security.column_acls, column, copied, memory, progress)?;
        let revision = replacement.tuple.revision;
        if let Some(previous) = security.acl_revisions.columns.get_mut(column.as_str()) {
            *previous = revision;
        } else {
            let name = ProductionControl::new(
                progress.memory,
                progress.cancellation,
                progress.cancellation,
            )
            .copy_text(column)?
            .into_budgeted()
            .expect("controlled revision key");
            security
                .acl_revisions
                .columns
                .insert_admitted_key(name, revision)?;
        }
    } else {
        let (copied, payload) = copied.into_parts();
        let previous = std::mem::replace(&mut security.acl, copied);
        let bytes = previous
            .as_ref()
            .map_or(0, |acl| acl.capacity() * size_of::<Entry>());
        drop(previous);
        drop(memory.split(bytes));
        memory.absorb(payload);
        security.acl_revisions.relation = Some(replacement.tuple.revision);
    }
    Ok(())
}

fn replace_column(
    columns: &mut Columns,
    name: &str,
    value: Budgeted<Vec<Entry>>,
    memory: &mut MemoryReservation,
    progress: &AsyncImageInput<'_>,
) -> StorageBackendResult<()> {
    if value.is_empty() {
        if let Some((name, previous)) = columns.remove_entry(name) {
            let bytes =
                Columns::entry_bytes() + name.capacity() + previous.capacity() * size_of::<Entry>();
            drop((name, previous));
            drop(memory.split(bytes));
        }
    } else if let Some(previous) = columns.get_mut(name) {
        let bytes = previous.capacity() * size_of::<Entry>();
        let (value, retained) = value.into_parts();
        *previous = value;
        drop(memory.split(bytes));
        memory.absorb(retained);
    } else {
        let mut node = progress.memory.reserve(Columns::entry_bytes())?;
        let name = ProductionControl::new(
            progress.memory,
            progress.cancellation,
            progress.cancellation,
        )
        .copy_text(name)?
        .into_budgeted()
        .expect("controlled column key");
        let (name, name_memory) = name.into_parts();
        node.absorb(name_memory);
        let (value, value_memory) = value.into_parts();
        node.absorb(value_memory);
        columns.insert(name, value);
        memory.absorb(node);
    }
    Ok(())
}
