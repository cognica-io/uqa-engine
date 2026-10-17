//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invoke a pinned range definition's selected SQL comparison and exact-key owners.

use parking_lot::Mutex;
use std::{collections::BTreeMap, sync::Arc};
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{
    catalog::{index::operator_classes, range_type::StoredRange, type_metadata},
    expr::{enums::EnumComparisonState, SQLValueCatalog},
    SQLError,
};

use crate::catalog::CatalogReadView;

/// A session retains one private subtype support call per range type. Immutable catalog readers do not own these states; rollback does not reset a previously selected enum comparison type.
#[derive(Default)]
pub struct RangeComparisonStates {
    states: Mutex<BTreeMap<u32, Arc<EnumComparisonState>>>,
}

impl RangeComparisonStates {
    fn get(&self, range_oid: u32) -> Arc<EnumComparisonState> {
        Arc::clone(self.states.lock().entry(range_oid).or_default())
    }
}

/// Compare admitted endpoints without input, domain validation or canonicalization. Both range and multirange identities select the same definition and private support state.
pub fn compare_bounds(
    view: &CatalogReadView,
    values: &dyn SQLValueCatalog,
    states: Option<&RangeComparisonStates>,
    range_oid: u32,
    bounds: (&Value, &Value),
    control: &ProductionControl<'_>,
) -> Result<std::cmp::Ordering, SQLError> {
    control.check()?;
    let definition = definition(view, range_oid)?;
    let selected = selected(definition, control)?;
    let state =
        (type_metadata::routine_pseudo_type_name(i64::from(selected.class().input_type_oid))
            == Some("anyenum"))
        .then(|| states.map(|states| states.get(definition.oid)))
        .flatten();
    uqa_sql::expr::compare_btree_inputs_with_invocation_state(
        selected,
        bounds.0,
        bounds.1,
        definition.collation_oid,
        Some(values),
        state.as_deref(),
        control,
    )
}

/// Produce the exact key under the same selected binary interpretation. Declared fixed built-in collations preserve byte equality; any future nondeterministic collation requires its own equality normalization before this boundary.
pub fn bound_equality_key(
    view: &CatalogReadView,
    values: &dyn SQLValueCatalog,
    range_oid: u32,
    value: &Value,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<u8>>, SQLError> {
    control.check()?;
    let definition = definition(view, range_oid)?;
    let selected = selected(definition, control)?;
    if definition.collation_oid != 0
        && selected.class().comparison_kind() != operator_classes::BuiltinBTreeComparison::Pattern
        && uqa_sql::catalog::collations::builtin_collation_order(definition.collation_oid).is_none()
    {
        return Err(SQLError::Internal(format!(
            "cache lookup failed for collation {}",
            definition.collation_oid
        )));
    }
    crate::distinct::btree_input_binary_key_with_catalog_and_control(
        selected,
        value,
        Some(values),
        control,
    )
}

fn definition(view: &CatalogReadView, oid: u32) -> Result<&StoredRange, SQLError> {
    view.range_definition_by_oid(oid)?
        .ok_or_else(|| SQLError::Internal(format!("cache lookup failed for range type {oid}")))
}

fn selected<'a>(
    definition: &'a StoredRange,
    control: &ProductionControl<'_>,
) -> Result<operator_classes::BoundBTreeOperatorClass<'a>, SQLError> {
    let class = operator_classes::builtin_btree_operator_class(definition.subtype_operator_class)
        .ok_or_else(|| {
        SQLError::Internal(format!(
            "cache lookup failed for operator class {}",
            definition.subtype_operator_class
        ))
    })?;
    class.bind_input(&definition.subtype, class.name, control)
}
