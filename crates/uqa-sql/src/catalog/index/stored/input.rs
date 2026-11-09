//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted projection of stored index addresses and ownership, without materializing expression trees.

use super::super::{IndexCatalogIdentity, IndexRelationships};
use crate::SQLError;
use uqa_core::{
    catalog_identity::CatalogObjectIdentity,
    memory::{Produced, ProductionControl},
};

mod json;
use json::{fields, object_id, read_error, FieldSet};

/// Borrowed definition payloads retain their original catalog owner. The only copied payload is the physical key, whose original decoding allowance stays with the result. This projection validates ownership metadata; expression and type semantics remain separate predicates.
#[derive(Default)]
pub struct IndexMetadata<'a> {
    pub catalog: Option<Produced<IndexCatalogIdentity>>,
    pub relationships: IndexRelationships,
    pub included_columns: Option<&'a str>,
    pub predicate: Option<&'a str>,
    pub unique: bool,
    pub nulls_not_distinct: bool,
}

pub fn metadata<'a>(
    definition: Option<&'a str>,
    control: &ProductionControl<'_>,
) -> Result<IndexMetadata<'a>, SQLError> {
    control.check()?;
    let Some(text) = definition else {
        return Ok(IndexMetadata::default());
    };
    let mut result = IndexMetadata::default();
    let mut seen = FieldSet::new(&[
        "catalog",
        "relationships",
        "included_columns",
        "column_order",
        "key_names",
        "key_types",
        "predicate",
        "unique",
        "nulls_not_distinct",
    ]);
    fields(text, control, |key, value| {
        match seen.field(key)? {
            0 if value != "null" => result.catalog = Some(catalog(value, control)?),
            1 => result.relationships = relationships(value, control)?,
            2 => {
                json::require_array(value)?;
                result.included_columns = Some(value);
            }
            3..=5 => json::require_array(value)?,
            6 if value != "null" => result.predicate = Some(value),
            7 => result.unique = json::boolean(value)?,
            8 => result.nulls_not_distinct = json::boolean(value)?,
            _ => {}
        }
        Ok(())
    })?;
    seen.require((1 << 7) | (1 << 8))?;
    Ok(result)
}

fn catalog(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<IndexCatalogIdentity>, SQLError> {
    let mut seen = FieldSet::new(&["identity", "table_object_id", "physical_key"]);
    let mut identity = None;
    let mut table_object_id = None;
    let mut physical_key = None;
    fields(text, control, |key, value| {
        match seen.field(key)? {
            0 => identity = Some(address(value, control)?),
            1 => table_object_id = Some(object_id(value, control)?),
            2 => {
                physical_key = Some(
                    uqa_core::json::decode_json_string_with_control(value, control)
                        .map_err(read_error)?,
                );
            }
            _ => unreachable!("known catalog field"),
        }
        Ok(())
    })?;
    seen.require(7)?;
    let (physical_key, memory) = physical_key.ok_or_else(invalid)?.into_parts();
    Ok(control.finish(
        IndexCatalogIdentity {
            identity: identity.ok_or_else(invalid)?,
            table_object_id: table_object_id.ok_or_else(invalid)?,
            physical_key,
        },
        memory,
    )?)
}

fn address(text: &str, control: &ProductionControl<'_>) -> Result<CatalogObjectIdentity, SQLError> {
    let mut seen = FieldSet::new(&["object_id", "oid"]);
    let mut object = None;
    let mut oid = None;
    fields(text, control, |key, value| {
        match seen.field(key)? {
            0 => object = Some(object_id(value, control)?),
            1 => oid = Some(value.parse().map_err(|_| invalid())?),
            _ => unreachable!("known identity field"),
        }
        Ok(())
    })?;
    seen.require(3)?;
    Ok(CatalogObjectIdentity {
        object_id: object.ok_or_else(invalid)?,
        oid: oid.ok_or_else(invalid)?,
    })
}

fn relationships(
    text: &str,
    control: &ProductionControl<'_>,
) -> Result<IndexRelationships, SQLError> {
    let mut seen = FieldSet::new(&["owning_constraint", "parent_index"]);
    let mut result = IndexRelationships::default();
    fields(text, control, |key, value| {
        let field = seen.field(key)?;
        let id = if value == "null" {
            None
        } else {
            Some(object_id(value, control)?)
        };
        match field {
            0 => result.owning_constraint = id,
            1 => result.parent_index = id,
            _ => unreachable!("known relationship field"),
        }
        Ok(())
    })?;
    Ok(result)
}

/// Compare a stored column-key or INCLUDE list without allocating a complete list or interpreting an expression as a column.
pub fn columns_match(
    text: Option<&str>,
    expected: &[String],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    control.check()?;
    let Some(text) = text else {
        return Ok(expected.is_empty());
    };
    let mut matches = true;
    let mut position = 0;
    json::array(text, control, |value| {
        if value.starts_with('"') {
            let name = uqa_core::json::decode_json_string_with_control(value, control)
                .map_err(read_error)?;
            matches &= expected
                .get(position)
                .is_some_and(|expected| name.as_str() == expected.as_str());
        } else {
            matches = false;
        }
        position += 1;
        Ok(())
    })?;
    Ok(matches && position == expected.len())
}

fn invalid() -> SQLError {
    SQLError::Internal("invalid native index ownership metadata".into())
}

#[cfg(test)]
mod tests;
