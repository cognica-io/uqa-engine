//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{
    boxed_text, inline, namespaces, Arc, Denial, LateralRestrictions, ProductionControl,
    ProductionMap, ProductionVec, RowSchema, SQLError, Selection,
};

impl RowSchema {
    pub(in crate::schema) fn joined_diagnostic_names_with_control(
        left: &Self,
        right: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Arc<LateralRestrictions>>, SQLError> {
        let mut selected = Selection::empty(control);
        for input in [left, right] {
            if let Some(input) = &input.index.cold.lateral_restrictions {
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
        }
        selected.finish(control)
    }
    pub(in crate::schema) fn requalified_column_names_with_control(
        input: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Arc<LateralRestrictions>>, SQLError> {
        let mut selected = Selection::empty(control);
        if let Some(input) = &input.index.cold.lateral_restrictions {
            for namespace in &input.column_namespaces {
                selected
                    .column_namespaces
                    .push_produced(inline(namespace.clone(), control)?)?;
            }
        }
        selected.finish(control)
    }
    pub(in crate::schema) fn outer_lateral_restrictions_with_control(
        input: &Self,
        outer: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Arc<LateralRestrictions>>, SQLError> {
        control.check()?;
        if outer.index.cold.lateral_restrictions.is_none()
            && outer.relation_qualifiers().next().is_none()
        {
            return Ok(input.index.cold.lateral_restrictions.clone());
        }
        Selection::merge(input, outer, true, control)?.finish(control)
    }
    pub(crate) fn without_lateral_access_with_control(
        input: &Self,
        namespace: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::with_lateral_denial_with_control(input, namespace, Denial::Join, control)
    }
    pub(crate) fn without_target_lateral_access_with_control(
        input: &Self,
        namespace: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::with_lateral_denial_with_control(input, namespace, Denial::MutationTarget, control)
    }
    fn with_lateral_denial_with_control(
        input: &Self,
        namespace: &str,
        reason: Denial,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = Selection::new(input, control)?;
        for qualifier in input.relation_qualifiers() {
            selected.qualifiers.insert_copy(qualifier, reason)?;
        }
        for identity in input
            .identities()
            .iter()
            .chain(input.index.aliases.keys())
            .chain(input.index.cold.binding_only.keys())
        {
            selected
                .columns
                .insert_copy(identity.column(), (namespace, reason))?;
        }
        input.with_restrictions_with_control(selected.finish(control)?, control)
    }
    pub(crate) fn with_peer_scope_with_control(
        left: &Self,
        right: &Self,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = Selection::merge(left, right, false, control)?;
        for qualifier in left.relation_qualifiers() {
            control.check()?;
            if right.has_qualifier(qualifier) {
                selected.ambiguous_qualifiers.insert_copy(qualifier, ())?;
            }
        }
        let restrictions = selected.finish(control)?;
        Self::join_with_control(left, right, [], control)?
            .with_restrictions_with_control(restrictions, control)
    }
    pub(crate) fn with_unavailable_names_with_control(
        input: &Self,
        unavailable: &Self,
        lateral_hint: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = Selection::new(input, control)?;
        let denied = unavailable.index.cold.lateral_restrictions.as_deref();
        for qualifier in unavailable.relation_qualifiers() {
            let target = denied.is_some_and(|denied| {
                denied.qualifiers.get(qualifier) == Some(&Denial::MutationTarget)
            });
            selected
                .unavailable_qualifiers
                .insert_copy(qualifier, lateral_hint && !target)?;
        }
        if let Some(denied) = denied {
            for namespace in &denied.namespaces {
                let target =
                    denied.qualifiers.get(&namespace.name) == Some(&Denial::MutationTarget);
                let mut seen = ProductionMap::new(*control);
                for column in &namespace.columns {
                    if seen.insert_copy(column.as_ref(), ())?.is_some() {
                        selected
                            .unavailable_ambiguous_columns
                            .insert_copy(column.as_ref(), ())?;
                    } else {
                        selected.append_unavailable(
                            column,
                            &namespace.name,
                            lateral_hint && !target,
                        )?;
                    }
                }
            }
        }
        input.with_restrictions_with_control(selected.finish(control)?, control)
    }
    pub(crate) fn with_source_diagnostic_namespace_with_control(
        input: &Self,
        namespace: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::with_named_diagnostic_namespace_with_control(input, namespace, false, control)
    }
    pub(crate) fn with_join_diagnostic_namespace_with_control(
        input: &Self,
        namespace: &str,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        Self::with_named_diagnostic_namespace_with_control(input, namespace, true, control)
    }
    fn with_named_diagnostic_namespace_with_control(
        input: &Self,
        namespace: &str,
        joined: bool,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        let mut selected = Selection::new(input, control)?;
        let mut columns = ProductionVec::new(*control);
        for identity in input.identities() {
            columns.push_produced(boxed_text(identity.column(), control)?)?;
        }
        let namespace = namespaces::produce(namespace, columns, control)?;
        selected.namespaces = ProductionVec::new(*control);
        selected
            .namespaces
            .push_produced(inline(namespace.clone(), control)?)?;
        if !joined {
            selected.column_namespaces = ProductionVec::new(*control);
            selected
                .column_namespaces
                .push_produced(inline(namespace, control)?)?;
        }
        input.with_restrictions_with_control(selected.finish(control)?, control)
    }
    fn with_restrictions_with_control(
        &self,
        restrictions: Option<Arc<LateralRestrictions>>,
        control: &ProductionControl<'_>,
    ) -> Result<Self, SQLError> {
        if restrictions.is_none() {
            control.check()?;
            return Ok(self.clone());
        }
        let mut metadata = crate::schema::production::copy::metadata(self, control)?;
        metadata.lateral_restrictions = restrictions;
        crate::schema::production::copy::with_metadata(self, metadata, control)
    }
}
