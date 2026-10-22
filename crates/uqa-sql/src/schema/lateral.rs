//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! FROM namespaces which exist during binding but cannot supply a LATERAL input.

use super::{RowSchema, SchemaMap, SchemaSet};
use crate::SQLError;
use std::sync::Arc;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LateralRestrictions {
    qualifiers: SchemaSet<Box<str>>,
    columns: SchemaMap<Box<str>, Box<str>>,
}

impl RowSchema {
    /// Retain the left namespace while rejecting its use by the right input of a RIGHT or FULL join. The namespace must remain present so even an unknown qualified column reports the forbidden relation first.
    pub(crate) fn without_lateral_access(input: &Self, namespace: &str) -> Self {
        let mut restrictions = input
            .index
            .cold
            .lateral_restrictions
            .as_deref()
            .cloned()
            .unwrap_or_default();
        restrictions
            .qualifiers
            .extend(input.relation_qualifiers().map(Box::<str>::from));
        for identity in input
            .identities()
            .iter()
            .chain(input.index.aliases.keys())
            .chain(input.index.cold.binding_only.keys())
        {
            restrictions
                .columns
                .insert(identity.column().into(), namespace.into());
        }
        let mut schema = input.clone();
        Arc::make_mut(&mut schema.index).cold.lateral_restrictions = Some(Arc::new(restrictions));
        schema
    }

    pub(crate) fn require_lateral_qualifier(&self, qualifier: &str) -> Result<(), SQLError> {
        if self
            .index
            .cold
            .lateral_restrictions
            .as_ref()
            .is_some_and(|restriction| restriction.qualifiers.contains(qualifier))
        {
            return Err(forbidden_reference(qualifier));
        }
        Ok(())
    }

    pub(crate) fn require_lateral_column(&self, column: &str) -> Result<(), SQLError> {
        if let Some(restrictions) = self.index.cold.lateral_restrictions.as_ref() {
            if let Some(qualifier) = restrictions.columns.get(column) {
                return Err(forbidden_reference(qualifier));
            }
            if restrictions.qualifiers.contains(column) {
                return Err(forbidden_reference(column));
            }
        }
        Ok(())
    }

    pub(super) fn outer_lateral_restrictions(
        input: &Self,
        outer: &Self,
    ) -> Option<Arc<LateralRestrictions>> {
        let Some(outer) = outer.index.cold.lateral_restrictions.as_ref() else {
            return input.index.cold.lateral_restrictions.clone();
        };
        let mut restrictions = input
            .index
            .cold
            .lateral_restrictions
            .as_deref()
            .cloned()
            .unwrap_or_default();
        restrictions.qualifiers.extend(
            outer
                .qualifiers
                .iter()
                .filter(|name| !input.has_qualifier(name))
                .cloned(),
        );
        restrictions.columns.extend(
            outer
                .columns
                .iter()
                .filter(|(name, _)| {
                    !input.has_unqualified_column(name)
                        && !input.column_is_ambiguous(name)
                        && !input.has_qualifier(name)
                })
                .map(|(name, qualifier)| (name.clone(), qualifier.clone())),
        );
        (!restrictions.qualifiers.is_empty() || !restrictions.columns.is_empty())
            .then(|| Arc::new(restrictions))
    }
}

fn forbidden_reference(qualifier: &str) -> SQLError {
    SQLError::Diagnostic {
        sqlstate: "42P10".into(),
        message: format!("invalid reference to FROM-clause entry for table \"{qualifier}\""),
        detail: Some(
            "The combining JOIN type must be INNER or LEFT for a LATERAL reference.".into(),
        ),
        hint: None,
    }
}
