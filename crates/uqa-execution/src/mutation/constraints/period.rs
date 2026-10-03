//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Scan referenced temporal keys and prove coverage of the child range.
use super::{
    dml_storage_error, missing_document_error, ConstraintContext, Document, ForeignKey,
    PhysicalDocumentIdentity, SQLError, Value,
};
use uqa_sql::{expr::multirange_from_ranges, semantics::period::period_ranges};

/// The child's period ranges, the periods of the referenced rows whose ordinary key matches the child's, and the identities of those rows.
struct PeriodParents {
    child_ranges: Vec<uqa_sql::expr::CanonicalRange>,
    coverage: uqa_sql::expr::CanonicalMultirange,
    parent_ids: Vec<PhysicalDocumentIdentity>,
}

pub fn period_foreign_key_coverage(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<(bool, Vec<PhysicalDocumentIdentity>), SQLError> {
    let Some(parents) = period_parents(
        context,
        foreign_key,
        local_values,
        excluded_parents,
        replacement_parent,
    )?
    else {
        return Ok((false, Vec::new()));
    };
    Ok((
        parents
            .child_ranges
            .iter()
            .all(|range| parents.coverage.contains_range(range)),
        parents.parent_ids,
    ))
}

/// Whether a referenced row whose ordinary key matches the child's has a period that overlaps the child's, as `PostgreSQL`'s partition removal check joins a temporal foreign key's rows.
pub fn period_foreign_key_overlap(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
) -> Result<bool, SQLError> {
    Ok(
        period_parents(context, foreign_key, local_values, &[], None)?.is_some_and(|parents| {
            parents
                .child_ranges
                .iter()
                .any(|range| parents.coverage.overlaps_range(range))
        }),
    )
}

fn period_parents(
    context: ConstraintContext<'_>,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
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
    let (child_subtype, child_ranges) = period_ranges(child_period, &parent_type)?;
    if child_ranges.is_empty() {
        return Ok(None);
    }
    let ordinary_values = &local_values[..local_values.len() - 1];
    let ordinary_columns = &foreign_key.ref_columns[..foreign_key.ref_columns.len() - 1];
    let mut parent_ranges = Vec::new();
    let mut parent_ids = Vec::new();
    for physical_table in uqa_sql::semantics::partition::foreign_key_scan_tables(
        context.partitions.catalog,
        &foreign_key.ref_table,
    )? {
        for doc_id in context.reads.table_doc_ids(&physical_table)? {
            let identity = PhysicalDocumentIdentity {
                table: physical_table.clone(),
                doc_id,
            };
            let replacement = replacement_parent
                .filter(|(replacement_identity, _)| *replacement_identity == &identity)
                .map(|(_, document)| document);
            if replacement.is_none() && excluded_parents.contains(&identity) {
                continue;
            }
            let owned_parent = if replacement.is_some() {
                None
            } else {
                Some(context.reads.get_document(&physical_table, doc_id)?)
            };
            let parent =
                match replacement.or_else(|| owned_parent.as_ref().and_then(Option::as_ref)) {
                    Some(parent) => parent,
                    None => {
                        return Err(missing_document_error(
                            "PERIOD foreign-key parent scan",
                            &physical_table,
                            doc_id,
                        ));
                    }
                };
            if !ordinary_columns
                .iter()
                .zip(ordinary_values)
                .all(|(column, value)| parent.get(column).cloned().unwrap_or(Value::Null) == *value)
            {
                continue;
            }
            let parent_period = parent.get(period_column).cloned().unwrap_or(Value::Null);
            if matches!(parent_period, Value::Null) {
                continue;
            }
            let (parent_subtype, mut ranges) = period_ranges(&parent_period, &parent_type)?;
            if parent_subtype != child_subtype {
                return Err(SQLError::TypeMismatch(
                    "PERIOD foreign-key range subtypes do not match".into(),
                ));
            }
            parent_ranges.append(&mut ranges);
            parent_ids.push(identity);
        }
    }
    Ok(Some(PeriodParents {
        child_ranges,
        coverage: multirange_from_ranges(child_subtype, parent_ranges),
        parent_ids,
    }))
}
