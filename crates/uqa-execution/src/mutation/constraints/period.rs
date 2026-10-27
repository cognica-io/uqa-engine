//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scan referenced temporal keys and prove coverage with bounded range and identity owners.

use super::{
    dml_storage_error, missing_document_error, ConstraintContext, Document, ForeignKey,
    PhysicalDocumentIdentity, SQLError, Value,
};
use crate::mutation::row_membership::PhysicalRowLookup as DeletedRowLookup;
use uqa_sql::{expr::CanonicalRange, semantics::period::period_ranges};

mod parents;
mod ranges;
use parents::ParentIds;
pub(crate) use parents::PeriodParentIds;
use ranges::{ParentRanges, RangeObservation};

struct PeriodParents {
    observation: RangeObservation,
    ids: Option<PeriodParentIds>,
}

/// Compatibility collection for callers that explicitly request owned parent identities. Engine constraint paths use bounded replay or coverage-only observation.
pub fn period_foreign_key_coverage(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<(bool, Vec<PhysicalDocumentIdentity>), SQLError> {
    let parents = period_parents(
        context,
        foreign_key,
        local_values,
        &|table, id| {
            Ok(excluded_parents
                .iter()
                .any(|row| row.table == table && row.doc_id == id))
        },
        replacement_parent,
        true,
    )?;
    let Some(parents) = parents else {
        return Ok((false, Vec::new()));
    };
    let ids = parents
        .ids
        .map(|ids| ids.read()?.collect())
        .transpose()?
        .unwrap_or_default();
    Ok((parents.observation.covered, ids))
}

pub(crate) fn period_foreign_key_parent_rows(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
) -> Result<(bool, Option<PeriodParentIds>), SQLError> {
    let parents = period_parents(
        context,
        foreign_key,
        local_values,
        &|_, _| Ok(false),
        None,
        true,
    )?;
    Ok(parents.map_or((false, None), |parents| {
        (parents.observation.covered, parents.ids)
    }))
}

pub(crate) fn period_foreign_key_is_covered(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<bool, SQLError> {
    period_foreign_key_coverage_excluding(
        context,
        foreign_key,
        local_values,
        &|table, id| {
            Ok(excluded_parents
                .iter()
                .any(|row| row.table == table && row.doc_id == id))
        },
        replacement_parent,
    )
}

pub(crate) fn period_foreign_key_coverage_excluding(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &DeletedRowLookup<'_>,
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<bool, SQLError> {
    Ok(period_parents(
        context,
        foreign_key,
        local_values,
        excluded_parents,
        replacement_parent,
        false,
    )?
    .is_some_and(|parents| parents.observation.covered))
}

/// Whether a matching parent overlaps the child, as `PostgreSQL`'s temporal partition removal check requires.
pub fn period_foreign_key_overlap(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
) -> Result<bool, SQLError> {
    Ok(period_parents(
        context,
        foreign_key,
        local_values,
        &|_, _| Ok(false),
        None,
        false,
    )?
    .is_some_and(|parents| parents.observation.overlaps))
}

fn period_parents(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &DeletedRowLookup<'_>,
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
    retain_ids: bool,
) -> Result<Option<PeriodParents>, SQLError> {
    super::authorize_foreign_key_parent_namespace(context, foreign_key)?;
    let Some(period_column) = foreign_key.ref_columns.last() else {
        return Err(SQLError::Internal(
            "PERIOD foreign key has no referenced period column".into(),
        ));
    };
    let parent_type = context
        .catalog
        .column_type(&foreign_key.ref_table, period_column)
        .map_err(|error| dml_storage_error("PERIOD foreign-key type lookup", error))?
        .ok_or_else(|| {
            SQLError::UnknownColumn(format!("{}.{period_column}", foreign_key.ref_table))
        })?;
    let Some(child_period) = local_values.last() else {
        return Err(SQLError::Internal(
            "PERIOD foreign key has no local period value".into(),
        ));
    };
    let (subtype, children) = period_ranges(child_period, &parent_type)?;
    if children.is_empty() {
        return Ok(None);
    }
    let tables = uqa_sql::semantics::partition::foreign_key_scan_tables(
        context.partitions.catalog,
        &foreign_key.ref_table,
    )?;
    let bytes = context.memory.work_mem_bytes()?;
    let mut ranges = ParentRanges::new(subtype, bytes, context.cancellation.clone());
    let mut ids =
        retain_ids.then(|| ParentIds::new(tables.clone(), bytes, context.cancellation.clone()));
    let scan = ParentScan {
        context,
        parent_type,
        period_column,
        subtype,
        ordinary_columns: &foreign_key.ref_columns[..foreign_key.ref_columns.len() - 1],
        ordinary_values: &local_values[..local_values.len() - 1],
        excluded_parents,
        replacement_parent,
    };
    for (ordinal, table) in tables.iter().enumerate() {
        scan.visit(table, &mut |doc_id, parent_ranges| {
            for range in parent_ranges {
                ranges.push(&range)?;
            }
            if let Some(ids) = &mut ids {
                ids.push(ordinal, doc_id)?;
            }
            Ok(())
        })?;
    }
    Ok(Some(PeriodParents {
        observation: ranges.observe(&children)?,
        ids: ids.map(ParentIds::finish).transpose()?,
    }))
}

struct ParentScan<'a> {
    context: ConstraintContext<'a>,
    parent_type: uqa_sql::ColumnType,
    period_column: &'a str,
    subtype: uqa_sql::ast::RangeSubtype,
    ordinary_columns: &'a [String],
    ordinary_values: &'a [Value],
    excluded_parents: &'a DeletedRowLookup<'a>,
    replacement_parent: Option<(&'a PhysicalDocumentIdentity, &'a Document)>,
}

impl ParentScan<'_> {
    fn visit(
        &self,
        table: &str,
        visit: &mut dyn FnMut(uqa_core::DocId, Vec<CanonicalRange>) -> Result<(), SQLError>,
    ) -> Result<(), SQLError> {
        let control = uqa_storage::read_control::StorageReadControl::new(
            &uqa_core::memory::MemoryBudget::new(64 * 1024),
            self.context.cancellation,
        );
        let mut after = None;
        loop {
            let page = self
                .context
                .reads
                .table_doc_id_page(table, after, 128, &control)?;
            let Some(last) = page.last().copied() else {
                break;
            };
            after = Some(last);
            for &id in page.iter() {
                self.context.cancellation.check()?;
                if let Some(ranges) = self.ranges(table, id)? {
                    visit(id, ranges)?;
                }
            }
        }
        Ok(())
    }

    fn ranges(
        &self,
        table: &str,
        id: uqa_core::DocId,
    ) -> Result<Option<Vec<CanonicalRange>>, SQLError> {
        let replacement = self
            .replacement_parent
            .filter(|(identity, _)| identity.table == table && identity.doc_id == id)
            .map(|(_, document)| document);
        if replacement.is_none() && (self.excluded_parents)(table, id)? {
            return Ok(None);
        }
        let owned = if replacement.is_some() {
            None
        } else {
            Some(self.context.reads.get_document(table, id)?)
        };
        let parent = replacement
            .or_else(|| owned.as_ref().and_then(Option::as_ref))
            .ok_or_else(|| missing_document_error("PERIOD foreign-key parent scan", table, id))?;
        for (column, value) in self.ordinary_columns.iter().zip(self.ordinary_values) {
            if !uqa_sql::expr::compare_typed_values_with_enum_catalog(
                parent.get(column).unwrap_or(&Value::Null),
                value,
                &uqa_core::memory::ProductionControl::uncontrolled(),
                Some(self.context.values),
                None,
            )?
            .is_eq()
            {
                return Ok(None);
            }
        }
        let period = parent
            .get(self.period_column)
            .cloned()
            .unwrap_or(Value::Null);
        if matches!(period, Value::Null) {
            return Ok(None);
        }
        let (subtype, ranges) = period_ranges(&period, &self.parent_type)?;
        if subtype != self.subtype {
            return Err(SQLError::TypeMismatch(
                "PERIOD foreign-key range subtypes do not match".into(),
            ));
        }
        Ok(Some(ranges))
    }
}
