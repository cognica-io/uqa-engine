//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Admitted projection of stored index addresses and ownership, without materializing expression trees.

use super::super::{IndexCatalogIdentity, IndexRelationships};
use crate::{
    catalog::work::{compare_text, inline, Work},
    SQLError,
};
use uqa_core::{
    catalog_identity::CatalogObjectIdentity,
    json::JsonToken,
    memory::{Produced, ProductionControl},
};

mod definition;
pub use definition::definition;

mod json;
use json::{object_id, FieldSet, Input};

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
    inline(metadata_with_work(definition, control, &mut ()))
}

/// Select the same ownership fields through Core's borrowed, resumable JSON reader.
pub async fn metadata_with_work<'a, W: Work + Send>(
    definition: Option<&'a str>,
    control: &ProductionControl<'_>,
    work: &mut W,
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
    let mut input = Input::begin(text, JsonToken::StartObject, control, work).await?;
    while let Some((key, value)) = input.field(work).await? {
        match seen.field(&key)? {
            0 if value != "null" => result.catalog = Some(catalog(value, control, work).await?),
            1 => result.relationships = relationships(value, control, work).await?,
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
    }
    seen.require((1 << 7) | (1 << 8))?;
    Ok(result)
}

async fn catalog<W: Work + Send>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<Produced<IndexCatalogIdentity>, SQLError> {
    let mut seen = FieldSet::new(&["identity", "table_object_id", "physical_key"]);
    let mut identity = None;
    let mut table_object_id = None;
    let mut physical_key = None;
    let mut input = Input::begin(text, JsonToken::StartObject, control, work).await?;
    while let Some((key, value)) = input.field(work).await? {
        match seen.field(&key)? {
            0 => identity = Some(address(value, control, work).await?),
            1 => table_object_id = Some(object_id(value, control, work).await?),
            2 => {
                physical_key = Some(input.string(value.as_bytes(), work).await?);
            }
            _ => unreachable!("known catalog field"),
        }
    }
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

async fn address<W: Work + Send>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<CatalogObjectIdentity, SQLError> {
    let mut seen = FieldSet::new(&["object_id", "oid"]);
    let mut object = None;
    let mut oid = None;
    let mut input = Input::begin(text, JsonToken::StartObject, control, work).await?;
    while let Some((key, value)) = input.field(work).await? {
        match seen.field(&key)? {
            0 => object = Some(object_id(value, control, work).await?),
            1 => oid = Some(value.parse().map_err(|_| invalid())?),
            _ => unreachable!("known identity field"),
        }
    }
    seen.require(3)?;
    Ok(CatalogObjectIdentity {
        object_id: object.ok_or_else(invalid)?,
        oid: oid.ok_or_else(invalid)?,
    })
}

async fn relationships<W: Work + Send>(
    text: &str,
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<IndexRelationships, SQLError> {
    let mut seen = FieldSet::new(&["owning_constraint", "parent_index"]);
    let mut result = IndexRelationships::default();
    let mut input = Input::begin(text, JsonToken::StartObject, control, work).await?;
    while let Some((key, value)) = input.field(work).await? {
        let field = seen.field(&key)?;
        let id = if value == "null" {
            None
        } else {
            Some(object_id(value, control, work).await?)
        };
        match field {
            0 => result.owning_constraint = id,
            1 => result.parent_index = id,
            _ => unreachable!("known relationship field"),
        }
    }
    Ok(result)
}

/// One stored key after validating its original value span. Expressions remain borrowed in the source and are never evaluated.
pub enum StoredColumnKey {
    Column(Produced<String>),
    Expression,
}

/// A cursor over the original list; each key is decoded only when its caller visits it.
pub struct ColumnKeys<'a, 'c> {
    input: Input<'a, 'c>,
    complete: bool,
}

impl<'a, 'c> ColumnKeys<'a, 'c> {
    pub async fn new<W: Work + Send>(
        text: &'a str,
        control: &ProductionControl<'c>,
        work: &mut W,
    ) -> Result<Self, SQLError> {
        Ok(Self {
            input: Input::begin(text, JsonToken::StartArray, control, work).await?,
            complete: false,
        })
    }

    pub async fn next<W: Work + Send>(
        &mut self,
        work: &mut W,
    ) -> Result<Option<StoredColumnKey>, SQLError> {
        if self.complete {
            return Ok(None);
        }
        let Some(value) = self.input.element(work).await? else {
            self.complete = true;
            return Ok(None);
        };
        let key = if value.starts_with('"') {
            StoredColumnKey::Column(self.input.string(value.as_bytes(), work).await?)
        } else {
            StoredColumnKey::Expression
        };
        Ok(Some(key))
    }
}

/// Compare a stored column-key or INCLUDE list without allocating a complete list or interpreting an expression as a column.
pub fn columns_match(
    text: Option<&str>,
    expected: &[String],
    control: &ProductionControl<'_>,
) -> Result<bool, SQLError> {
    inline(columns_match_with_work(text, expected, control, &mut ()))
}

pub async fn columns_match_with_work<W: Work + Send>(
    text: Option<&str>,
    expected: &[String],
    control: &ProductionControl<'_>,
    work: &mut W,
) -> Result<bool, SQLError> {
    control.check()?;
    let Some(text) = text else {
        return Ok(expected.is_empty());
    };
    let mut keys = ColumnKeys::new(text, control, work).await?;
    let mut matches = true;
    let mut position = 0;
    while let Some(key) = keys.next(work).await? {
        matches &= match (key, expected.get(position)) {
            (StoredColumnKey::Column(name), Some(expected)) => {
                compare_text(work, &name, expected, control).await?.is_eq()
            }
            _ => false,
        };
        position += 1;
    }
    Ok(matches && position == expected.len())
}

/// Visit ordinary column keys with admitted decoding. Expression keys return false without evaluating or materializing their trees.
pub fn visit_column_keys(
    text: &str,
    control: &ProductionControl<'_>,
    mut visit: impl FnMut(&str) -> Result<(), SQLError>,
) -> Result<bool, SQLError> {
    inline(async {
        let mut keys = ColumnKeys::new(text, control, &mut ()).await?;
        let mut ordinary = true;
        while let Some(key) = keys.next(&mut ()).await? {
            match key {
                StoredColumnKey::Column(name) => visit(&name)?,
                StoredColumnKey::Expression => ordinary = false,
            }
        }
        Ok(ordinary)
    })
}

fn invalid() -> SQLError {
    SQLError::Internal("invalid native index ownership metadata".into())
}

#[cfg(test)]
mod tests;
