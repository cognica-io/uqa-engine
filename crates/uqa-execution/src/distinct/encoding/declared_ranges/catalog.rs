//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use std::sync::Arc;
use uqa_core::{EnumLabelKey, EnumValue, ValueRetentionError};
use uqa_sql::{
    ast::{CompositeTypeReference, EnumTypeReference, RangeAlignment, RangeTypeReference},
    expr::{
        composites::{CompositeAttribute, CompositeTypeDescriptor},
        enums::{EnumTypeLabel, EnumTypeLabels},
        RangeValueDescriptor,
    },
    ColumnType,
};

pub(super) struct Catalog {
    pub data: serde_json::Value,
    labels: Arc<EnumTypeLabels>,
    record: Arc<CompositeTypeDescriptor>,
}

pub(super) fn integer(value: &serde_json::Value) -> u32 {
    value.as_str().unwrap().parse().unwrap()
}

impl Catalog {
    pub fn new() -> Self {
        let data: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../uqa-sql/src/expr/datums/inline/tests/ranges/comparison.json"
        )))
        .unwrap();
        let case = |id| {
            data["cases"]
                .as_array()
                .unwrap()
                .iter()
                .find(|case| case["id"] == id)
                .unwrap()
        };
        let rows = case("enum_labels")["results"][0]["rows"]
            .as_array()
            .unwrap();
        let labels = Arc::new(EnumTypeLabels {
            type_oid: integer(&rows[0][0]),
            labels: rows
                .iter()
                .enumerate()
                .map(|(index, row)| EnumTypeLabel {
                    oid: integer(&row[1]),
                    key: EnumLabelKey::from_bytes(vec![(index + 1) as u8]).unwrap(),
                    label: row[2].as_str().unwrap().into(),
                })
                .collect(),
        });
        let row = case("type_metadata")["results"][0]["rows"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row[0] == "record_window")
            .unwrap();
        let record = Arc::new(CompositeTypeDescriptor {
            type_oid: integer(&row[3]),
            relation_oid: 0,
            dropped: vec![],
            attributes: vec![
                CompositeAttribute {
                    name: "n".into(),
                    number: 1,
                    ty: ColumnType::Integer,
                    collation_oid: None,
                },
                CompositeAttribute {
                    name: "s".into(),
                    number: 2,
                    ty: ColumnType::Text,
                    collation_oid: None,
                },
            ],
        });
        Self {
            data,
            labels,
            record,
        }
    }
    pub fn case(&self, id: &str) -> &serde_json::Value {
        self.data["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|case| case["id"] == id)
            .unwrap()
    }
    fn rows(&self) -> &[serde_json::Value] {
        self.case("type_metadata")["results"][0]["rows"]
            .as_array()
            .unwrap()
    }
    pub fn metadata(&self, name: &str) -> &serde_json::Value {
        self.rows().iter().find(|row| row[0] == name).unwrap()
    }
    fn endpoint(row: &serde_json::Value) -> ColumnType {
        let oid = integer(&row[3]);
        match row[0].as_str().unwrap() {
            "enum_window" => ColumnType::Enum(EnumTypeReference {
                schema: "public".into(),
                name: "range_mood".into(),
                oid,
                array_oid: 0,
            }),
            "domain_window" => ColumnType::Domain {
                schema: "public".into(),
                name: "range_positive".into(),
                oid,
                array_oid: None,
                collation_oid: None,
                base: Box::new(ColumnType::Integer),
            },
            "text_window" => ColumnType::Text,
            "array_window" => ColumnType::Array(Box::new(ColumnType::Integer)),
            "record_window" => ColumnType::Composite(CompositeTypeReference {
                schema: "public".into(),
                name: "range_pair".into(),
                oid,
                array_oid: 0,
                relation_oid: 0,
            }),
            "reverse_window" => ColumnType::Integer,
            _ => unreachable!(),
        }
    }
}

