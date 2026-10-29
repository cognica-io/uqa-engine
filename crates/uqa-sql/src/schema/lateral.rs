//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM namespaces which are forbidden or invisible at a particular binding boundary.

use super::{RowSchema, SchemaMap, SchemaSet};
use crate::SQLError;
use std::sync::Arc;

mod columns;
mod production;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Denial {
    Join,
    MutationTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiagnosticNamespace {
    name: Box<str>,
    columns: Vec<Box<str>>,
    _memory: super::production::lease::SchemaMemory,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LateralRestrictions {
    qualifiers: SchemaMap<Box<str>, Denial>,
    columns: SchemaMap<Box<str>, (Box<str>, Denial)>,
    ambiguous_qualifiers: SchemaSet<Box<str>>,
    namespaces: Vec<Arc<DiagnosticNamespace>>,
    column_namespaces: Vec<Arc<DiagnosticNamespace>>,
    unavailable_ambiguous_columns: SchemaSet<Box<str>>,
    unavailable_qualifiers: SchemaMap<Box<str>, bool>,
    unavailable_columns: SchemaMap<Box<str>, Vec<(Box<str>, bool)>>,
    _memory: super::production::lease::SchemaMemory,
}

impl RowSchema {
    pub(super) fn lateral_qualifier_is_ambiguous(&self, qualifier: &str) -> bool {
        self.index
            .cold
            .lateral_restrictions
            .as_ref()
            .is_some_and(|r| r.ambiguous_qualifiers.contains(qualifier))
    }

    pub(super) fn joined_diagnostic_names(
        left: &Self,
        right: &Self,
    ) -> Option<Arc<LateralRestrictions>> {
        let mut restrictions = LateralRestrictions::default();
        for input in [left, right] {
            if let Some(input) = input.index.cold.lateral_restrictions.as_ref() {
                restrictions
                    .namespaces
                    .extend(input.namespaces.iter().cloned());
                restrictions
                    .column_namespaces
                    .extend(input.column_namespaces.iter().cloned());
            }
        }
        (!restrictions.namespaces.is_empty() || !restrictions.column_namespaces.is_empty())
            .then(|| Arc::new(restrictions))
    }

    pub(super) fn requalified_column_names(input: &Self) -> Option<Arc<LateralRestrictions>> {
        let column_namespaces = input
            .index
            .cold
            .lateral_restrictions
            .as_ref()?
            .column_namespaces
            .clone();
        (!column_namespaces.is_empty()).then(|| {
            Arc::new(LateralRestrictions {
                column_namespaces,
                ..LateralRestrictions::default()
            })
        })
    }

    pub(crate) fn with_column_diagnostic_names_from(mut self, input: &Self) -> Self {
        Arc::make_mut(&mut self.index).cold.lateral_restrictions =
            Self::requalified_column_names(input);
        self
    }

    /// Retain the left namespace while rejecting its use by the right input of a RIGHT or FULL join. The namespace must remain present so even an unknown qualified column reports the forbidden relation first.
    pub(crate) fn without_lateral_access(input: &Self, namespace: &str) -> Self {
        Self::with_lateral_denial(input, namespace, Denial::Join)
    }

    pub(crate) fn without_target_lateral_access(input: &Self, namespace: &str) -> Self {
        Self::with_lateral_denial(input, namespace, Denial::MutationTarget)
    }

    fn with_lateral_denial(input: &Self, namespace: &str, reason: Denial) -> Self {
        let mut restrictions = input.restrictions();
        for qualifier in input.relation_qualifiers() {
            restrictions.qualifiers.insert(qualifier.into(), reason);
        }
        for identity in input
            .identities()
            .iter()
            .chain(input.index.aliases.keys())
            .chain(input.index.cold.binding_only.keys())
        {
            restrictions
                .columns
                .insert(identity.column().into(), (namespace.into(), reason));
        }
        input.with_restrictions(restrictions)
    }

    /// Combine namespaces at the same query level. A preceding FROM item does not become a shadowable lexical ancestor merely because the next item contains a join.
    pub(crate) fn with_peer_scope(left: &Self, right: &Self) -> Self {
        let mut restrictions = Self::merged_restrictions(left, right, false);
        for qualifier in left.relation_qualifiers() {
            if right.has_qualifier(qualifier) {
                restrictions.ambiguous_qualifiers.insert(qualifier.into());
            }
        }
        Self::join(left, right, std::iter::empty::<String>()).with_restrictions(restrictions)
    }

    pub(crate) fn with_source_diagnostic_namespace(input: &Self, namespace: &str) -> Self {
        Self::with_named_diagnostic_namespace(input, namespace, false)
    }

    pub(crate) fn with_join_diagnostic_namespace(input: &Self, namespace: &str) -> Self {
        Self::with_named_diagnostic_namespace(input, namespace, true)
    }

    fn with_named_diagnostic_namespace(input: &Self, namespace: &str, joined: bool) -> Self {
        let mut restrictions = input.restrictions();
        restrictions.namespaces = vec![Arc::new(DiagnosticNamespace {
            name: namespace.into(),
            columns: input
                .identities()
                .iter()
                .map(|identity| identity.column().into())
                .collect(),
            _memory: crate::schema::production::lease::SchemaMemory::default(),
        })];
        // Missing-column hints search range entries, excluding the synthetic join entry. A derived relation starts its own range entry instead of exposing its body's namespaces.
        if !joined {
            restrictions
                .column_namespaces
                .clone_from(&restrictions.namespaces);
        }
        input.with_restrictions(restrictions)
    }

    /// Keep unavailable names only for diagnostics; they never gain value slots, wildcard columns or successful lookup identities.
    pub(crate) fn with_unavailable_names(
        input: &Self,
        unavailable: &Self,
        lateral_hint: bool,
    ) -> Self {
        let mut restrictions = input.restrictions();
        let denied = unavailable.index.cold.lateral_restrictions.as_deref();
        for qualifier in unavailable.relation_qualifiers() {
            let target = denied
                .is_some_and(|r| r.qualifiers.get(qualifier) == Some(&Denial::MutationTarget));
            restrictions
                .unavailable_qualifiers
                .insert(qualifier.into(), lateral_hint && !target);
        }
        if let Some(denied) = denied {
            for namespace in &denied.namespaces {
                let target =
                    denied.qualifiers.get(&namespace.name) == Some(&Denial::MutationTarget);
                let mut seen = SchemaSet::default();
                for column in &namespace.columns {
                    if !seen.insert(column.clone()) {
                        restrictions
                            .unavailable_ambiguous_columns
                            .insert(column.clone());
                        continue;
                    }
                    restrictions
                        .unavailable_columns
                        .entry(column.clone())
                        .or_default()
                        .push((namespace.name.clone(), lateral_hint && !target));
                }
            }
        }
        input.with_restrictions(restrictions)
    }

    pub(crate) fn require_lateral_qualifier(&self, qualifier: &str) -> Result<(), SQLError> {
        if let Some(restrictions) = self.index.cold.lateral_restrictions.as_ref() {
            if restrictions.ambiguous_qualifiers.contains(qualifier) {
                return Err(SQLError::Routine {
                    sqlstate: "42P09".into(),
                    message: format!("table reference \"{qualifier}\" is ambiguous"),
                });
            }
            if let Some(reason) = restrictions.qualifiers.get(qualifier) {
                return Err(forbidden_reference(qualifier, *reason));
            }
        }
        Ok(())
    }

    pub(crate) fn require_lateral_column(&self, column: &str) -> Result<(), SQLError> {
        if let Some(restrictions) = self.index.cold.lateral_restrictions.as_ref() {
            if let Some((qualifier, reason)) = restrictions.columns.get(column) {
                return Err(forbidden_reference(qualifier, *reason));
            }
            if !self.has_unqualified_column(column) {
                self.require_lateral_qualifier(column)?;
            }
        }
        Ok(())
    }

    pub(crate) fn unavailable_qualifier_error(&self, qualifier: &str) -> Option<SQLError> {
        let lateral = *self
            .index
            .cold
            .lateral_restrictions
            .as_ref()?
            .unavailable_qualifiers
            .get(qualifier)?;
        Some(SQLError::Diagnostic {
            sqlstate: "42P01".into(),
            message: format!("invalid reference to FROM-clause entry for table \"{qualifier}\""),
            detail: Some(unavailable_entry(qualifier)),
            hint: lateral.then(|| {
                "To reference that table, you must mark this subquery with LATERAL.".into()
            }),
        })
    }

    pub(crate) fn unavailable_column_error(&self, column: &str) -> Option<SQLError> {
        if self
            .index
            .cold
            .lateral_restrictions
            .as_ref()?
            .unavailable_ambiguous_columns
            .contains(column)
        {
            return Some(SQLError::AmbiguousColumn(column.to_owned()));
        }
        let names = self
            .index
            .cold
            .lateral_restrictions
            .as_ref()?
            .unavailable_columns
            .get(column)?;
        let (detail, hint) = match names.as_slice() {
            [(qualifier, lateral)] => (
                format!(
                    "There is a column named \"{column}\" in table \"{qualifier}\", but it cannot be referenced from this part of the query."
                ),
                lateral.then(|| {
                    "To reference that column, you must mark this subquery with LATERAL.".into()
                }),
            ),
            [_, _, ..] => (
                format!(
                    "There are columns named \"{column}\", but they are in tables that cannot be referenced from this part of the query."
                ),
                Some("Try using a table-qualified name.".into()),
            ),
            [] => return None,
        };
        Some(SQLError::Diagnostic {
            sqlstate: "42703".into(),
            message: format!("column \"{column}\" does not exist"),
            detail: Some(detail),
            hint,
        })
    }

    fn restrictions(&self) -> LateralRestrictions {
        self.index
            .cold
            .lateral_restrictions
            .as_deref()
            .cloned()
            .unwrap_or_default()
    }

    fn with_restrictions(&self, restrictions: LateralRestrictions) -> Self {
        if restrictions == LateralRestrictions::default() {
            return self.clone();
        }
        let mut schema = self.clone();
        Arc::make_mut(&mut schema.index).cold.lateral_restrictions = Some(Arc::new(restrictions));
        schema
    }

    fn merged_restrictions(input: &Self, outer: &Self, shadows: bool) -> LateralRestrictions {
        let mut restrictions = input.restrictions();
        if restrictions.column_namespaces.is_empty() {
            restrictions.column_namespaces = columns::namespaces(input);
        }
        for namespace in columns::namespaces(outer) {
            if !restrictions
                .column_namespaces
                .iter()
                .any(|current| Arc::ptr_eq(current, &namespace))
            {
                restrictions.column_namespaces.push(namespace);
            }
        }
        if let Some(outer) = outer.index.cold.lateral_restrictions.as_deref() {
            restrictions.qualifiers.extend(
                outer
                    .qualifiers
                    .iter()
                    .filter(|(name, _)| !shadows || !input.has_qualifier(name))
                    .map(|(name, reason)| (name.clone(), *reason)),
            );
            restrictions.columns.extend(
                outer
                    .columns
                    .iter()
                    .filter(|(name, _)| {
                        !shadows
                            || (!input.has_unqualified_column(name)
                                && !input.column_is_ambiguous(name)
                                && !input.has_qualifier(name))
                    })
                    .map(|(name, value)| (name.clone(), value.clone())),
            );
            restrictions.ambiguous_qualifiers.extend(
                outer
                    .ambiguous_qualifiers
                    .iter()
                    .filter(|name| !shadows || !input.has_qualifier(name))
                    .cloned(),
            );
            if !shadows {
                restrictions
                    .namespaces
                    .extend(outer.namespaces.iter().cloned());
            }
            restrictions
                .unavailable_ambiguous_columns
                .extend(outer.unavailable_ambiguous_columns.iter().cloned());
            for (name, lateral) in &outer.unavailable_qualifiers {
                restrictions
                    .unavailable_qualifiers
                    .entry(name.clone())
                    .or_insert(*lateral);
            }
            for (name, values) in &outer.unavailable_columns {
                restrictions
                    .unavailable_columns
                    .entry(name.clone())
                    .or_insert_with(|| values.clone());
            }
        }
        restrictions
    }

    pub(super) fn outer_lateral_restrictions(
        input: &Self,
        outer: &Self,
    ) -> Option<Arc<LateralRestrictions>> {
        if outer.index.cold.lateral_restrictions.is_none()
            && outer.relation_qualifiers().next().is_none()
        {
            return input.index.cold.lateral_restrictions.clone();
        }
        let restrictions = Self::merged_restrictions(input, outer, true);
        (restrictions != LateralRestrictions::default()).then(|| Arc::new(restrictions))
    }
}

fn unavailable_entry(qualifier: &str) -> String {
    format!(
        "There is an entry for table \"{qualifier}\", but it cannot be referenced from this part of the query."
    )
}

fn forbidden_reference(qualifier: &str, reason: Denial) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P10".into(),
        message: format!("invalid reference to FROM-clause entry for table \"{qualifier}\""),
        detail: (reason == Denial::Join).then(|| {
            "The combining JOIN type must be INNER or LEFT for a LATERAL reference.".into()
        }),
        hint: (reason == Denial::MutationTarget).then(|| unavailable_entry(qualifier)),
    }
}
