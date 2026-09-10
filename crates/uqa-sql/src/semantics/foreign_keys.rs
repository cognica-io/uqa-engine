//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Foreign-key comparison types, null matching, and normalized key values.
use crate::{
    ast::{ForeignKey, ForeignKeyMatch},
    semantics::partition::PartitionCatalog,
    ColumnType, ResultRow as Document, SQLError,
};
use uqa_core::Value;
fn dml_storage_error(action: &str, error: impl std::fmt::Display) -> SQLError {
    SQLError::Internal(format!("{action} failed in storage backend: {error}"))
}

pub struct ForeignKeyLookup {
    pub values: Vec<Value>,
    pub comparison: ForeignKeyComparison,
}

pub struct ForeignKeyComparison {
    pub comparison_types: Vec<ColumnType>,
    pub exact_reference_lookup: bool,
}

impl ForeignKeyComparison {
    pub fn normalize(&self, values: Vec<Value>) -> Result<Vec<Value>, SQLError> {
        normalize_foreign_key_values(values, &self.comparison_types)
    }
}

pub fn foreign_key_relation_name(table: &str) -> String {
    uqa_core::RelationIdentity::from_legacy_name(table)
        .map_or_else(|_| table.to_string(), |relation| relation.name)
}

pub fn foreign_key_lookup_values(
    catalog: &dyn PartitionCatalog,
    table: &str,
    fk: &ForeignKey,
    document: &Document,
) -> Result<Option<ForeignKeyLookup>, SQLError> {
    let comparison = foreign_key_comparison_types(catalog, table, fk)?;
    Ok(foreign_key_values(fk, document, &comparison)?
        .map(|values| ForeignKeyLookup { values, comparison }))
}

pub fn foreign_key_values(
    fk: &ForeignKey,
    document: &Document,
    comparison: &ForeignKeyComparison,
) -> Result<Option<Vec<Value>>, SQLError> {
    let local_values: Vec<Value> = fk
        .local_columns
        .iter()
        .map(|c| document.get(c).cloned().unwrap_or(Value::Null))
        .collect();
    let null_count = local_values
        .iter()
        .filter(|value| matches!(value, Value::Null))
        .count();
    if null_count == 0 {
        if local_values.len() != fk.ref_columns.len() {
            return Err(SQLError::Internal(
                "FOREIGN KEY local and referenced column counts diverged after validation".into(),
            ));
        }
        return comparison.normalize(local_values).map(Some);
    }
    match fk.match_type {
        ForeignKeyMatch::Simple => Ok(None),
        ForeignKeyMatch::Full if null_count == local_values.len() => Ok(None),
        ForeignKeyMatch::Full => {
            Err(SQLError::Routine {
                sqlstate: "23503".into(),
                message: format!(
                    "insert or update on table violates foreign key constraint \"{}\": MATCH FULL does not allow mixing of null and nonnull key values",
                    fk.name.as_deref().unwrap_or("<unnamed>")
                ),
            })
        }
    }
}

pub fn foreign_key_comparison_types(
    catalog: &dyn PartitionCatalog,
    table: &str,
    fk: &ForeignKey,
) -> Result<ForeignKeyComparison, SQLError> {
    if fk.local_columns.len() != fk.ref_columns.len() {
        return Err(SQLError::Internal(
            "FOREIGN KEY local and referenced column counts diverged after validation".into(),
        ));
    }
    let local_columns = catalog
        .try_describe_table(table)
        .map_err(|error| dml_storage_error("FOREIGN KEY local columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let referenced_columns = catalog
        .try_describe_table(&fk.ref_table)
        .map_err(|error| dml_storage_error("FOREIGN KEY referenced columns", error))?
        .ok_or_else(|| SQLError::UnknownTable(fk.ref_table.clone()))?;
    let mut comparison_types = Vec::with_capacity(fk.local_columns.len());
    let mut exact_reference_lookup = true;
    for (local_column, referenced_column) in fk.local_columns.iter().zip(&fk.ref_columns) {
        let local_type = local_columns
            .iter()
            .find(|definition| definition.name == *local_column)
            .map(|definition| &definition.ty)
            .ok_or_else(|| SQLError::UnknownColumn(format!("{table}.{local_column}")))?;
        let referenced_type = referenced_columns
            .iter()
            .find(|definition| definition.name == *referenced_column)
            .map(|definition| &definition.ty)
            .ok_or_else(|| {
                SQLError::UnknownColumn(format!("{}.{referenced_column}", fk.ref_table))
            })?;
        let comparison_type =
            crate::type_resolution::foreign_key_operand_type(local_type, referenced_type).map_err(|_| {
                SQLError::Routine {
                    sqlstate: "42804".into(),
                    message: format!(
                        "foreign key constraint cannot be implemented: key columns \"{local_column}\" and \"{referenced_column}\" are of incompatible types: {} and {}",
                        local_type.sql_name(),
                        referenced_type.sql_name()
                    ),
                }
            })?;
        exact_reference_lookup &= comparison_type == *referenced_type;
        comparison_types.push(comparison_type);
    }
    Ok(ForeignKeyComparison {
        comparison_types,
        exact_reference_lookup,
    })
}

pub fn foreign_key_parent_values(
    fk: &ForeignKey,
    document: &Document,
    comparison: &ForeignKeyComparison,
) -> Result<Vec<Value>, SQLError> {
    comparison.normalize(
        fk.ref_columns
            .iter()
            .map(|column| document.get(column).cloned().unwrap_or(Value::Null))
            .collect(),
    )
}

pub fn normalize_foreign_key_values(
    values: Vec<Value>,
    comparison_types: &[ColumnType],
) -> Result<Vec<Value>, SQLError> {
    if values.len() != comparison_types.len() {
        return Err(SQLError::Internal(
            "FOREIGN KEY value and comparison-type counts diverged after validation".into(),
        ));
    }
    values
        .into_iter()
        .zip(comparison_types)
        .map(|(value, ty)| crate::assignment::conversion::convert_value_to_column_type(value, ty))
        .collect()
}
