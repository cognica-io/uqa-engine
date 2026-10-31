//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{BTreeMap, ColumnStats};
use std::sync::Arc;
use uqa_core::memory::{BudgetedMap, Produced, ProductionControl};
use uqa_sql::SQLError;

mod builder;
mod copy;
pub(crate) use builder::Builder;
use builder::ColumnName;
#[cfg(test)]
mod tests;

type AdmittedColumns = BudgetedMap<ColumnName, Produced<ColumnStats>>;

/// Column statistics shared by cost estimators, relation descriptors and catalog readers. Empty collections allocate nothing. Captured catalog generations keep their original ownership, including producer reservations for newly collected distributions.
#[derive(Clone, Default)]
pub struct ColumnStatistics(Columns);

#[derive(Clone, Default)]
enum Columns {
    #[default]
    Empty,
    Ordinary(Arc<BTreeMap<String, ColumnStats>>),
    Admitted(Arc<Produced<AdmittedColumns>>),
}

impl From<BTreeMap<String, ColumnStats>> for ColumnStatistics {
    fn from(columns: BTreeMap<String, ColumnStats>) -> Self {
        if columns.is_empty() {
            Self::default()
        } else {
            Self(Columns::Ordinary(Arc::new(columns)))
        }
    }
}

impl From<Arc<BTreeMap<String, ColumnStats>>> for ColumnStatistics {
    fn from(columns: Arc<BTreeMap<String, ColumnStats>>) -> Self {
        Self(Columns::Ordinary(columns))
    }
}

impl FromIterator<(String, ColumnStats)> for ColumnStatistics {
    fn from_iter<T: IntoIterator<Item = (String, ColumnStats)>>(columns: T) -> Self {
        BTreeMap::from_iter(columns).into()
    }
}

impl ColumnStatistics {
    pub fn len(&self) -> usize {
        match &self.0 {
            Columns::Empty => 0,
            Columns::Ordinary(columns) => columns.len(),
            Columns::Admitted(columns) => columns.len(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn get(&self, name: &str) -> Option<&ColumnStats> {
        match &self.0 {
            Columns::Empty => None,
            Columns::Ordinary(columns) => columns.get(name),
            Columns::Admitted(columns) => columns.get(name).map(|value| &**value),
        }
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn get_key_value(&self, name: &str) -> Option<(&String, &ColumnStats)> {
        match &self.0 {
            Columns::Empty => None,
            Columns::Ordinary(columns) => columns.get_key_value(name),
            Columns::Admitted(columns) => columns
                .get_key_value(name)
                .map(|(name, value)| (&*name.0, &**value)),
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &ColumnStats)> {
        let ordinary = match &self.0 {
            Columns::Ordinary(columns) => Some(columns.iter()),
            _ => None,
        };
        let admitted = match &self.0 {
            Columns::Admitted(columns) => Some(columns.iter()),
            _ => None,
        };
        ordinary.into_iter().flatten().chain(
            admitted
                .into_iter()
                .flatten()
                .map(|(name, value)| (&*name.0, &**value)),
        )
    }

    pub fn values(&self) -> impl Iterator<Item = &ColumnStats> {
        self.iter().map(|(_, value)| value)
    }

    pub fn keys(&self) -> impl Iterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }

    /// Independent ordinary copy for legacy callers that explicitly request an owned map. Native planning retains this immutable handle instead.
    pub fn to_owned_map(&self) -> BTreeMap<String, ColumnStats> {
        self.iter()
            .map(|(name, stats)| (name.clone(), stats.clone()))
            .collect()
    }

    /// Construct a replacement generation before publication. Unchanged columns and new distributions are copied through their producer under the invoking allowance; no overwritten old snapshot is retained as an ownership placeholder.
    pub fn with_replacements(
        &self,
        replacements: &Self,
        control: ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut output = Builder::new(control);
        for (name, stats) in self
            .iter()
            .filter(|(name, _)| !replacements.contains_key(name))
            .chain(replacements.iter())
        {
            control.check()?;
            output.insert(name, copy::column(stats, control)?)?;
        }
        output.finish()
    }

    /// Standalone statistics builders detach before changing a shared generation. Native publication uses the admitted producer.
    pub(super) fn insert(&mut self, name: String, stats: ColumnStats) {
        if !matches!(self.0, Columns::Ordinary(_)) {
            self.0 = Columns::Ordinary(Arc::new(self.to_owned_map()));
        }
        let Columns::Ordinary(columns) = &mut self.0 else {
            unreachable!()
        };
        Arc::make_mut(columns).insert(name, stats);
    }
}

impl std::ops::Index<&str> for ColumnStatistics {
    type Output = ColumnStats;
    fn index(&self, name: &str) -> &Self::Output {
        self.get(name).expect("missing column statistics")
    }
}

impl std::fmt::Debug for ColumnStatistics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}
