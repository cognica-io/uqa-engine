//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Legacy partition counters are selected from complete membership before any counter is changed.

use uqa_core::{
    image_io::{ImageDecodeWindow, ImageDecoder},
    memory::{MemoryReservation, OwnedMap, OwnedSet, ProductionControl, ProductionVec},
    RelationIdentity,
};
use uqa_sql::{
    ast::{ColumnDef, TableHierarchy},
    catalog::table_definition::SharedTableDefinition,
};
use uqa_storage::{read_control::StorageReadControl, StorageBackendError, StorageBackendResult};

mod work;
use work::Work;

#[cfg(test)]
mod tests;

/// Retained table-state adapters lend immutable declarations and the current physical counter.
pub trait WatermarkTable: Clone + Send + Sync {
    fn columns(&self) -> SharedTableDefinition<Vec<ColumnDef>>;
    fn hierarchy(&self) -> SharedTableDefinition<TableHierarchy>;
    fn watermark(&self) -> StorageBackendResult<u128>;
    /// Apply a monotone counter update through its existing state owner; this operation cannot fail.
    fn raise_watermark(&self, minimum: u128);
}

/// One immutable table-membership generation. Individual declaration and counter reads retain their original observation points.
pub trait WatermarkTables: Send + Sync {
    type Table: WatermarkTable;
    fn tables(&self) -> impl Iterator<Item = (&RelationIdentity, Self::Table)> + Send;
}

pub struct WatermarkUpdate<T: WatermarkTable> {
    table: T,
    minimum: u128,
}

impl<T: WatermarkTable> WatermarkUpdate<T> {
    pub fn publish(self) {
        self.table.raise_watermark(self.minimum);
    }
}

/// A complete, ordered update list. No table has changed while this plan is constructed; dropping an unread plan releases all native handles and its vector allowance.
pub struct WatermarkPlan<T: WatermarkTable> {
    updates: std::vec::IntoIter<WatermarkUpdate<T>>,
    _memory: Option<MemoryReservation>,
}

impl<T: WatermarkTable> Iterator for WatermarkPlan<T> {
    type Item = WatermarkUpdate<T>;

    fn next(&mut self) -> Option<Self::Item> {
        self.updates.next()
    }
}

pub struct WatermarkInput<T: WatermarkTable> {
    input: ImageDecoder<WatermarkPlan<T>, StorageBackendError>,
}

impl<T: WatermarkTable + 'static> WatermarkInput<T> {
    pub fn new(
        tables: impl WatermarkTables<Table = T> + 'static,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let retained = control.clone();
        let input = ImageDecoder::new(
            control.memory(),
            control.cancellation(),
            |mut input| async move {
                let mut input = input.input();
                let control = ProductionControl::new(
                    retained.memory(),
                    retained.cancellation(),
                    input.cancellation,
                );
                collect(&tables, &control, &mut input).await
            },
        )?;
        Ok(Self { input })
    }

    /// Bound name production, column and ancestor visits, and update construction. Every terminal path consumes the unpublished plan.
    pub fn advance(&mut self, steps: usize) -> StorageBackendResult<Option<WatermarkPlan<T>>> {
        self.input
            .advance(&mut std::io::empty(), ImageDecodeWindow { bytes: 1, steps })
    }
}

pub fn plan<T: WatermarkTables>(tables: &T) -> StorageBackendResult<WatermarkPlan<T::Table>> {
    work::inline(collect(tables, &ProductionControl::uncontrolled(), &mut ()))
}

async fn collect<T: WatermarkTables>(
    tables: &T,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> StorageBackendResult<WatermarkPlan<T::Table>> {
    control.check()?;
    let mut entries = OwnedMap::<String, T::Table>::new();
    for (relation, table) in tables.tables() {
        work.checkpoint().await?;
        let name = work.name(relation, control).await?;
        if control.budget().is_some() {
            let name = name.into_budgeted().expect("controlled watermark name");
            entries.insert_admitted_key(name, table)?;
        } else {
            entries.insert(
                name.into_uncontrolled().expect("ordinary watermark name"),
                table,
            );
        }
    }
    let mut roots = OwnedMap::<&str, u128>::new();
    for (name, table) in &entries {
        work.checkpoint().await?;
        let columns = table.columns();
        let mut legacy = false;
        for column in columns.iter() {
            work.checkpoint().await?;
            control.check()?;
            if column
                .auto_increment
                .as_ref()
                .is_some_and(uqa_sql::ast::AutoIncrement::is_legacy)
            {
                legacy = true;
                break;
            }
        }
        drop(columns);
        if !legacy {
            continue;
        }
        let root = root(name, &entries, control, work).await?;
        if let Some(root) = root {
            let watermark = table.watermark()?;
            if let Some(current) = roots.get_mut(root) {
                *current = (*current).max(watermark);
            } else if let Some(memory) = control.budget() {
                roots.insert_budgeted(root, watermark, memory)?;
            } else {
                roots.insert(root, watermark);
            }
        }
    }
    let mut updates = ProductionVec::new(*control);
    for (name, minimum) in &roots {
        work.checkpoint().await?;
        control.check()?;
        let table = entries.get(*name).ok_or_else(|| {
            invalid(format!(
                "partition hierarchy references missing root `{name}`"
            ))
        })?;
        updates.reserve(1)?;
        updates.push_produced(control.finish(
            WatermarkUpdate {
                table: table.clone(),
                minimum: *minimum,
            },
            control.empty_reservation(),
        )?)?;
    }
    let (updates, memory) = updates.finish()?.into_parts();
    Ok(WatermarkPlan {
        updates: updates.into_iter(),
        _memory: memory,
    })
}

async fn root<'a, T: WatermarkTable>(
    start: &'a str,
    entries: &'a OwnedMap<String, T>,
    control: &ProductionControl<'_>,
    work: &mut impl Work,
) -> StorageBackendResult<Option<&'a str>> {
    let mut current = start;
    let mut visited = OwnedSet::new();
    let mut participates = false;
    loop {
        work.checkpoint().await?;
        control.check()?;
        let inserted = match control.budget() {
            Some(memory) => visited.insert_budgeted(current, memory)?,
            None => visited.insert(current),
        };
        if !inserted {
            return Err(invalid(format!(
                "partition hierarchy cycle reaches `{current}`"
            )));
        }
        let table = entries.get(current).ok_or_else(|| {
            invalid(format!(
                "partition hierarchy references missing table `{current}`"
            ))
        })?;
        let hierarchy = table.hierarchy();
        participates |= hierarchy.partition_spec.is_some() || hierarchy.is_partition();
        if !hierarchy.is_partition() {
            return Ok(participates.then_some(current));
        }
        let parent = hierarchy
            .parents
            .first()
            .ok_or_else(|| invalid("partition has no parent relation"))?;
        let (name, _) = entries.get_key_value(parent.as_str()).ok_or_else(|| {
            invalid(format!(
                "partition hierarchy references missing table `{parent}`"
            ))
        })?;
        current = name;
    }
}

fn invalid(message: impl Into<String>) -> StorageBackendError {
    StorageBackendError::Other(message.into())
}
