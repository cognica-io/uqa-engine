//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed namespace observations produce only the surviving shadow-name sets and replacement handles.

use super::{DeferredOuterRewrite, RowSchema, SQLError};
use uqa_core::memory::{MemoryReservation, OwnedSet, Produced, ProductionControl};

#[derive(Clone, Copy)]
pub(super) enum Selection {
    All,
    Qualified,
    Unqualified,
}

pub(super) fn in_namespace(
    rewrite: &DeferredOuterRewrite,
    local: Option<&RowSchema>,
    selection: Selection,
    control: &ProductionControl<'_>,
) -> Result<Produced<DeferredOuterRewrite>, SQLError> {
    let mut columns = Names::new(control);
    let mut qualifiers = Names::new(control);
    for column in &rewrite.shadowed_columns {
        columns.insert(column, control)?;
    }
    for qualifier in &rewrite.shadowed_qualifiers {
        qualifiers.insert(qualifier, control)?;
    }
    if let Some(local) = local {
        for (position, name) in local.columns().iter().enumerate() {
            columns.insert(local.public_name(position).unwrap_or(name), control)?;
        }
        for identity in rewrite.replacements.keys() {
            control.check()?;
            match identity.qualifier() {
                Some(qualifier) if local.has_qualifier(qualifier) => {
                    qualifiers.insert(qualifier, control)?;
                }
                None if local.has_unqualified_column(identity.column())
                    || local.has_qualifier(identity.column()) =>
                {
                    columns.insert(identity.column(), control)?;
                }
                _ => {}
            }
        }
    }
    let replacements = match selection {
        Selection::All => rewrite.replacements.clone(),
        Selection::Qualified => rewrite
            .replacements
            .selected(control, |identity| identity.qualifier().is_some())?,
        Selection::Unqualified => rewrite
            .replacements
            .selected(control, |identity| identity.qualifier().is_none())?,
    };
    finish(
        rewrite.outer_columns.clone(),
        replacements,
        columns,
        qualifiers,
        control,
    )
}

pub(super) fn merged(
    current: &DeferredOuterRewrite,
    incoming: &DeferredOuterRewrite,
    control: &ProductionControl<'_>,
) -> Result<Produced<DeferredOuterRewrite>, SQLError> {
    let mut columns = Names::new(control);
    let mut qualifiers = Names::new(control);
    for column in current
        .shadowed_columns
        .iter()
        .chain(incoming.shadowed_columns.iter())
    {
        columns.insert(column, control)?;
    }
    for qualifier in current
        .shadowed_qualifiers
        .iter()
        .chain(incoming.shadowed_qualifiers.iter())
    {
        qualifiers.insert(qualifier, control)?;
    }
    finish(
        current.outer_columns.clone(),
        current
            .replacements
            .merged(&incoming.replacements, control)?,
        columns,
        qualifiers,
        control,
    )
}

fn finish(
    outer_columns: std::sync::Arc<
        Produced<std::collections::BTreeMap<crate::ColumnIdentity, crate::ast::InternalColumnRef>>,
    >,
    replacements: super::super::replacements::Replacements,
    columns: Names,
    qualifiers: Names,
    control: &ProductionControl<'_>,
) -> Result<Produced<DeferredOuterRewrite>, SQLError> {
    let Names {
        values: shadowed_columns,
        memory: columns_memory,
    } = columns;
    let Names {
        values: shadowed_qualifiers,
        memory: qualifiers_memory,
    } = qualifiers;
    Ok(control.finish(
        DeferredOuterRewrite {
            outer_columns,
            replacements,
            shadowed_qualifiers,
            shadowed_columns,
        },
        control.combine(columns_memory, qualifiers_memory),
    )?)
}

struct Names {
    values: OwnedSet<String>,
    memory: Option<MemoryReservation>,
}
impl Names {
    fn new(control: &ProductionControl<'_>) -> Self {
        Self {
            values: OwnedSet::new(),
            memory: control.empty_reservation(),
        }
    }
    fn insert(&mut self, name: &str, control: &ProductionControl<'_>) -> Result<(), SQLError> {
        control.check()?;
        if self.values.contains(name) {
            return Ok(());
        }
        let node = control.reserve(OwnedSet::<String>::entry_bytes())?;
        let name = control.copy_text(name)?;
        let (name, memory) = name.into_parts();
        self.memory = control.combine(self.memory.take(), control.combine(node, memory));
        self.values.insert(name);
        Ok(())
    }
}
