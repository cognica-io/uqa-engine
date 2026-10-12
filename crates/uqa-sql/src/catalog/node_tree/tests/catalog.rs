//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Borrowed type metadata from independent catalog-node references.

use super::{deparse, Routines};
use crate::ast::{CompositeTypeReference, EnumTypeReference, FunctionBinding};
use crate::expr::composites::{
    CompositeAttribute, CompositeTypeCatalog, CompositeTypeDescriptor, DroppedCompositeAttribute,
};
use crate::expr::enums::{EnumTypeLabel, EnumTypeLabels};
use crate::expr::{CatalogInputFunctions, EngineHook, SQLValueCatalog};
use crate::{ColumnType, FunctionTypeResolver, SQLError};
use serde::Deserialize;
use std::sync::Arc;
use uqa_core::{EnumLabelKey, EnumValue, Value};

#[derive(Deserialize)]
pub(super) struct Type {
    oid: u32,
    name: String,
    array_oid: u32,
    relation_oid: u32,
    kind: String,
    base: Option<String>,
    labels: Option<Vec<Label>>,
    attributes: Option<Vec<Attribute>>,
}

#[derive(Deserialize)]
struct Label {
    oid: u32,
    label: String,
}

#[derive(Deserialize)]
struct Attribute {
    name: String,
    number: i16,
    #[serde(rename = "type")]
    ty: String,
    dropped: bool,
    length: i64,
    alignment: String,
}

pub(super) struct Catalog<'a>(pub(super) &'a [Type], pub(super) bool);

impl Catalog<'_> {
    fn declared(&self, item: &Type) -> ColumnType {
        match item.kind.as_str() {
            "e" => ColumnType::Enum(EnumTypeReference {
                schema: "public".into(),
                name: item.name.clone(),
                oid: item.oid,
                array_oid: item.array_oid,
            }),
            "c" => ColumnType::Composite(CompositeTypeReference {
                schema: "public".into(),
                name: item.name.clone(),
                oid: item.oid,
                array_oid: item.array_oid,
                relation_oid: item.relation_oid,
            }),
            "d" => ColumnType::Domain {
                schema: "public".into(),
                name: item.name.clone(),
                oid: item.oid,
                array_oid: Some(item.array_oid),
                base: Box::new(self.named(item.base.as_ref().unwrap()).unwrap()),
            },
            kind => panic!("unexpected captured type kind {kind}"),
        }
    }

    pub(super) fn named(&self, name: &str) -> Option<ColumnType> {
        if let Some(element) = name.strip_suffix("[]") {
            return self
                .named(element)
                .map(|ty| ColumnType::Array(Box::new(ty)));
        }
        if let Some(identity) = crate::ast::UserTypeIdentity::parse(name) {
            return self
                .0
                .iter()
                .find(|item| item.oid == identity.oid)
                .map(|item| self.declared(item));
        }
        let original = name;
        let name = name.trim_start_matches("public.").trim_matches('"');
        self.0
            .iter()
            .find(|item| item.name == name)
            .map(|item| self.declared(item))
            .or_else(|| {
                ColumnType::from_sql_name(original)
                    .ok()
                    .filter(|ty| !matches!(ty, ColumnType::Named(_)))
            })
    }
}

impl SQLValueCatalog for Catalog<'_> {
    fn value_type_by_oid(&self, oid: u32) -> Result<Option<ColumnType>, SQLError> {
        Ok(self.0.iter().find_map(|item| {
            if item.oid == oid {
                Some(self.declared(item))
            } else if item.array_oid == oid {
                Some(ColumnType::Array(Box::new(self.declared(item))))
            } else {
                None
            }
        }))
    }

    fn value_composite_type(
        &self,
        oid: u32,
    ) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        CompositeTypeCatalog::composite_type(self, oid)
    }

    fn enum_type_labels(&self, oid: u32) -> Result<Option<Arc<EnumTypeLabels>>, SQLError> {
        Ok(self
            .0
            .iter()
            .find(|item| item.oid == oid)
            .and_then(|item| item.labels.as_ref())
            .map(|labels| {
                Arc::new(EnumTypeLabels {
                    type_oid: oid,
                    labels: labels
                        .iter()
                        .enumerate()
                        .map(|(index, label)| EnumTypeLabel {
                            oid: label.oid,
                            key: EnumLabelKey::from_bytes(vec![u8::try_from(index + 1).unwrap()])
                                .unwrap(),
                            label: label.label.clone(),
                        })
                        .collect(),
                })
            }))
    }

    fn enum_value_by_oid(&self, oid: u32) -> Result<Option<EnumValue>, SQLError> {
        for item in self.0 {
            if let Some(labels) = self.enum_type_labels(item.oid)? {
                if let Some(label) = labels.labels.iter().find(|label| label.oid == oid) {
                    return Ok(Some(
                        EnumValue::new(item.oid, label.key.clone()).with_label_oid(Some(oid)),
                    ));
                }
            }
        }
        Ok(None)
    }

    fn enum_label_uncommitted(&self, _: u32) -> bool {
        assert!(self.1, "admitted enum output must not repeat input checks");
        false
    }
    fn enum_type_name(&self, oid: u32) -> Result<Option<String>, SQLError> {
        Ok(self
            .0
            .iter()
            .find(|item| item.oid == oid)
            .map(|item| item.name.clone()))
    }
    fn has_enum_types(&self) -> bool {
        true
    }
}

