//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Shared substitutions keep their independently admitted fields through immutable query copies.

use super::{ColumnIdentity, OuterReplacement};
use crate::SQLError;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};
type SharedEntry = Arc<Produced<(ColumnIdentity, OuterReplacement)>>;

#[derive(Debug, Clone)]
pub(super) enum Replacements {
    Ordinary(Arc<BTreeMap<ColumnIdentity, OuterReplacement>>),
    Controlled(Arc<Produced<Vec<SharedEntry>>>),
}
impl Replacements {
    pub(super) fn from_inputs_owned(
        inputs: uqa_core::memory::OwnedMap<
            Produced<ColumnIdentity>,
            Produced<crate::plan::ExpressionPlan>,
        >,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut values = ProductionVec::new(*control);
        for (identity, expression) in inputs {
            control.check()?;
            let (identity, identity_memory) = identity.into_parts();
            let (expression, expression_memory) = expression.into_parts();
            let replacement = OuterReplacement {
                expression,
                qualifier: String::new(),
                memory: control.combine(identity_memory, expression_memory),
            };
            let entry = control
                .finish((identity, replacement), control.empty_reservation())?
                .into_shared(control)?;
            values.push_produced(control.finish(entry, control.empty_reservation())?)?;
        }
        Ok(Self::Controlled(values.finish()?.into_shared(control)?))
    }

    pub(super) fn ordinary(values: BTreeMap<ColumnIdentity, OuterReplacement>) -> Self {
        Self::Ordinary(Arc::new(values))
    }
    pub(super) fn iter(&self) -> Entries<'_> {
        match self {
            Self::Ordinary(values) => Entries::Ordinary(values.iter()),
            Self::Controlled(values) => Entries::Controlled(values.iter()),
        }
    }
    pub(super) fn values(&self) -> impl Iterator<Item = &OuterReplacement> {
        self.iter().map(|(_, value)| value)
    }
    pub(super) fn keys(&self) -> impl Iterator<Item = &ColumnIdentity> {
        self.iter().map(|(key, _)| key)
    }
    pub(super) fn is_empty(&self) -> bool {
        match self {
            Self::Ordinary(values) => values.is_empty(),
            Self::Controlled(values) => values.is_empty(),
        }
    }
    pub(super) fn get(&self, key: &ColumnIdentity) -> Option<&OuterReplacement> {
        match self {
            Self::Ordinary(values) => values.get(key),
            Self::Controlled(values) => values
                .binary_search_by(|entry| entry.0.cmp(key))
                .ok()
                .map(|position| &values[position].1),
        }
    }
    /// Legacy mutable analysis owns a new ordinary tree. Native controlled transformations use the consuming producer instead of this COW boundary.
    pub(super) fn ordinary_mut(&mut self) -> &mut BTreeMap<ColumnIdentity, OuterReplacement> {
        if let Self::Controlled(values) = self {
            let values = values
                .iter()
                .map(|entry| (entry.0.clone(), entry.1.clone()))
                .collect();
            *self = Self::ordinary(values);
        }
        let Self::Ordinary(values) = self else {
            unreachable!()
        };
        Arc::make_mut(values)
    }
}

pub(super) enum Entries<'a> {
    Ordinary(std::collections::btree_map::Iter<'a, ColumnIdentity, OuterReplacement>),
    Controlled(std::slice::Iter<'a, SharedEntry>),
}
impl<'a> Iterator for Entries<'a> {
    type Item = (&'a ColumnIdentity, &'a OuterReplacement);
    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Ordinary(values) => values.next(),
            Self::Controlled(values) => values.next().map(|entry| (&entry.0, &entry.1)),
        }
    }
}

impl Replacements {
    pub(super) fn selected(
        &self,
        control: &ProductionControl<'_>,
        mut keep: impl FnMut(&ColumnIdentity) -> bool,
    ) -> Result<Self, SQLError> {
        control.check()?;
        let mut values = ProductionVec::new(*control);
        for (key, _) in self.iter() {
            control.check()?;
            if keep(key) {
                let entry = self.share_entry(key, control)?;
                values.push_produced(control.finish(entry, control.empty_reservation())?)?;
            }
        }
        Ok(Self::Controlled(values.finish()?.into_shared(control)?))
    }

    pub(super) fn merged(
        &self,
        incoming: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut values = ProductionVec::new(*control);
        let mut current = self.iter().peekable();
        let mut next = incoming.iter().peekable();
        loop {
            control.check()?;
            let (owner, key) = match (current.peek(), next.peek()) {
                (Some((left, _)), Some((right, _))) if left < right => {
                    (self, current.next().expect("current key").0)
                }
                (Some((left, _)), Some((right, _))) if left == right => {
                    current.next();
                    (incoming, next.next().expect("incoming key").0)
                }
                (_, Some(_)) => (incoming, next.next().expect("incoming key").0),
                (Some(_), None) => (self, current.next().expect("current key").0),
                (None, None) => break,
            };
            values.push_produced(control.finish(
                owner.share_entry(key, control)?,
                control.empty_reservation(),
            )?)?;
        }
        Ok(Self::Controlled(values.finish()?.into_shared(control)?))
    }

    fn share_entry(
        &self,
        key: &ColumnIdentity,
        control: &ProductionControl<'_>,
    ) -> Result<SharedEntry, SQLError> {
        if let Self::Controlled(values) = self {
            let index = values
                .binary_search_by(|entry| entry.0.cmp(key))
                .expect("selected replacement");
            return Ok(values[index].clone());
        }
        let value = self.get(key).expect("selected replacement");
        let identity =
            ColumnIdentity::from_parts_with_control(key.qualifier(), key.column(), control)?;
        let qualifier = control.copy_text(&value.qualifier)?;
        let expression = value
            .expression
            .copy_for_rewrite(control)?
            .finish(control)?;
        let (identity, identity_memory) = identity.into_parts();
        let (qualifier, qualifier_memory) = qualifier.into_parts();
        let (expression, expression_memory) = expression.into_parts();
        let value = OuterReplacement {
            expression,
            qualifier,
            memory: control.combine(
                identity_memory,
                control.combine(qualifier_memory, expression_memory),
            ),
        };
        Ok(control
            .finish((identity, value), control.empty_reservation())?
            .into_shared(control)?)
    }
}
