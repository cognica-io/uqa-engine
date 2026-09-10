//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Trigger row images and OLD/NEW variable binding.
use super::{
    BTreeMap, BTreeSet, DocId, Document, ResolvedVariable, Result, SQLError, TriggerContext, Value,
    VariableResolver,
};

pub(super) struct TriggerVariableResolver<'a> {
    pub(super) old: &'a Value,
    pub(super) new: &'a Value,
    pub(super) types: &'a BTreeMap<String, String>,
}

impl TriggerVariableResolver<'_> {
    fn record_field(&self, record: &Value, column: &str) -> Result<ResolvedVariable> {
        let value = match record {
            Value::Null => Value::Null,
            Value::Record(fields) => fields
                .iter()
                .find(|(name, _)| name == column)
                .map(|(_, value)| value.clone())
                .ok_or_else(|| SQLError::UnknownColumn(column.to_string()))?,
            _ => {
                return Err(SQLError::Routine {
                    sqlstate: "55000".into(),
                    message: "trigger row variable is not assigned yet".into(),
                })
            }
        };
        Ok(ResolvedVariable {
            value,
            declared_type: self.types.get(column).cloned(),
        })
    }
}

impl VariableResolver for TriggerVariableResolver<'_> {
    fn resolve_name(&mut self, _name: &str) -> Result<Option<ResolvedVariable>> {
        Ok(None)
    }

    fn resolve_qualified(
        &mut self,
        qualifier: &str,
        column: &str,
    ) -> Result<Option<ResolvedVariable>> {
        if qualifier.eq_ignore_ascii_case("old") {
            return self.record_field(self.old, column).map(Some);
        }
        if qualifier.eq_ignore_ascii_case("new") {
            return self.record_field(self.new, column).map(Some);
        }
        Ok(None)
    }

    fn resolve_param(&mut self, _index: usize) -> Result<Option<ResolvedVariable>> {
        Ok(None)
    }
}

pub(super) fn trigger_column_types(
    context: &TriggerContext<'_>,
    table: &str,
) -> Result<BTreeMap<String, String>> {
    let columns = context.catalog.rule_relation_columns(table)?;
    Ok(columns
        .into_iter()
        .map(|(name, ty)| (name, ty.sql_name()))
        .collect())
}

pub(super) fn trigger_record(
    context: &TriggerContext<'_>,
    table: &str,
    doc_id: DocId,
    document: Option<&Document>,
    mask_generated: bool,
) -> Result<Value> {
    let definitions = context
        .relations
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read trigger row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    let Some(document) = document else {
        return Ok(Value::Record(
            definitions
                .iter()
                .map(|column| (column.name.clone(), Value::Null))
                .collect(),
        ));
    };
    let mut materialized = document.clone();
    for column in &definitions {
        let unavailable = column.generated.as_ref().is_some_and(|generated| {
            mask_generated || generated.kind == uqa_sql::ast::GeneratedColumnKind::Virtual
        });
        if unavailable {
            materialized.insert(column.name.clone(), Value::Null);
        }
    }
    if definitions.is_empty() {
        return Ok(Value::Record(materialized.into_iter().collect()));
    }
    let fallback_id = i64::try_from(doc_id).map(Value::Int).map_err(|_| {
        SQLError::TypeMismatch(format!("document id {doc_id} exceeds PostgreSQL bigint"))
    })?;
    Ok(Value::Record(
        definitions
            .iter()
            .map(|column| {
                let value = materialized.get(&column.name).cloned().unwrap_or_else(|| {
                    if column.primary_key && column.ty.is_integer() {
                        fallback_id.clone()
                    } else {
                        Value::Null
                    }
                });
                (column.name.clone(), value)
            })
            .collect(),
    ))
}

pub(super) fn trigger_document(
    context: &TriggerContext<'_>,
    table: &str,
    value: Value,
) -> Result<Option<Document>> {
    let fields = match value {
        Value::Null => return Ok(None),
        Value::Record(fields) => fields,
        _ => {
            return Err(SQLError::Routine {
                sqlstate: "39P01".into(),
                message: "trigger function returned non-composite value".into(),
            })
        }
    };
    let definitions = context
        .relations
        .try_describe_table(table)
        .map_err(|error| SQLError::Internal(format!("read trigger row type: {error}")))?
        .ok_or_else(|| SQLError::UnknownTable(table.to_string()))?;
    if definitions.is_empty() {
        return Ok(Some(fields.into_iter().collect()));
    }
    let known = definitions
        .iter()
        .map(|column| column.name.as_str())
        .collect::<BTreeSet<_>>();
    if let Some((unknown, _)) = fields
        .iter()
        .find(|(name, _)| !known.contains(name.as_str()))
    {
        return Err(SQLError::UnknownColumn(format!("{table}.{unknown}")));
    }
    let values = fields.into_iter().collect::<BTreeMap<_, _>>();
    let mut document = Document::new();
    for column in definitions {
        let value = values.get(&column.name).cloned().unwrap_or(Value::Null);
        document.insert(column.name.clone(), value);
    }
    Ok(Some(document))
}
