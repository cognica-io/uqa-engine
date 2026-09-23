//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Controlled captures freeze field containers while ordinary clones remain writable.

use super::{AnalyzerBindings, AnalyzerDefault, FieldRevisions};
use crate::{read_control::StorageReadControl, StorageBackendResult};
use std::{collections::BTreeMap, sync::Arc};
use uqa_analysis::CompiledAnalyzer;
use uqa_core::memory::{Budgeted, BudgetedMap, BudgetedString, MemoryReservation};

type RetainedFields = Arc<Budgeted<BudgetedMap<String, RetainedField>>>;

pub(super) struct RetainedField {
    revisions: FieldRevisions,
    _name_memory: MemoryReservation,
}

pub(super) enum FieldBindings {
    Live(BTreeMap<String, FieldRevisions>),
    Retained(RetainedFields),
}

impl Clone for FieldBindings {
    fn clone(&self) -> Self {
        // The public writable clone retains its established independent mutation semantics. Controlled captures use `retained` instead.
        Self::Live(
            self.iter()
                .map(|(name, pair)| (name.clone(), pair.clone()))
                .collect(),
        )
    }
}

impl std::fmt::Debug for FieldBindings {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl FieldBindings {
    pub(super) fn get(&self, name: &str) -> Option<&FieldRevisions> {
        match self {
            Self::Live(fields) => fields.get(name),
            Self::Retained(fields) => fields.get(name).map(|field| &field.revisions),
        }
    }

    fn iter(&self) -> impl Iterator<Item = (&String, &FieldRevisions)> {
        let (live, retained) = match self {
            Self::Live(fields) => (Some(fields), None),
            Self::Retained(fields) => (None, Some(fields)),
        };
        live.into_iter().flat_map(|fields| fields.iter()).chain(
            retained
                .into_iter()
                .flat_map(|fields| fields.iter().map(|(name, field)| (name, &field.revisions))),
        )
    }

    pub(super) fn live_mut(&mut self) -> &mut BTreeMap<String, FieldRevisions> {
        let Self::Live(fields) = self else {
            unreachable!(
                "frozen bindings are private to immutable indexes; writable clones own live fields"
            )
        };
        fields
    }
}

impl AnalyzerBindings {
    pub(in crate::inverted_index) fn from_retained_revisions<'a>(
        default: AnalyzerDefault,
        fields: impl IntoIterator<
            Item = StorageBackendResult<(&'a str, Arc<CompiledAnalyzer>, Arc<CompiledAnalyzer>)>,
        >,
        control: &StorageReadControl,
    ) -> StorageBackendResult<Self> {
        control.check()?;
        let fields = retain_fields(fields, control)?;
        Ok(Self {
            default: default.0,
            fields,
        })
    }

    pub(crate) fn retained(&self, control: &StorageReadControl) -> StorageBackendResult<Self> {
        control.check()?;
        let fields = match &self.fields {
            FieldBindings::Retained(fields) => FieldBindings::Retained(Arc::clone(fields)),
            FieldBindings::Live(fields) if fields.is_empty() => {
                FieldBindings::Live(BTreeMap::new())
            }
            FieldBindings::Live(_) => retain_fields(
                self.fields.iter().map(|(name, pair)| {
                    Ok((
                        name.as_str(),
                        Arc::clone(&pair.index),
                        Arc::clone(&pair.search),
                    ))
                }),
                control,
            )?,
        };
        Ok(Self {
            default: Arc::clone(&self.default),
            fields,
        })
    }
}

fn retain_fields<'a>(
    fields: impl IntoIterator<
        Item = StorageBackendResult<(&'a str, Arc<CompiledAnalyzer>, Arc<CompiledAnalyzer>)>,
    >,
    control: &StorageReadControl,
) -> StorageBackendResult<FieldBindings> {
    let mut retained = BudgetedMap::new(control.memory());
    for field in fields {
        control.check()?;
        let (name, index, search) = field?;
        let mut owned = BudgetedString::new(control.memory());
        owned.reserve(name.len())?;
        for (offset, character) in name.chars().enumerate() {
            if offset % 1024 == 0 {
                control.check()?;
            }
            owned.push(character)?;
        }
        let (name, memory) = owned.into_parts();
        retained.insert(
            name,
            RetainedField {
                revisions: FieldRevisions { index, search },
                _name_memory: memory,
            },
        )?;
    }
    control.check()?;
    if retained.is_empty() {
        return Ok(FieldBindings::Live(BTreeMap::new()));
    }
    Ok(FieldBindings::Retained(
        Budgeted::new(retained, control.memory().empty_reservation()).into_shared()?,
    ))
}
