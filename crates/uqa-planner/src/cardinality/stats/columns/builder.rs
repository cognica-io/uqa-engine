//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    AdmittedColumns, BTreeMap, BudgetedMap, ColumnStatistics, ColumnStats, Columns, Produced,
    ProductionControl, SQLError,
};
use std::{borrow::Borrow, cmp::Ordering};
use uqa_core::memory::{MemoryError, OwnedMap, OwnedProductionControl};

pub(super) struct ColumnName(pub(super) Produced<String>);

impl Borrow<str> for ColumnName {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl PartialEq for ColumnName {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
    }
}
impl Eq for ColumnName {}
impl PartialOrd for ColumnName {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for ColumnName {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.cmp(&other.0)
    }
}

enum Map {
    Ordinary(BTreeMap<String, ColumnStats>),
    Admitted(AdmittedColumns),
}

pub(crate) struct Builder {
    columns: Map,
    control: OwnedProductionControl,
    retained_bytes: Option<usize>,
}

impl Builder {
    pub(crate) fn new(control: ProductionControl<'_>) -> Self {
        Self {
            columns: control.budget().map_or_else(
                || Map::Ordinary(BTreeMap::new()),
                |budget| Map::Admitted(BudgetedMap::new(budget)),
            ),
            control: control.capture(),
            retained_bytes: None,
        }
    }

    /// Stored rows have a bounded encoded size; account each completed row while it is current so final publication never revisits the accumulated population.
    pub(crate) fn stored(control: ProductionControl<'_>) -> Self {
        let mut builder = Self::new(control);
        builder.retained_bytes = Some(size_of::<Produced<AdmittedColumns>>());
        builder
    }

    fn entry_bytes(&self, name: &str) -> Result<usize, SQLError> {
        let Map::Admitted(columns) = &self.columns else {
            unreachable!("stored statistics use an admitted map")
        };
        let Some((name, stats)) = columns.get_key_value(name) else {
            return Ok(0);
        };
        let mut bytes = OwnedMap::<ColumnName, Produced<ColumnStats>>::entry_bytes();
        super::footprint::column(&name.0, stats, &self.control.control(), &mut bytes)?;
        Ok(bytes)
    }

    pub(crate) fn insert(
        &mut self,
        name: &str,
        stats: Produced<ColumnStats>,
    ) -> Result<(), SQLError> {
        let previous = self
            .retained_bytes
            .map(|_| self.entry_bytes(name))
            .transpose()?;
        let owned_name = self.control.control().copy_text(name)?;
        self.insert_owned(owned_name, stats)?;
        if let (Some(bytes), Some(previous)) = (self.retained_bytes, previous) {
            let current = self.entry_bytes(name)?;
            self.retained_bytes = Some(
                bytes
                    .checked_sub(previous)
                    .and_then(|bytes| bytes.checked_add(current))
                    .ok_or(MemoryError::SizeOverflow)?,
            );
        }
        Ok(())
    }

    pub(super) fn insert_owned(
        &mut self,
        name: Produced<String>,
        stats: Produced<ColumnStats>,
    ) -> Result<(), SQLError> {
        self.control.control().check()?;
        match &mut self.columns {
            Map::Ordinary(columns) => {
                columns.insert(
                    name.into_uncontrolled().expect("ordinary column name"),
                    stats.into_uncontrolled().expect("ordinary distribution"),
                );
            }
            Map::Admitted(columns) => {
                columns.insert(ColumnName(name), stats)?;
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<ColumnStatistics, SQLError> {
        self.control.control().check()?;
        Ok(match self.columns {
            Map::Ordinary(columns) => columns.into(),
            Map::Admitted(columns) if columns.is_empty() => ColumnStatistics::default(),
            Map::Admitted(columns) => {
                let control = self.control.control();
                let bytes = match self.retained_bytes {
                    Some(bytes) => bytes,
                    None => super::footprint::admitted(&columns, &control)?,
                };
                ColumnStatistics(
                    Columns::Admitted(
                        control
                            .finish(columns, control.empty_reservation())?
                            .into_shared(&control)?,
                    ),
                    Ok(bytes),
                )
            }
        })
    }
}
