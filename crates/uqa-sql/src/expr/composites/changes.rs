//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Stored composite values follow their type's attributes. `PostgreSQL` stores fields by attribute number, so adding, dropping and renaming an attribute changes no stored tuple; a value here carries its type's current attribute names, so each change rewrites the values of the type wherever a declared type nests it: directly, in arrays, under domains and inside other composite types.

use uqa_core::{ArrayValue, Value};

use super::{descriptor, CompositeTypeCatalog, Result, SQLError};
use crate::ast::ColumnType;

/// A change to one composite type's attributes, applied to the type's values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeChange {
    /// A new last attribute, which existing values hold as NULL.
    Add(String),
    Drop(String),
    Rename {
        from: String,
        to: String,
    },
    Type {
        name: String,
        from: Box<ColumnType>,
        to: Box<ColumnType>,
    },
}

impl AttributeChange {
    fn apply(&self, fields: &mut Vec<(String, Value)>) {
        match self {
            Self::Add(name) => fields.push((name.clone(), Value::Null)),
            Self::Drop(name) => fields.retain(|(field, _)| field != name),
            Self::Rename { from, to } => {
                for (field, _) in fields.iter_mut() {
                    if field == from {
                        field.clone_from(to);
                    }
                }
            }
            Self::Type { name, from, to } => {
                if let Some((_, value)) = fields.iter_mut().find(|(field, _)| field == name) {
                    if let Some(projected) = super::datum::reinterpret(value, from, to) {
                        *value = projected;
                    }
                }
            }
        }
    }
}

/// Whether values of `ty` can hold values of the composite type `target`.
pub fn type_contains_composite(
    ty: &ColumnType,
    target: u32,
    catalog: &dyn CompositeTypeCatalog,
) -> Result<bool> {
    Ok(match ty {
        ColumnType::Domain { base, .. } | ColumnType::Array(base) => {
            type_contains_composite(base, target, catalog)?
        }
        ColumnType::Composite(reference) => {
            if reference.oid == target {
                return Ok(true);
            }
            let descriptor = descriptor(Some(catalog), reference.oid)?;
            for attribute in &descriptor.attributes {
                if type_contains_composite(&attribute.ty, target, catalog)? {
                    return Ok(true);
                }
            }
            false
        }
        _ => false,
    })
}

/// Apply `change` to every value of the composite type `target` in `value`, which has the declared type `ty`. Nested composite values are rewritten through their types' attributes before the change.
pub fn apply_attribute_change(
    value: Value,
    ty: &ColumnType,
    target: u32,
    change: &AttributeChange,
    catalog: &dyn CompositeTypeCatalog,
) -> Result<Value> {
    match (ty, value) {
        (_, Value::Null) => Ok(Value::Null),
        (ColumnType::Domain { base, .. }, value) => {
            apply_attribute_change(value, base, target, change, catalog)
        }
        (ColumnType::Array(element), Value::Array(array)) => {
            let lower_bounds = array.lower_bounds().to_vec();
            let elements = array
                .elements()
                .iter()
                .cloned()
                .map(|value| apply_array_element(value, element, target, change, catalog))
                .collect::<Result<Vec<_>>>()?;
            ArrayValue::with_lower_bounds(elements, lower_bounds)
                .map(Value::Array)
                .ok_or_else(|| {
                    SQLError::Internal("composite attribute change reshaped an array".into())
                })
        }
        (ColumnType::Composite(reference), Value::Record(fields)) => {
            let descriptor = descriptor(Some(catalog), reference.oid)?;
            let mut fields = fields
                .into_iter()
                .map(|(name, value)| {
                    let value = match descriptor.attribute(&name) {
                        Some((_, attribute)) => {
                            apply_attribute_change(value, &attribute.ty, target, change, catalog)?
                        }
                        None => value,
                    };
                    Ok((name, value))
                })
                .collect::<Result<Vec<_>>>()?;
            if reference.oid == target {
                change.apply(&mut fields);
            }
            Ok(Value::Record(fields))
        }
        (_, value) => Ok(value),
    }
}

