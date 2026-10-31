//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Lateral lookup evidence owns its selected indexes, diagnostic lists and immutable shared headers.

use super::{
    Arc, Denial, DiagnosticNamespace, LateralRestrictions, RowSchema, SQLError, SchemaMap,
    SchemaSet,
};
use crate::schema::production::lease::{boxed_text, SchemaMemory};
use uqa_core::memory::{Produced, ProductionControl, ProductionMap, ProductionVec};

mod namespaces;
mod operations;

struct Selection<'a, 'c> {
    qualifiers: ProductionMap<'c, &'a str, Denial>,
    columns: ProductionMap<'c, &'a str, (&'a str, Denial)>,
    ambiguous_qualifiers: ProductionMap<'c, &'a str, ()>,
    namespaces: ProductionVec<'c, Arc<DiagnosticNamespace>>,
    column_namespaces: ProductionVec<'c, Arc<DiagnosticNamespace>>,
    unavailable_ambiguous_columns: ProductionMap<'c, &'a str, ()>,
    unavailable_qualifiers: ProductionMap<'c, &'a str, bool>,
    unavailable_counts: ProductionMap<'c, &'a str, usize>,
    unavailable_columns: ProductionMap<'c, (&'a str, usize), (&'a str, bool)>,
}

fn inline<T>(value: T, control: &ProductionControl<'_>) -> Result<Produced<T>, SQLError> {
    Ok(control.finish(value, control.empty_reservation())?)
}

impl<'a, 'c> Selection<'a, 'c> {
    fn empty(control: &ProductionControl<'c>) -> Self {
        Self {
            qualifiers: ProductionMap::new(*control),
            columns: ProductionMap::new(*control),
            ambiguous_qualifiers: ProductionMap::new(*control),
            namespaces: ProductionVec::new(*control),
            column_namespaces: ProductionVec::new(*control),
            unavailable_ambiguous_columns: ProductionMap::new(*control),
            unavailable_qualifiers: ProductionMap::new(*control),
            unavailable_counts: ProductionMap::new(*control),
            unavailable_columns: ProductionMap::new(*control),
        }
    }
    fn new(input: &'a RowSchema, control: &ProductionControl<'c>) -> Result<Self, SQLError> {
        let mut selected = Self::empty(control);
        if let Some(input) = input.index.cold.lateral_restrictions.as_deref() {
            for (key, value) in &input.qualifiers {
                selected.qualifiers.insert_copy(key.as_ref(), *value)?;
            }
            for (key, (name, denial)) in &input.columns {
                selected
                    .columns
                    .insert_copy(key.as_ref(), (name.as_ref(), *denial))?;
            }
            for key in &input.ambiguous_qualifiers {
                selected
                    .ambiguous_qualifiers
                    .insert_copy(key.as_ref(), ())?;
            }
            for key in &input.unavailable_ambiguous_columns {
                selected
                    .unavailable_ambiguous_columns
                    .insert_copy(key.as_ref(), ())?;
            }
            for (key, value) in &input.unavailable_qualifiers {
                selected
                    .unavailable_qualifiers
                    .insert_copy(key.as_ref(), *value)?;
            }
            for (key, values) in &input.unavailable_columns {
                selected.unavailable_counts.insert_copy(key.as_ref(), 0)?;
                for (namespace, hint) in values {
                    selected.append_unavailable(key, namespace, *hint)?;
                }
            }
            for namespace in &input.namespaces {
                selected
                    .namespaces
                    .push_produced(inline(namespace.clone(), control)?)?;
            }
            for namespace in &input.column_namespaces {
                selected
                    .column_namespaces
                    .push_produced(inline(namespace.clone(), control)?)?;
            }
        }
        Ok(selected)
    }
    fn append_unavailable(
        &mut self,
        column: &'a str,
        namespace: &'a str,
        hint: bool,
    ) -> Result<(), SQLError> {
        let count = self.unavailable_counts.get(column).copied().unwrap_or(0);
        self.unavailable_columns
            .insert_copy((column, count), (namespace, hint))?;
        self.unavailable_counts.insert_copy(column, count + 1)?;
        Ok(())
    }
    fn merge(
        input: &'a RowSchema,
        outer: &'a RowSchema,
        shadows: bool,
        control: &ProductionControl<'c>,
    ) -> Result<Self, SQLError> {
        let mut selected = Self::new(input, control)?;
        if selected.column_namespaces.is_empty() {
            namespaces::append(input, &mut selected.column_namespaces, false, control)?;
        }
        namespaces::append(outer, &mut selected.column_namespaces, true, control)?;
        if let Some(outer) = outer.index.cold.lateral_restrictions.as_deref() {
            for (name, reason) in &outer.qualifiers {
                if !shadows || !input.has_qualifier(name) {
                    selected.qualifiers.insert_copy(name.as_ref(), *reason)?;
                }
            }
            for (name, (namespace, reason)) in &outer.columns {
                control.check()?;
                if !shadows
                    || (!input.has_unqualified_column(name)
                        && !input.column_is_ambiguous(name)
                        && !input.has_qualifier(name))
                {
                    selected
                        .columns
                        .insert_copy(name.as_ref(), (namespace.as_ref(), *reason))?;
                }
            }
            for name in &outer.ambiguous_qualifiers {
                if !shadows || !input.has_qualifier(name) {
                    selected
                        .ambiguous_qualifiers
                        .insert_copy(name.as_ref(), ())?;
                }
            }
            if !shadows {
                for namespace in &outer.namespaces {
                    selected
                        .namespaces
                        .push_produced(inline(namespace.clone(), control)?)?;
                }
            }
            for name in &outer.unavailable_ambiguous_columns {
                selected
                    .unavailable_ambiguous_columns
                    .insert_copy(name.as_ref(), ())?;
            }
            for (name, hint) in &outer.unavailable_qualifiers {
                if selected.unavailable_qualifiers.get(name.as_ref()).is_none() {
                    selected
                        .unavailable_qualifiers
                        .insert_copy(name.as_ref(), *hint)?;
                }
            }
            for (name, values) in &outer.unavailable_columns {
                if selected.unavailable_counts.get(name.as_ref()).is_none() {
                    selected.unavailable_counts.insert_copy(name.as_ref(), 0)?;
                    for (namespace, hint) in values {
                        selected.append_unavailable(name, namespace, *hint)?;
                    }
                }
            }
        }
        Ok(selected)
    }
    fn finish(
        self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Arc<LateralRestrictions>>, SQLError> {
        control.check()?;
        if self.qualifiers.is_empty()
            && self.columns.is_empty()
            && self.ambiguous_qualifiers.is_empty()
            && self.namespaces.is_empty()
            && self.column_namespaces.is_empty()
            && self.unavailable_ambiguous_columns.is_empty()
            && self.unavailable_qualifiers.is_empty()
            && self.unavailable_counts.is_empty()
        {
            return Ok(None);
        }
        let qualifiers = SchemaMap::from_produced(
            self.qualifiers
                .iter()
                .map(|(name, value)| Ok((boxed_text(name, control)?, inline(*value, control)?))),
            control,
        )?;
        let columns = SchemaMap::from_produced(
            self.columns.iter().map(|(name, (namespace, reason))| {
                let (namespace, memory) = boxed_text(namespace, control)?.into_parts();
                Ok((
                    boxed_text(name, control)?,
                    control.finish((namespace, *reason), memory)?,
                ))
            }),
            control,
        )?;
        let ambiguous_qualifiers = SchemaSet::from_produced(
            self.ambiguous_qualifiers
                .iter()
                .map(|(name, ())| boxed_text(name, control)),
            control,
        )?;
        let unavailable_ambiguous_columns = SchemaSet::from_produced(
            self.unavailable_ambiguous_columns
                .iter()
                .map(|(name, ())| boxed_text(name, control)),
            control,
        )?;
        let unavailable_qualifiers = SchemaMap::from_produced(
            self.unavailable_qualifiers
                .iter()
                .map(|(name, hint)| Ok((boxed_text(name, control)?, inline(*hint, control)?))),
            control,
        )?;
        let unavailable_columns = SchemaMap::from_produced(
            self.unavailable_counts.iter().map(|(name, count)| {
                let mut entries = ProductionVec::new(*control);
                for index in 0..*count {
                    let (namespace, hint) = self
                        .unavailable_columns
                        .get(&(*name, index))
                        .expect("selected diagnostic entry");
                    let (namespace, memory) = boxed_text(namespace, control)?.into_parts();
                    entries.push_produced(control.finish((namespace, *hint), memory)?)?;
                }
                Ok((boxed_text(name, control)?, entries.finish()?))
            }),
            control,
        )?;
        let names = self.namespaces.finish()?;
        let columns_names = self.column_namespaces.finish()?;
        let header = control.reserve(size_of::<LateralRestrictions>())?;
        let (namespaces, names_memory) = names.into_parts();
        let (column_namespaces, columns_memory) = columns_names.into_parts();
        Ok(Some(Arc::new(LateralRestrictions {
            qualifiers,
            columns,
            ambiguous_qualifiers,
            namespaces,
            column_namespaces,
            unavailable_ambiguous_columns,
            unavailable_qualifiers,
            unavailable_columns,
            _memory: SchemaMemory::new(
                control.combine(header, control.combine(names_memory, columns_memory)),
            ),
        })))
    }
}
