//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Deterministic built-in Unicode comparison owns decomposition and admitted scratch in SQL.

mod combining;
mod data;
mod elements;
mod mapping;
mod stream;

use crate::{
    catalog::collations::{builtin_collation_order, BuiltinCollationOrder},
    SQLError,
};
use elements::{
    CollationElement, CE_BUFFER_SIZE, NO_CE_PRIMARY, NO_CE_SECONDARY, NO_CE_TERTIARY, TERTIARY_MASK,
};
use icu_collator::provider::{Baked, CollationDiacriticsV1};
use icu_provider::{DataProvider, DataRequest};
use std::cmp::Ordering;
use stream::CollationElements;
use uqa_core::{
    compare_bytes_with_control,
    memory::{ProductionControl, ProductionSmallVec},
};

/// Select the deterministic string-comparison provider. Callers preserve strict NULL handling before reaching this selection, as `PostgreSQL`'s `check_collation_set` does.
pub(in crate::expr) fn string_comparison_order(
    collation: u32,
) -> Result<BuiltinCollationOrder, SQLError> {
    if collation == 0 {
        return Err(SQLError::Diagnostic {
            sqlstate: "42P22".into(),
            message: "could not determine which collation to use for string comparison".into(),
            detail: None,
            hint: Some("Use the COLLATE clause to set the collation explicitly.".into()),
        });
    }
    builtin_collation_order(collation)
        .ok_or_else(|| SQLError::Internal(format!("cache lookup failed for collation {collation}")))
}

/// Compare the deterministic `PostgreSQL` `unicode` built-in (root ICU, tertiary strength, normalization checking off). Byte-distinct strings retain distinct equality classes even when their Unicode weights match.
pub fn compare_unicode_with_control(
    left: &str,
    right: &str,
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    let byte_order = compare_bytes_with_control(left.as_bytes(), right.as_bytes(), control)?;
    if byte_order.is_eq() {
        return Ok(byte_order);
    }
    let diacritics =
        <Baked as DataProvider<CollationDiacriticsV1>>::load(&Baked, DataRequest::default())
            .map_err(|error| {
                SQLError::Internal(format!("built-in Unicode collation data: {error}"))
            })?
            .payload;
    let root = Baked::SINGLETON_COLLATION_ROOT_V1;
    let jamo = Baked::SINGLETON_COLLATION_JAMO_V1
        .ce32s
        .as_ule_slice()
        .try_into()
        .map_err(|_| SQLError::Internal("built-in Unicode Jamo width".into()))?;
    let decompositions = icu_normalizer::provider::Baked::SINGLETON_NORMALIZER_NFD_DATA_V1;
    let tables = icu_normalizer::provider::Baked::SINGLETON_NORMALIZER_NFD_TABLES_V1;
    let mut left = CollationElements::new(
        left.chars(),
        *control,
        root,
        jamo,
        &diacritics.get().secondaries,
        decompositions,
        tables,
    );
    let mut right = CollationElements::new(
        right.chars(),
        *control,
        root,
        jamo,
        &diacritics.get().secondaries,
        decompositions,
        tables,
    );
    left.init()?;
    right.init()?;
    let mut left_ces = ProductionSmallVec::<[CollationElement; CE_BUFFER_SIZE]>::new(*control);
    let mut right_ces = ProductionSmallVec::<[CollationElement; CE_BUFFER_SIZE]>::new(*control);
    loop {
        control.check()?;
        let left_primary = loop {
            let ce = left.next()?;
            left_ces.push(ce)?;
            if ce.primary() != 0 {
                break ce.primary();
            }
        };
        let right_primary = loop {
            let ce = right.next()?;
            right_ces.push(ce)?;
            if ce.primary() != 0 {
                break ce.primary();
            }
        };
        let order = left_primary.cmp(&right_primary);
        if !order.is_eq() {
            return Ok(order);
        }
        if left_primary == NO_CE_PRIMARY {
            break;
        }
    }
    let order = compare_secondary_and_tertiary(&left_ces, &right_ces, control)?;
    control.check()?;
    Ok(order.then(byte_order))
}

fn compare_secondary_and_tertiary(
    left_ces: &[CollationElement],
    right_ces: &[CollationElement],
    control: &ProductionControl<'_>,
) -> Result<Ordering, SQLError> {
    for (weight, sentinel) in [
        (
            CollationElement::secondary as fn(CollationElement) -> u16,
            NO_CE_SECONDARY,
        ),
        (tertiary, NO_CE_TERTIARY),
    ] {
        let mut left = left_ces.iter();
        let mut right = right_ces.iter();
        loop {
            control.check()?;
            let l = loop {
                control.check()?;
                let value = weight(*left.next().unwrap_or_default());
                if value != 0 {
                    break value;
                }
            };
            let r = loop {
                control.check()?;
                let value = weight(*right.next().unwrap_or_default());
                if value != 0 {
                    break value;
                }
            };
            let order = l.cmp(&r);
            if !order.is_eq() {
                return Ok(order);
            }
            if l == sentinel {
                break;
            }
        }
    }
    control.check()?;
    Ok(Ordering::Equal)
}

fn tertiary(ce: CollationElement) -> u16 {
    ce.non_primary().tertiary_case_quarternary(TERTIARY_MASK)
}

#[cfg(test)]
mod tests;
