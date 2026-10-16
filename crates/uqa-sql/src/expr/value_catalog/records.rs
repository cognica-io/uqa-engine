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

    /// Observe a retained field selection without evaluating the field. Named fields use the pinned descriptor; anonymous fields distinguish explicit zero from absent legacy metadata.
    pub fn field_collation_oid(&self, index: usize) -> Option<u32> {
        if let Some(descriptor) = &self.descriptor {
            return descriptor
                .attributes
                .get(index)
                .map(|attribute| attribute.collation_oid());
        }
        match self.fields {
            Fields::Anonymous(fields) => fields.field_types()?.get(index)?.collation_oid,
            Fields::Named(_) => None,
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expr::{composites::CompositeAttribute, enums::EnumTypeLabels};
    use std::cell::RefCell;

    struct Catalog(RefCell<Arc<CompositeTypeDescriptor>>);

    impl SQLValueCatalog for Catalog {
        fn value_composite_type(
            &self,
            _: u32,
        ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
            Ok(Some(self.0.borrow().clone()))
        }
        fn enum_type_labels(&self, _: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
            Ok(None)
        }
        fn enum_label_uncommitted(&self, _: u32) -> bool {
            false
        }
        fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
            Ok(None)
        }
        fn has_enum_types(&self) -> bool {
            false
        }
    }

    #[test]
    fn field_collations_keep_named_generations_and_anonymous_zero() {
        let descriptor = |collation_oid| {
            Arc::new(CompositeTypeDescriptor {
                dropped: Vec::new(),
                type_oid: 20_001,
                relation_oid: 20_002,
                attributes: vec![CompositeAttribute {
                    name: "v".into(),
                    ty: ColumnType::Text,
                    collation_oid,
                    number: 1,
                }],
            })
        };
        let catalog = Catalog(RefCell::new(descriptor(Some(950))));
        let value = Value::Record(uqa_core::RecordValue::from_parts(
            vec![("v".into(), Value::Null)],
            Some(20_001),
        ));
        let original = SQLRecordFields::new(&value, Some(&catalog)).unwrap();
        *catalog.0.borrow_mut() = descriptor(Some(963));
        let changed = SQLRecordFields::new(&value, Some(&catalog)).unwrap();
        assert_eq!(original.field_collation_oid(0), Some(950));
        assert_eq!(changed.field_collation_oid(0), Some(963));
        assert_eq!(original.field_collation_oid(1), None);
        *catalog.0.borrow_mut() = descriptor(None);
        assert_eq!(
            SQLRecordFields::new(&value, Some(&catalog))
                .unwrap()
                .field_collation_oid(0),
            Some(100)
        );

        let anonymous = Value::Row(
            uqa_core::RowValue::typed(
                vec![Value::Null; 3],
                [None, Some(0), Some(950)]
                    .into_iter()
                    .map(|collation_oid| RecordFieldType {
                        oid: 25,
                        type_modifier: -1,
                        collation_oid,
                    })
                    .collect(),
            )
            .unwrap(),
        );
        let fields = SQLRecordFields::new(&anonymous, None).unwrap();
        assert_eq!(fields.field_collation_oid(0), None);
        assert_eq!(fields.field_collation_oid(1), Some(0));
        assert_eq!(fields.field_collation_oid(2), Some(950));
    }
}
