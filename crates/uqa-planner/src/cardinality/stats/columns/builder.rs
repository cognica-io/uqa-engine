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

pub(crate) struct Builder<'a> {
    columns: Map,
    control: ProductionControl<'a>,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(control: ProductionControl<'a>) -> Self {
        Self {
            columns: control.budget().map_or_else(
                || Map::Ordinary(BTreeMap::new()),
                |budget| Map::Admitted(BudgetedMap::new(budget)),
            ),
            control,
        }
    }

    pub(crate) fn insert(
        &mut self,
        name: &str,
        stats: Produced<ColumnStats>,
    ) -> Result<(), SQLError> {
        let name = self.control.copy_text(name)?;
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
        self.control.check()?;
        Ok(match self.columns {
            Map::Ordinary(columns) => columns.into(),
            Map::Admitted(columns) if columns.is_empty() => ColumnStatistics::default(),
            Map::Admitted(columns) => ColumnStatistics(Columns::Admitted(
                self.control
                    .finish(columns, self.control.empty_reservation())?
                    .into_shared(&self.control)?,
            )),
        })
    }
}
