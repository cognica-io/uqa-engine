//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A procedural record keeps its assignment-time field layout and original tuple.

use std::sync::Arc;
use uqa_core::{memory::ProductionControl, Value};
use uqa_sql::{
    expr::{
        composites::{CompositeTypeCatalog, CompositeTypeDescriptor},
        EngineHook, EngineValueCatalog, SQLRecordView,
    },
    RowSchema, SQLError,
};

#[derive(Clone)]
pub(in crate::routines) struct RecordVariableDescriptor {
    pub schema: RowSchema,
    physical: Option<Arc<CompositeTypeDescriptor>>,
}

impl From<RowSchema> for RecordVariableDescriptor {
    fn from(schema: RowSchema) -> Self {
        Self {
            schema,
            physical: None,
        }
    }
}

impl RecordVariableDescriptor {
    pub fn bind(
        value: Value,
        schema: Option<RowSchema>,
        engine: &dyn EngineHook,
    ) -> Result<(Value, Option<Self>), SQLError> {
        match value {
            Value::Datum(ref datum)
                if uqa_sql::expr::composites::datum_is_composite(datum, engine)? =>
            {
                let control = ProductionControl::uncontrolled();
                let catalog = EngineValueCatalog(engine);
                let physical = if let Some(descriptor) =
                    CompositeTypeDescriptor::expanded_with_control(datum, Some(&catalog), &control)?
                {
                    Arc::new(
                        descriptor
                            .into_uncontrolled()
                            .expect("procedural record descriptor"),
                    )
                } else {
                    SQLRecordView::new(&value, None, Some(&catalog), &control)?
                        .into_descriptor()
                        .expect("physical record descriptor")
                };
                let schema = RowSchema::with_column_metadata(
                    None,
                    physical.attributes.iter().map(|attribute| {
                        (
                            attribute.name.clone(),
                            Some(attribute.ty.clone()),
                            attribute.collation_oid,
                        )
                    }),
                );
                Ok((
                    physical
                        .expand_record_with_control(&value, &control)?
                        .into_uncontrolled()
                        .expect("procedural expanded record"),
                    Some(Self {
                        schema,
                        physical: Some(physical),
                    }),
                ))
            }
            Value::Record(_) | Value::Null => Ok((value, schema.map(Self::from))),
            Value::Row(row) => {
                let value = Value::Record(
                    row.into_values()
                        .into_iter()
                        .enumerate()
                        .map(|(index, value)| (format!("f{}", index + 1), value))
                        .collect::<Vec<_>>()
                        .into(),
                );
                Ok((value, schema.map(Self::from)))
            }
            _ => Err(SQLError::Routine {
                sqlstate: "42804".into(),
                message: "cannot assign non-composite value to a record variable".into(),
            }),
        }
    }

    pub fn field_index(&self, field: &str) -> Option<usize> {
        self.schema.columns().iter().position(|name| name == field)
    }

    pub fn read_field(&self, value: &Value, field: &str) -> Result<Option<Value>, SQLError> {
        if self.field_index(field).is_none() {
            return Ok(None);
        }
        let Value::Datum(datum) = value else {
            return Ok(None);
        };
        uqa_sql::expr::datums::read_composite_field_with_control(
            datum,
            field,
            Some(self),
            &ProductionControl::uncontrolled(),
        )
        .map(|value| Some(value.into_uncontrolled().expect("procedural record field")))
    }

    pub fn replace_field(
        &self,
        record: &Value,
        field: &str,
        value: &Value,
        engine: &dyn EngineHook,
    ) -> Result<Value, SQLError> {
        let descriptor = self.physical.as_ref().ok_or_else(|| {
            SQLError::Internal("physical record has no assignment descriptor".into())
        })?;
        let control = ProductionControl::uncontrolled();
        uqa_sql::expr::composites::constructor::construct_current_with_control(
            descriptor,
            engine.composite_types(),
            &control,
            |index| {
                let name = &descriptor.attributes[index].name;
                let value = if name == field {
                    value.clone()
                } else {
                    self.read_field(record, name)?
                        .expect("assigned record field")
                };
                Ok(control.retain_external_value(value)?)
            },
        )
        .and_then(|value| {
            descriptor
                .expand_record_with_control(&value, &control)
                .map(|value| {
                    value
                        .into_uncontrolled()
                        .expect("procedural record assignment")
                })
        })
    }
}

impl CompositeTypeCatalog for RecordVariableDescriptor {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(self
            .physical
            .as_ref()
            .filter(|descriptor| descriptor.type_oid == oid)
            .cloned())
    }
}
