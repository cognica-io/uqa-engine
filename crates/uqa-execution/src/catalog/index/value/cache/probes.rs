//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Retained probes use the resident accelerator's eligibility and fallible SQL comparison rules.

use super::{comparison, storage, ValueIndexCache};
use std::ops::Bound::{self, Excluded, Included, Unbounded};
use uqa_core::{DocId, PostingList, Predicate, Value};
use uqa_sql::{expr::SQLValueCatalog, SQLError};
use uqa_storage::read_control::StorageReadControl;

impl ValueIndexCache {
    pub fn visit_key_candidates_with_catalog(
        &self,
        predicate: &Predicate,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        if !matches!(predicate, Predicate::Equals(_) | Predicate::IsNull) {
            return Ok(false);
        }
        self.visit(predicate, catalog, control, visit)
    }

    fn visit(
        &self,
        predicate: &Predicate,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &StorageReadControl,
        visit: &mut dyn FnMut(DocId) -> Result<(), SQLError>,
    ) -> Result<bool, SQLError> {
        if self.is_carried() {
            return Ok(false);
        }
        if comparison::needs_sql_comparison(predicate, self.policy.comparison.can_fail()) {
            if let Some(values) = &self.retained {
                let mut failure = None;
                let result = values.visit_values(control, &mut |id, value| {
                    let next = comparison::matches(&value, predicate, catalog)
                        .and_then(|matched| if matched { visit(id) } else { Ok(()) });
                    match next {
                        Ok(()) => Ok(true),
                        Err(error) => {
                            failure = Some(error);
                            Ok(false)
                        }
                    }
                });
                if let Some(error) = failure {
                    return Err(error);
                }
                result.map_err(storage)?;
            } else {
                for (&id, value) in &self.policy.values {
                    if comparison::matches(value, predicate, catalog)? {
                        visit(id)?;
                    }
                }
            }
            return Ok(true);
        }
        if !self.supports(predicate) {
            return Ok(false);
        }
        let Some(values) = &self.retained else {
            if matches!(predicate, Predicate::Equals(_) | Predicate::IsNull) {
                return self
                    .policy
                    .visit_key_candidates_with_catalog(predicate, catalog, visit);
            }
            for entry in self.policy.scan(predicate).expect("supported").entries() {
                visit(entry.doc_id)?;
            }
            return Ok(true);
        };
        ranges(predicate, &mut |low, high| {
            let mut failure = None;
            let result = values.visit_range(low, high, control, &mut |id| match visit(id) {
                Ok(()) => Ok(true),
                Err(error) => {
                    failure = Some(error);
                    Ok(false)
                }
            });
            failure.map_or_else(|| result.map_err(storage), Err)
        })?;
        Ok(true)
    }

    pub fn scan_observing_with_catalog(
        &self,
        predicate: &Predicate,
        catalog: Option<&dyn SQLValueCatalog>,
        control: &StorageReadControl,
        observe: impl FnOnce() -> Result<(), SQLError>,
    ) -> Result<Option<PostingList>, SQLError> {
        if self.retained.is_none() {
            return self
                .policy
                .scan_observing_with_catalog(predicate, catalog, observe);
        }
        if self.is_carried()
            || (!self.supports(predicate)
                && !comparison::needs_sql_comparison(predicate, self.policy.comparison.can_fail()))
        {
            return Ok(None);
        }
        observe()?;
        let mut ids = Vec::new();
        self.visit(predicate, catalog, control, &mut |id| {
            ids.push(id);
            Ok(())
        })?;
        ids.sort_unstable();
        ids.dedup();
        Ok(Some(super::super::posting_list_from_sorted_ids(
            ids.into_iter(),
        )))
    }

    pub fn field_candidates(
        &self,
        value: &Value,
        control: &StorageReadControl,
    ) -> Result<Option<PostingList>, SQLError> {
        if !super::super::field_is_index_safe(value) {
            return Ok(None);
        }
        let predicate = if matches!(value, Value::Null) {
            Predicate::IsNull
        } else {
            if self.policy.comparison.field_needs_evaluation() {
                return Ok(None);
            }
            Predicate::Equals(value.clone())
        };
        self.scan_observing_with_catalog(&predicate, None, control, || Ok(()))
    }

    pub fn estimate_cardinality(
        &self,
        predicate: &Predicate,
        control: &StorageReadControl,
    ) -> Result<Option<usize>, SQLError> {
        if !self.supports(predicate) {
            return Ok(None);
        }
        let Some(values) = &self.retained else {
            return Ok(self.policy.estimate_cardinality(predicate));
        };
        let mut count = 0_usize;
        ranges(predicate, &mut |low, high| {
            count = count.saturating_add(values.count_range(low, high, control).map_err(storage)?);
            Ok(())
        })?;
        Ok(Some(count))
    }
}

type RangeVisitor<'a> = dyn FnMut(Bound<&Value>, Bound<&Value>) -> Result<(), SQLError> + 'a;

fn ranges(predicate: &Predicate, visit: &mut RangeVisitor<'_>) -> Result<(), SQLError> {
    let non_null = Excluded(&Value::Null);
    match predicate {
        Predicate::Equals(Value::Null)
        | Predicate::LessThan(Value::Null)
        | Predicate::LessThanOrEqual(Value::Null) => Ok(()),
        Predicate::Equals(value) => visit(Included(value), Included(value)),
        Predicate::IsNull => visit(Included(&Value::Null), Included(&Value::Null)),
        Predicate::IsNotNull | Predicate::GreaterThanOrEqual(Value::Null) => {
            visit(non_null, Unbounded)
        }
        Predicate::GreaterThan(value) => visit(Excluded(value), Unbounded),
        Predicate::GreaterThanOrEqual(value) => visit(Included(value), Unbounded),
        Predicate::LessThan(value) => visit(non_null, Excluded(value)),
        Predicate::LessThanOrEqual(value) => visit(non_null, Included(value)),
        Predicate::Between { low, high } if low > high => Ok(()),
        Predicate::Between { low, high } => visit(
            if matches!(low, Value::Null) {
                non_null
            } else {
                Included(low)
            },
            Included(high),
        ),
        Predicate::InSet(values) => {
            // InSet already uses Core-ordered distinct values; posting results restore document order at their existing output boundary.
            for value in values {
                if !matches!(value, Value::Null) {
                    visit(Included(value), Included(value))?;
                }
            }
            Ok(())
        }
        Predicate::NotEquals(_) => unreachable!("unsupported complement predicate"),
    }
}
