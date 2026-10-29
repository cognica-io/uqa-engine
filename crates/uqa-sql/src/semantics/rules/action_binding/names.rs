//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Replaceable rule-scope names retain independent payload and ordered-node owners.

use crate::SQLError;
use uqa_core::memory::{BudgetedVec, OwnedMap, OwnedSet, Produced, ProductionControl};

pub(super) type Names = OwnedSet<Produced<String>>;
pub(super) type Named<T> = OwnedMap<Produced<String>, T>;

pub(super) fn folded_cmp(name: &str, stored: &str) -> std::cmp::Ordering {
    name.bytes()
        .map(|byte| byte.to_ascii_lowercase())
        .cmp(stored.bytes())
}
pub(super) fn folded(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<String>, SQLError> {
    let (mut text, memory) = control.copy_text(name)?.into_parts();
    text.make_ascii_lowercase();
    Ok(control.finish(text, memory)?)
}
pub(super) fn insert_name(
    names: &mut Names,
    name: &str,
    lowercase: bool,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    control.check()?;
    if if lowercase {
        names.contains_by(|stored| folded_cmp(name, stored))
    } else {
        names.contains(name)
    } {
        return Ok(());
    }
    let name = if lowercase {
        folded(name, control)?
    } else {
        control.copy_text(name)?
    };
    match control.budget() {
        Some(budget) => {
            names.insert_budgeted(name, budget)?;
        }
        None => {
            names.insert(name);
        }
    }
    Ok(())
}
pub(super) fn insert<T>(
    names: &mut Named<T>,
    name: &str,
    value: T,
    control: &ProductionControl<'_>,
) -> Result<(), SQLError> {
    let name = folded(name, control)?;
    if let Some(previous) = names.get_mut(name.as_str()) {
        *previous = value;
        return Ok(());
    }
    match control.budget() {
        Some(budget) => {
            names.insert_budgeted(name, value, budget)?;
        }
        None => {
            names.insert(name, value);
        }
    }
    Ok(())
}

enum Buffer {
    Ordinary(Vec<Produced<String>>),
    Controlled(BudgetedVec<Produced<String>>),
}
pub(super) struct Columns(Buffer);
impl Columns {
    pub fn new(control: &ProductionControl<'_>) -> Self {
        Self(control.budget().map_or_else(
            || Buffer::Ordinary(Vec::new()),
            |budget| Buffer::Controlled(BudgetedVec::new(budget)),
        ))
    }
    pub fn copy(&self, control: &ProductionControl<'_>) -> Result<Self, SQLError> {
        Self::from_names(self.iter().map(|name| name.as_str()), control)
    }
    pub fn from_names<'a>(
        names: impl IntoIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut output = Self::new(control);
        output.extend(names, control)?;
        Ok(output)
    }
    pub fn from_produced(
        names: Produced<Vec<String>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut output = Self::new(control);
        let (names, mut memory) = names.into_parts();
        for name in names {
            let lease = memory.as_mut().map(|memory| memory.split(name.capacity()));
            output.push_produced(control.finish(name, lease)?, control)?;
        }
        Ok(output)
    }
    pub fn push_produced(
        &mut self,
        name: Produced<String>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        match &mut self.0 {
            Buffer::Ordinary(names) => names.push(name),
            Buffer::Controlled(names) => names.push(name)?,
        }
        Ok(())
    }
    pub fn push(&mut self, name: &str, control: &ProductionControl<'_>) -> Result<(), SQLError> {
        self.push_produced(control.copy_text(name)?, control)
    }
    pub fn extend<'a>(
        &mut self,
        names: impl IntoIterator<Item = &'a str>,
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        for name in names {
            self.push(name, control)?;
        }
        Ok(())
    }
    pub fn rename(
        &mut self,
        aliases: &[String],
        control: &ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        if self.is_empty() {
            return self.extend(aliases.iter().map(String::as_str), control);
        }
        let columns = match &mut self.0 {
            Buffer::Ordinary(names) => names.as_mut_slice(),
            Buffer::Controlled(names) => &mut **names,
        };
        for (column, alias) in columns.iter_mut().zip(aliases) {
            *column = control.copy_text(alias)?;
        }
        Ok(())
    }
}
impl std::ops::Deref for Columns {
    type Target = [Produced<String>];
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            Buffer::Ordinary(names) => names,
            Buffer::Controlled(names) => names,
        }
    }
}

/// Action row membership is shared by ordinary binding and admitted read-only inspection.
pub(super) trait ActionColumns {
    fn contains(&self, name: &str) -> bool;
    fn contains_folded(&self, name: &str) -> bool;
}
impl ActionColumns for std::collections::BTreeSet<String> {
    fn contains(&self, name: &str) -> bool {
        Self::contains(self, name)
    }
    fn contains_folded(&self, name: &str) -> bool {
        self.iter().any(|column| column.eq_ignore_ascii_case(name))
    }
}
impl ActionColumns for Names {
    fn contains(&self, name: &str) -> bool {
        Self::contains(self, name)
    }
    fn contains_folded(&self, name: &str) -> bool {
        self.iter().any(|column| column.eq_ignore_ascii_case(name))
    }
}
