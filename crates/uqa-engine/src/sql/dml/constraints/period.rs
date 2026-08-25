//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Temporal primary-key and foreign-key range coverage.

use super::{
    dml_storage_error, missing_document_error, ColumnType, Document, Engine, ForeignKey,
    PhysicalDocumentIdentity, SQLError, Value,
};
use uqa_sql::ast::RangeSubtype;
use uqa_sql::expr::{multirange_from_ranges, parse_multirange, parse_range, CanonicalRange};

pub(in crate::sql) fn period_foreign_key_coverage(
    engine: &Engine,
    foreign_key: &ForeignKey,
    local_values: &[Value],
    excluded_parents: &[PhysicalDocumentIdentity],
    replacement_parent: Option<(&PhysicalDocumentIdentity, &Document)>,
) -> Result<(bool, Vec<PhysicalDocumentIdentity>), SQLError> {
    let Some(period_column) = foreign_key.ref_columns.last() else {
        return Err(SQLError::Internal(
            "PERIOD foreign key has no referenced period column".into(),
        ));
    };
    let parent_type = engine
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
        return Ok((false, Vec::new()));
    }
    let ordinary_values = &local_values[..local_values.len() - 1];
    let ordinary_columns = &foreign_key.ref_columns[..foreign_key.ref_columns.len() - 1];
    let mut parent_ranges = Vec::new();
    let mut parent_ids = Vec::new();
    for physical_table in engine.hierarchy_scan_tables(&foreign_key.ref_table, true)? {
        for doc_id in engine.table_doc_ids(&physical_table)? {
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
                Some(engine.get_document(&physical_table, doc_id)?)
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
    let coverage = multirange_from_ranges(child_subtype, parent_ranges);
    Ok((
        child_ranges
            .iter()
            .all(|range| coverage.contains_range(range)),
        parent_ids,
    ))
}

pub(super) fn period_ranges(
    value: &Value,
    column_type: &ColumnType,
) -> Result<(RangeSubtype, Vec<CanonicalRange>), SQLError> {
    let (Value::Str(text) | Value::FixedChar(text)) = value else {
        return Err(SQLError::TypeMismatch(format!(
            "PERIOD value has incompatible runtime carrier {value:?}"
        )));
    };
    match column_type {
        ColumnType::Range(subtype) => {
            let range = parse_range(text, *subtype)?;
            Ok((
                *subtype,
                (!range.is_empty()).then_some(range).into_iter().collect(),
            ))
        }
        ColumnType::Multirange(subtype) => Ok((
            *subtype,
            parse_multirange(text, *subtype)?.ranges().to_vec(),
        )),
        other => Err(SQLError::TypeMismatch(format!(
            "PERIOD column must be a range or multirange, got {}",
            other.sql_name()
        ))),
    }
}
