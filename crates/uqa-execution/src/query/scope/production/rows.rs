//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use crate::query::cte::rows::CteRows;
use std::{borrow::Borrow, cmp::Ordering, sync::Arc};
use uqa_core::memory::{BudgetedMapIter, OwnedMap, Produced, ProductionControl};
use uqa_sql::SQLError;

#[derive(Clone)]
enum Name {
    Ordinary(String),
    Captured(Arc<Produced<String>>),
}
impl Name {
    fn value(&self) -> &String {
        match self {
            Self::Ordinary(value) => value,
            Self::Captured(value) => value,
        }
    }
}
impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        self.value()
    }
}
impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        self.value() == other.value()
    }
}
impl Eq for Name {}
impl PartialOrd for Name {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Name {
    fn cmp(&self, other: &Self) -> Ordering {
        self.value().cmp(other.value())
    }
}

/// Ordered lexical row bindings. Admitted keys retain their text through every shared name owner, while each exact map node releases its own allowance when removed.
#[derive(Clone, Default)]
pub struct CteRowsMap {
    rows: OwnedMap<Name, CteRows>,
}

impl CteRowsMap {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn len(&self) -> usize {
        self.rows.len()
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
    pub fn get(&self, name: &str) -> Option<&CteRows> {
        self.rows.get(name)
    }
    pub fn get_mut(&mut self, name: &str) -> Option<&mut CteRows> {
        self.rows.get_mut(name)
    }
    pub fn contains_key(&self, name: &str) -> bool {
        self.rows.contains_key(name)
    }
    pub fn remove(&mut self, name: &str) -> Option<CteRows> {
        self.rows.remove(name)
    }
    pub fn clear(&mut self) {
        self.rows.clear();
    }
    pub fn iter(&self) -> CteRowsIter<'_> {
        CteRowsIter(self.rows.iter())
    }
    pub fn keys(&self) -> impl ExactSizeIterator<Item = &String> {
        self.iter().map(|(name, _)| name)
    }
    pub fn values(&self) -> impl ExactSizeIterator<Item = &CteRows> {
        self.iter().map(|(_, rows)| rows)
    }

    pub fn insert(&mut self, name: String, rows: CteRows) -> Option<CteRows> {
        if let Some(previous) = self.rows.get_mut(name.as_str()) {
            return Some(std::mem::replace(previous, rows));
        }
        self.rows.insert(Name::Ordinary(name), rows)
    }

    pub(in crate::query::scope) fn insert_with_control(
        &mut self,
        name: &str,
        rows: CteRows,
        control: &ProductionControl<'_>,
    ) -> Result<Option<CteRows>, SQLError> {
        control.check()?;
        if let Some(previous) = self.rows.get_mut(name) {
            return Ok(Some(std::mem::replace(previous, rows)));
        }
        let name = Name::Captured(control.copy_text(name)?.into_shared(control)?);
        Ok(if let Some(budget) = control.budget() {
            self.rows.insert_budgeted(name, rows, budget)?
        } else {
            self.rows.insert(name, rows)
        })
    }
}

impl Extend<(String, CteRows)> for CteRowsMap {
    fn extend<I: IntoIterator<Item = (String, CteRows)>>(&mut self, entries: I) {
        for (name, rows) in entries {
            self.insert(name, rows);
        }
    }
}
impl FromIterator<(String, CteRows)> for CteRowsMap {
    fn from_iter<I: IntoIterator<Item = (String, CteRows)>>(entries: I) -> Self {
        let mut map = Self::new();
        map.extend(entries);
        map
    }
}
impl std::ops::Index<&str> for CteRowsMap {
    type Output = CteRows;
    fn index(&self, name: &str) -> &Self::Output {
        self.get(name).expect("missing lexical row binding")
    }
}

pub struct CteRowsIter<'a>(BudgetedMapIter<'a, Name, CteRows>);
impl<'a> Iterator for CteRowsIter<'a> {
    type Item = (&'a String, &'a CteRows);
    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(name, rows)| (name.value(), rows))
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.0.size_hint()
    }
}
impl ExactSizeIterator for CteRowsIter<'_> {}
impl std::iter::FusedIterator for CteRowsIter<'_> {}
impl<'a> IntoIterator for &'a CteRowsMap {
    type Item = (&'a String, &'a CteRows);
    type IntoIter = CteRowsIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
