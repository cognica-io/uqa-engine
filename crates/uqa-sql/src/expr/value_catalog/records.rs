//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed record fields retain the statement's descriptor for SQL consumers.

use super::{Arc, SQLValueCatalog};
use crate::{
    catalog::type_metadata::pg_type_oid, expr::composites::CompositeTypeDescriptor, ColumnType,
    SQLError,
};
use uqa_core::{RecordFieldType, Value};

enum Fields<'a> {
    Named(&'a uqa_core::RecordValue),
    Anonymous(&'a uqa_core::RowValue),
}

/// Record values and declared field identities from one pinned catalog generation. Constructing the view does not read or copy any field value.
pub struct SQLRecordFields<'a> {
    fields: Fields<'a>,
    descriptor: Option<Arc<CompositeTypeDescriptor>>,
}

impl<'a> SQLRecordFields<'a> {
    pub fn new(value: &'a Value, catalog: Option<&dyn SQLValueCatalog>) -> Result<Self, SQLError> {
        let (fields, oid) = match value {
            Value::Record(record) => (Fields::Named(record), record.type_oid()),
            Value::Row(row) => (Fields::Anonymous(row), None),
            _ => {
                return Err(SQLError::Internal(
                    "record field view requires a record value".into(),
                ))
            }
        };
        let descriptor = oid
            .map(|oid| {
                catalog
                    .ok_or_else(|| {
                        SQLError::Internal("named record comparison has no catalog".into())
                    })?
                    .value_composite_type(oid)?
                    .ok_or_else(|| SQLError::Routine {
                        sqlstate: "42704".into(),
                        message: format!("type with OID {oid} does not exist"),
                    })
            })
            .transpose()?;
        Ok(Self { fields, descriptor })
    }

    pub fn value(&self, index: usize) -> Option<&'a Value> {
        match self.fields {
            Fields::Named(fields) => fields.get(index).map(|(_, value)| value),
            Fields::Anonymous(fields) => fields.get(index),
        }
    }

    pub fn declared_field_type(&self, index: usize) -> Option<&ColumnType> {
        self.descriptor
            .as_ref()?
            .attributes
            .get(index)
            .map(|attribute| &attribute.ty)
    }

    pub fn field_type_oid(&self, index: usize) -> Option<u32> {
        if let Some(ty) = self.declared_field_type(index) {
            return Some(pg_type_oid(ty) as u32);
        }
        match self.fields {
            Fields::Anonymous(fields) => fields
                .field_types()?
                .get(index)
                .map(|RecordFieldType { oid, .. }| *oid),
            Fields::Named(_) => None,
        }
    }
}