impl SQLValueCatalog for Catalog {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        if oid == self.record.type_oid {
            return Ok(Some(Self::endpoint(self.metadata("record_window"))));
        }
        for row in self.rows() {
            for (type_index, array_index, multirange) in [(1, 2, false), (6, 7, true)] {
                if oid == integer(&row[type_index]) || oid == integer(&row[array_index]) {
                    let reference = RangeTypeReference {
                        schema: "public".into(),
                        name: row[0].as_str().unwrap().into(),
                        oid: integer(&row[type_index]),
                        array_oid: integer(&row[array_index]),
                        alignment: RangeAlignment::for_endpoint(&Self::endpoint(row)),
                    };
                    let ty = if multirange {
                        ColumnType::DeclaredMultirange(reference)
                    } else {
                        ColumnType::DeclaredRange(reference)
                    };
                    return Ok(Some(if oid == integer(&row[array_index]) {
                        ColumnType::Array(Box::new(ty))
                    } else {
                        ty
                    }));
                }
            }
        }
        Ok(None)
    }
    fn value_range_type(&self, oid: u32) -> Result<Option<Arc<RangeValueDescriptor>>, SQLError> {
        Ok(self
            .rows()
            .iter()
            .find(|row| oid == integer(&row[1]) || oid == integer(&row[6]))
            .map(|row| {
                Arc::new(RangeValueDescriptor {
                    range_oid: integer(&row[1]),
                    multirange_oid: integer(&row[6]),
                    endpoint: Self::endpoint(row),
                })
            }))
    }
    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok((oid == self.record.type_oid).then(|| Arc::clone(&self.record)))
    }
    fn compare_range_bounds(
        &self,
        oid: u32,
        left: &Value,
        right: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<std::cmp::Ordering, SQLError> {
        let row = self
            .rows()
            .iter()
            .find(|row| integer(&row[1]) == oid)
            .unwrap();
        let order = uqa_sql::expr::compare_typed_values_with_enum_catalog(
            left,
            right,
            control,
            Some(self),
            None,
        )?;
        Ok(if row[0] == "reverse_window" {
            order.reverse()
        } else {
            order
        })
    }
    fn range_bound_equality_key(
        &self,
        oid: u32,
        value: &Value,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<Vec<u8>>, SQLError> {
        assert!(self.rows().iter().any(|row| integer(&row[1]) == oid));
        // This finite reference uses canonical integer, text, enum, array and record endpoints. Its independent PostgreSQL equality matrix checks this selected test support function's complete key contract; this is not a general SQL-key implementation.
        let mut writer = KeyWriter {
            bytes: ProductionVec::new(*control),
            failure: None,
        };
        let result = serde_json::to_writer(&mut writer, value);
        if let Some(error) = writer.failure {
            return Err(error.into());
        }
        result.map_err(|error| SQLError::Internal(error.to_string()))?;
        Ok(writer.bytes.finish()?)
    }
    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok((oid == self.labels.type_oid).then(|| Arc::clone(&self.labels)))
    }
    fn enum_value_by_oid(&self, oid: u32) -> Result<Option<EnumValue>, SQLError> {
        Ok(self
            .labels
            .labels
            .iter()
            .find(|label| label.oid == oid)
            .map(|label| {
                EnumValue::new(self.labels.type_oid, label.key.clone()).with_label_oid(Some(oid))
            }))
    }
    fn enum_label_uncommitted(&self, _: u32) -> bool {
        panic!("observation must not repeat input")
    }
    fn enum_type_name(&self, _: u32) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

struct KeyWriter<'a> {
    bytes: ProductionVec<'a, u8>,
    failure: Option<ValueRetentionError>,
}
impl std::io::Write for KeyWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            if let Err(error) = self.bytes.push_copy(*byte) {
                self.failure = Some(error);
                return Err(std::io::Error::other("key production failed"));
            }
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
