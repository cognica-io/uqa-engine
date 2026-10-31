//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Constant lookup owns independently admitted keys, map nodes and mutable scalar payloads.

use std::{borrow::Borrow, cmp::Ordering};
use uqa_core::memory::{BudgetedMap, OwnedMap, Produced, ProductionControl};
use uqa_sql::{plan::OwnedScalarExpr, SQLError};

struct Name(Produced<String>);
impl Borrow<str> for Name {
    fn borrow(&self) -> &str {
        &self.0
    }
}
impl PartialEq for Name {
    fn eq(&self, other: &Self) -> bool {
        *self.0 == *other.0
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
        self.0.cmp(&other.0)
    }
}

enum Entries {
    Ordinary(OwnedMap<Name, Option<OwnedScalarExpr>>),
    Controlled(BudgetedMap<Name, Option<OwnedScalarExpr>>),
}

pub(super) struct Constants {
    entries: Entries,
}

impl Constants {
    pub(super) fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            entries: control.budget().map_or_else(
                || Entries::Ordinary(OwnedMap::new()),
                |budget| Entries::Controlled(BudgetedMap::new(budget)),
            ),
        }
    }

    pub(super) fn get(&self, name: &str) -> Option<&OwnedScalarExpr> {
        match &self.entries {
            Entries::Ordinary(entries) => entries.get(name),
            Entries::Controlled(entries) => entries.get(name),
        }
        .and_then(Option::as_ref)
    }

    pub(super) fn insert(
        &mut self,
        name: Produced<String>,
        value: Option<OwnedScalarExpr>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        let existing = match &mut self.entries {
            Entries::Ordinary(entries) => entries.get_mut(name.as_str()),
            Entries::Controlled(entries) => entries.get_mut(name.as_str()),
        };
        if let Some(existing) = existing {
            *existing = None;
            return Ok(());
        }
        match &mut self.entries {
            Entries::Ordinary(entries) => {
                entries.insert(Name(name), value);
            }
            Entries::Controlled(entries) => {
                entries.insert(Name(name), value)?;
            }
        }
        Ok(())
    }
}
