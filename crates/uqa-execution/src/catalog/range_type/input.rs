//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Invoke selected range input and canonical support while SQL owns syntax, normalization and physical production.

use super::support::{compare_bounds, RangeComparisonStates};
use crate::catalog::CatalogReadView;
use uqa_core::{
    memory::{Produced, ProductionControl},
    Value,
};
use uqa_sql::{
    catalog::range_type::StoredRange,
    expr::{
        cast_value_with_type_resolution_with_control, declared_multirange_datum_with_control,
        declared_range_datum_with_control, parse_multirange_with_operations,
        parse_range_with_operations, CanonicalRange, EngineHook, RangeComparisonValue,
        RangeConstruction, RangeValueDescriptor,
    },
    SQLError,
};

mod construction;
pub use construction::{construct_multirange, construct_range};

/// Catalog support calls retain their selected routine identity and execute as callbacks, without another name lookup or EXECUTE privilege check.
pub trait RangeCanonicalInvocation {
    fn invoke_canonical(
        &self,
        catalog: &CatalogReadView,
        function_oid: u32,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError>;
}

pub struct RangeInputContext<'a> {
    pub catalog: &'a CatalogReadView,
    pub expressions: &'a dyn EngineHook,
    pub canonical: &'a dyn RangeCanonicalInvocation,
    pub states: Option<&'a RangeComparisonStates>,
}

/// Decode text through the exact retained definition. Endpoint input and canonical effects occur only in SQL's constructor; encoding never repeats them.
pub fn read_input(
    context: &RangeInputContext<'_>,
    oid: u32,
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<Value>, SQLError> {
    control.check()?;
    let (definition, descriptor) = selected(context, oid)?;
    let operations = SelectedInput {
        context,
        definition,
        descriptor: &descriptor,
    };
    if oid == definition.oid {
        let range = parse_range_with_operations(text, &operations, control)?;
        declared_range_datum_with_control(
            &range,
            &descriptor,
            context.expressions.composite_types(),
            control,
        )
    } else {
        let ranges = parse_multirange_with_operations(text, &operations, control)?;
        declared_multirange_datum_with_control(
            &ranges,
            &descriptor,
            context.expressions.composite_types(),
            control,
        )
    }
}

fn selected<'a>(
    context: &'a RangeInputContext<'_>,
    oid: u32,
) -> Result<(&'a StoredRange, std::sync::Arc<RangeValueDescriptor>), SQLError> {
    let missing = || SQLError::Internal(format!("cache lookup failed for range type {oid}"));
    let definition = context
        .catalog
        .range_definition_by_oid(oid)?
        .ok_or_else(missing)?;
    let descriptor = context
        .catalog
        .range_value_descriptor(oid)?
        .ok_or_else(missing)?;
    Ok((definition, descriptor))
}

struct SelectedInput<'a> {
    context: &'a RangeInputContext<'a>,
    definition: &'a StoredRange,
    descriptor: &'a RangeValueDescriptor,
}

impl RangeConstruction for SelectedInput<'_> {
    type Identity = u32;

    fn identity(&self) -> u32 {
        self.definition.oid
    }

    fn has_canonical_function(&self) -> bool {
        self.definition.canonical_oid.is_some()
    }

    fn input_bound(
        &self,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Value>, SQLError> {
        let target = self.definition.subtype.catalog_name_with_control(control)?;
        cast_value_with_type_resolution_with_control(
            value,
            None,
            &target,
            Some(self.context.expressions),
            control,
        )
    }

    fn compare_bounds(
        &self,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<std::cmp::Ordering, SQLError> {
        let values = self.context.expressions.enum_labels().ok_or_else(|| {
            SQLError::Internal("range input requires its selected value catalog".into())
        })?;
        compare_bounds(
            self.context.catalog,
            values,
            self.context.states,
            self.definition.oid,
            (left, right),
            control,
        )
    }

    fn canonicalize(
        &self,
        range: &CanonicalRange<u32>,
        control: &ProductionControl<'_>,
    ) -> Result<Option<Produced<CanonicalRange<u32>>>, SQLError> {
        let Some(oid) = self.definition.canonical_oid else {
            return Ok(None);
        };
        let argument = declared_range_datum_with_control(
            range,
            self.descriptor,
            self.context.expressions.composite_types(),
            control,
        )?;
        let result = self.context.canonical.invoke_canonical(
            self.context.catalog,
            oid,
            &argument,
            control,
        )?;
        control.check()?;
        if matches!(*result, Value::Null) {
            return Err(SQLError::Routine {
                sqlstate: "XX000".into(),
                message: format!("function {oid} returned NULL"),
            });
        }
        match RangeComparisonValue::from_declared_with_control(
            &result,
            self.definition.oid,
            self.context.expressions.enum_labels(),
            control,
        )? {
            Some(RangeComparisonValue::DeclaredRange(range)) => Ok(Some(range)),
            _ => Err(SQLError::Internal(
                "range canonical function returned a non-range value".into(),
            )),
        }
    }
}