impl CompositeTypeCatalog for Catalog<'_> {
    fn composite_type(&self, oid: u32) -> Result<Option<Arc<CompositeTypeDescriptor>>, SQLError> {
        Ok(self
            .0
            .iter()
            .find(|item| item.oid == oid && item.kind == "c")
            .map(|item| {
                Arc::new(CompositeTypeDescriptor {
                    type_oid: oid,
                    relation_oid: item.relation_oid,
                    dropped: item
                        .attributes
                        .as_ref()
                        .unwrap()
                        .iter()
                        .filter(|attr| attr.dropped)
                        .map(|attr| DroppedCompositeAttribute {
                            number: attr.number,
                            length: attr.length,
                            alignment: attr.alignment.as_bytes()[0],
                        })
                        .collect(),
                    attributes: item
                        .attributes
                        .as_ref()
                        .unwrap()
                        .iter()
                        .filter(|attr| !attr.dropped)
                        .map(|attr| CompositeAttribute {
                            name: attr.name.clone(),
                            number: attr.number,
                            ty: self.named(&attr.ty).unwrap(),
                        })
                        .collect(),
                })
            }))
    }
}

impl EngineHook for Catalog<'_> {
    fn cast_domain(
        &self,
        value: &Value,
        source: Option<&str>,
        target: &ColumnType,
    ) -> Result<Option<Value>, SQLError> {
        let ColumnType::Domain { base, .. } = target else {
            return Ok(None);
        };
        assert!(self.1, "admitted values must not repeat domain input");
        crate::expr::cast_value_with_type_resolution(
            value,
            source,
            &base.catalog_name(),
            Some(self),
        )
        .map(Some)
    }
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        unreachable!()
    }
    fn enum_labels(&self) -> Option<&dyn SQLValueCatalog> {
        Some(self)
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, String> {
        Ok(self.named(name))
    }
    fn resolve_type_oid(&self, oid: u32) -> Result<Option<ColumnType>, String> {
        self.value_type_by_oid(oid)
            .map_err(|error| error.to_string())
    }
}

impl CatalogInputFunctions for Catalog<'_> {
    fn read_unknown_input(&self, text: &str, target: &ColumnType) -> Result<Value, SQLError> {
        assert!(self.1, "retained constants must not invoke type input");
        crate::expr::read_catalog_input(text, target, self)
    }
}

impl FunctionTypeResolver for Catalog<'_> {
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        Ok(self.named(name))
    }
    fn enum_labels(&self) -> Option<&dyn SQLValueCatalog> {
        Some(self)
    }
    fn composite_types(&self) -> Option<&dyn CompositeTypeCatalog> {
        Some(self)
    }
    fn catalog_input_functions(&self) -> Option<&dyn CatalogInputFunctions> {
        Some(self)
    }
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}

impl deparse::ExpressionNames for Catalog<'_> {
    fn column(&self, _: i64) -> Result<String, SQLError> {
        unreachable!()
    }
    fn routine(&self, _: i64) -> Result<Vec<String>, SQLError> {
        unreachable!()
    }
    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        for item in self.0 {
            if i64::from(item.oid) == oid {
                return Ok(crate::expr::quote_ident(&item.name));
            }
            if i64::from(item.array_oid) == oid {
                return Ok(format!("{}[]", crate::expr::quote_ident(&item.name)));
            }
        }
        deparse::ExpressionNames::type_name(&Routines, oid, modifier)
    }
    fn value_catalog(&self) -> Option<&dyn SQLValueCatalog> {
        Some(self)
    }
}
