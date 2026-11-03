//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored index and scoring metadata capture preserves original encoded payloads without decoding.

use crate::{
    read_control::StorageReadControl, CatalogIndexRow, RelationIdentity, StorageBackendResult,
};
use std::collections::BTreeMap;
use uqa_core::memory::{Budgeted, Produced, ProductionControl, ProductionVec};

fn production(control: &StorageReadControl) -> ProductionControl<'_> {
    ProductionControl::new(
        control.memory(),
        control.cancellation(),
        control.cancellation(),
    )
}

fn relation(
    value: &RelationIdentity,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<RelationIdentity>> {
    let (schema, schema_memory) = control.copy_text(&value.schema)?.into_parts();
    let (name, name_memory) = control.copy_text(&value.name)?.into_parts();
    Ok(control.finish(
        RelationIdentity { schema, name },
        control.combine(schema_memory, name_memory),
    )?)
}

fn index(
    value: &CatalogIndexRow,
    control: &ProductionControl<'_>,
) -> StorageBackendResult<Produced<CatalogIndexRow>> {
    let CatalogIndexRow {
        relation: name,
        index_type,
        table_name,
        columns_json,
        parameters_json,
        definition_json,
    } = value;
    let (name, mut memory) = relation(name, control)?.into_parts();
    let mut text = |value: &str| -> StorageBackendResult<String> {
        let (value, lease) = control.copy_text(value)?.into_parts();
        memory = control.combine(memory.take(), lease);
        Ok(value)
    };
    let value = CatalogIndexRow {
        relation: name,
        index_type: text(index_type)?,
        table_name: text(table_name)?,
        columns_json: text(columns_json)?,
        parameters_json: text(parameters_json)?,
        definition_json: definition_json.as_deref().map(text).transpose()?,
    };
    Ok(control.finish(value, memory)?)
}

pub fn indexes(
    values: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Vec<(RelationIdentity, CatalogIndexRow)>>> {
    selected_indexes(values, |_| Ok(true), control)
}

/// The immutable table selection precedes output capacity and encoded-payload admission.
pub fn selected_indexes(
    values: &BTreeMap<RelationIdentity, CatalogIndexRow>,
    selected: impl Fn(&str) -> StorageBackendResult<bool>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Vec<(RelationIdentity, CatalogIndexRow)>>> {
    let control = production(control);
    let count = values.values().try_fold(0, |count, value| {
        control.check()?;
        Ok::<_, crate::StorageBackendError>(count + usize::from(selected(&value.table_name)?))
    })?;
    let mut entries = ProductionVec::new(control);
    entries.reserve(count)?;
    for (name, value) in values {
        control.check()?;
        if !selected(&value.table_name)? {
            continue;
        }
        let (name, key_memory) = relation(name, &control)?.into_parts();
        let (value, value_memory) = index(value, &control)?.into_parts();
        entries.push_produced(
            control.finish((name, value), control.combine(key_memory, value_memory))?,
        )?;
    }
    Ok(entries
        .finish()?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("storage capture owns an allowance")))
}

pub fn scoring_params(
    values: &BTreeMap<String, String>,
    control: &StorageReadControl,
) -> StorageBackendResult<Budgeted<Vec<(String, String)>>> {
    let control = production(control);
    let mut entries = ProductionVec::new(control);
    entries.reserve(values.len())?;
    for (name, value) in values {
        let (name, key_memory) = control.copy_text(name)?.into_parts();
        let (value, value_memory) = control.copy_text(value)?.into_parts();
        entries.push_produced(
            control.finish((name, value), control.combine(key_memory, value_memory))?,
        )?;
    }
    Ok(entries
        .finish()?
        .into_budgeted()
        .unwrap_or_else(|_| unreachable!("storage capture owns an allowance")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row() -> CatalogIndexRow {
        CatalogIndexRow {
            relation: RelationIdentity::new("Quoted.Schema", "Index.Name"),
            index_type: "btree".into(),
            table_name: "\"Quoted.Schema\".\"Table.Name\"".into(),
            columns_json: "[ \"a\", \"b\" ]".into(),
            parameters_json: "{ \"parameter\":1.00 }".into(),
            definition_json: Some("{ \"expression\": \"original\" }".into()),
        }
    }

    #[test]
    fn metadata_capture_keeps_original_encoded_index_and_scoring_payloads() {
        let row = row();
        let source = BTreeMap::from([(row.relation.clone(), row)]);
        let control = StorageReadControl::with_limit(16384);
        let mut saved = None;
        let measured =
            allocation_counter::measure(|| saved = Some(indexes(&source, &control).unwrap()));
        let saved = saved.unwrap();
        assert_eq!(
            measured.bytes_current,
            i64::try_from(control.memory().used()).unwrap()
        );
        let before = source.values().next().unwrap();
        let after = &saved[0].1;
        assert_eq!(saved[0].0, before.relation);
        assert_eq!(after.relation, before.relation);
        assert_eq!(after.index_type, before.index_type);
        assert_eq!(after.table_name, before.table_name);
        assert_eq!(after.columns_json, before.columns_json);
        assert_eq!(after.parameters_json, before.parameters_json);
        assert_eq!(after.definition_json, before.definition_json);
        drop(source);
        let parameters =
            BTreeMap::from([("exact signal".into(), "{ \"x\": 1.00, \"x\": 2 }".into())]);
        let values = scoring_params(&parameters, &control).unwrap();
        assert!(values.iter().map(|(k, v)| (k, v)).eq(parameters.iter()));
        drop((parameters, values, saved));
        assert_eq!(control.memory().used(), 0);
    }

    #[test]
    fn metadata_capture_rejects_large_late_payloads_and_cancellation_without_leaks() {
        let mut row = row();
        row.definition_json = Some("x".repeat(32768));
        let source = BTreeMap::from([(row.relation.clone(), row)]);
        let parameters = BTreeMap::from([("signal".into(), "x".repeat(32768))]);
        let control = StorageReadControl::with_limit(4096);
        assert!(indexes(&source, &control).is_err());
        assert_eq!(control.memory().used(), 0);
        assert!(scoring_params(&parameters, &control).is_err());
        assert_eq!(control.memory().used(), 0);
        control.cancellation().cancel();
        assert!(indexes(&source, &control).is_err());
        assert!(scoring_params(&parameters, &control).is_err());
        assert_eq!(control.memory().used(), 0);
    }
}