/// Multidimensional arrays nest their inner dimensions as lists of elements.
fn apply_array_element(
    value: Value,
    element: &ColumnType,
    target: u32,
    change: &AttributeChange,
    catalog: &dyn CompositeTypeCatalog,
) -> Result<Value> {
    match value {
        Value::List(values) => values
            .into_iter()
            .map(|value| apply_array_element(value, element, target, change, catalog))
            .collect::<Result<Vec<_>>>()
            .map(Value::List),
        value => apply_attribute_change(value, element, target, change, catalog),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use uqa_core::{ArrayValue, Value};

    use super::{apply_attribute_change, type_contains_composite, AttributeChange};
    use crate::ast::{ColumnType, CompositeTypeReference};
    use crate::expr::composites::{
        CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor,
    };

    struct Catalog(BTreeMap<u32, Arc<CompositeTypeDescriptor>>);

    impl CompositeTypeCatalog for Catalog {
        fn composite_type(
            &self,
            type_oid: u32,
        ) -> crate::error::Result<Option<Arc<CompositeTypeDescriptor>>> {
            Ok(self.0.get(&type_oid).cloned())
        }
    }

    fn reference(oid: u32, name: &str) -> ColumnType {
        ColumnType::Composite(CompositeTypeReference {
            schema: "public".into(),
            name: name.into(),
            oid,
            array_oid: oid + 1,
            relation_oid: oid - 1,
        })
    }

    fn catalog() -> Catalog {
        let attribute = |name: &str, ty: ColumnType, number: i16| CompositeAttribute {
            name: name.into(),
            ty,
            number,
        };
        Catalog(BTreeMap::from([
            (
                20_002,
                Arc::new(CompositeTypeDescriptor {
                    dropped: Vec::new(),
                    type_oid: 20_002,
                    relation_oid: 20_001,
                    attributes: vec![
                        attribute("x", ColumnType::Integer, 1),
                        attribute("y", ColumnType::Text, 2),
                    ],
                }),
            ),
            (
                20_012,
                Arc::new(CompositeTypeDescriptor {
                    dropped: Vec::new(),
                    type_oid: 20_012,
                    relation_oid: 20_011,
                    attributes: vec![
                        attribute("p", reference(20_002, "pair"), 1),
                        attribute(
                            "ps",
                            ColumnType::Array(Box::new(reference(20_002, "pair"))),
                            2,
                        ),
                    ],
                }),
            ),
        ]))
    }

    fn pair(x: i64, y: &str) -> Value {
        Value::Record(vec![
            ("x".into(), Value::Int(x)),
            ("y".into(), Value::Str(y.into())),
        ])
    }

    fn nested_value() -> Value {
        Value::Record(vec![
            ("p".into(), pair(1, "a")),
            (
                "ps".into(),
                Value::Array(ArrayValue::try_new(vec![pair(2, "b"), Value::Null]).unwrap()),
            ),
        ])
    }

    #[test]
    fn renames_follow_the_type_through_nested_composites_arrays_and_domains() {
        let catalog = catalog();
        let outer = reference(20_012, "outer_t");
        let column = ColumnType::Domain {
            schema: "public".into(),
            name: "outer_domain".into(),
            oid: 20_020,
            array_oid: None,
            base: Box::new(outer.clone()),
        };
        assert!(type_contains_composite(&column, 20_002, &catalog).unwrap());
        assert!(!type_contains_composite(&ColumnType::Integer, 20_002, &catalog).unwrap());
        let renamed = apply_attribute_change(
            nested_value(),
            &column,
            20_002,
            &AttributeChange::Rename {
                from: "y".into(),
                to: "label".into(),
            },
            &catalog,
        )
        .unwrap();
        let expected_pair = |x: i64, label: &str| {
            Value::Record(vec![
                ("x".into(), Value::Int(x)),
                ("label".into(), Value::Str(label.into())),
            ])
        };
        assert_eq!(
            renamed,
            Value::Record(vec![
                ("p".into(), expected_pair(1, "a")),
                (
                    "ps".into(),
                    Value::Array(
                        ArrayValue::try_new(vec![expected_pair(2, "b"), Value::Null]).unwrap()
                    ),
                ),
            ])
        );
    }

    #[test]
    fn drops_and_additions_change_only_values_of_the_changed_type() {
        let catalog = catalog();
        let outer = reference(20_012, "outer_t");
        let dropped = apply_attribute_change(
            nested_value(),
            &outer,
            20_002,
            &AttributeChange::Drop("x".into()),
            &catalog,
        )
        .unwrap();
        let Value::Record(fields) = &dropped else {
            panic!("composite value");
        };
        assert_eq!(
            fields[0].1,
            Value::Record(vec![("y".into(), Value::Str("a".into()))])
        );
        let added = apply_attribute_change(
            pair(3, "c"),
            &reference(20_002, "pair"),
            20_002,
            &AttributeChange::Add("z".into()),
            &catalog,
        )
        .unwrap();
        assert_eq!(
            added,
            Value::Record(vec![
                ("x".into(), Value::Int(3)),
                ("y".into(), Value::Str("c".into())),
                ("z".into(), Value::Null),
            ])
        );
        // The outer type's own change leaves nested values of other types alone.
        let outer_change = apply_attribute_change(
            nested_value(),
            &outer,
            20_012,
            &AttributeChange::Drop("ps".into()),
            &catalog,
        )
        .unwrap();
        assert_eq!(
            outer_change,
            Value::Record(vec![("p".into(), pair(1, "a"))])
        );
        assert_eq!(
            apply_attribute_change(
                Value::Null,
                &outer,
                20_002,
                &AttributeChange::Add("z".into()),
                &catalog
            )
            .unwrap(),
            Value::Null
        );
    }
}
