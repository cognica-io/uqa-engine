//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent catalog array nodes before and after enum and composite changes.

use super::{deparse, expressions, parse, Field, Routines, SQLError};
use crate::ast::{CompositeTypeReference, EnumTypeReference, Expr, FunctionBinding};
use crate::expr::composites::{
    CompositeAttribute, CompositeConstantSource, CompositeTypeCatalog, CompositeTypeDescriptor,
    DroppedCompositeAttribute,
};
use crate::expr::enums::{EnumTypeLabel, EnumTypeLabels};
use crate::expr::{CatalogInputFunctions, EngineHook, SQLValueCatalog};
use crate::{ColumnType, FunctionTypeResolver, RowSchema};
use serde::Deserialize;
use std::sync::Arc;
use uqa_core::{EnumLabelKey, EnumValue, Value};

#[derive(Deserialize)]
struct Fixture {
    catalog: Vec<Type>,
    changed_catalog: Vec<Type>,
    layout_catalog: Vec<Type>,
    cases: Vec<Case>,
    initial: Vec<Observation>,
    changed: Vec<Observation>,
    layout: Vec<Observation>,
}

#[derive(Deserialize)]
struct Type {
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

#[derive(Deserialize)]
struct Case {
    #[serde(rename = "type")]
    ty: String,
    input: String,
    check: String,
}

#[derive(Deserialize)]
struct Observation {
    name: String,
    conbin: String,
    sql: String,
    pretty: String,
}

struct Catalog<'a>(&'a [Type], bool);

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

    fn named(&self, name: &str) -> Option<ColumnType> {
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
                return Ok(item.name.clone());
            }
            if i64::from(item.array_oid) == oid {
                return Ok(format!("{}[]", item.name));
            }
        }
        deparse::ExpressionNames::type_name(&Routines, oid, modifier)
    }
    fn value_catalog(&self) -> Option<&dyn SQLValueCatalog> {
        Some(self)
    }
}

fn check(observed: &Field, expected: &Observation, catalog: &Catalog<'_>) {
    assert_eq!(
        *observed,
        parse(&expected.conbin).unwrap(),
        "{} node",
        expected.name
    );
    for (pretty, output) in [(false, &expected.sql), (true, &expected.pretty)] {
        assert_eq!(
            deparse::expression(observed, catalog, pretty).unwrap(),
            *output,
            "{} SQL",
            expected.name
        );
    }
}

fn first_constant(field: &Field) -> Option<&super::Node> {
    match field {
        Field::Node(node) if node.kind == "CONST" => Some(node),
        Field::Node(node) => node
            .fields
            .iter()
            .find_map(|(_, field)| first_constant(field)),
        Field::List(fields) => fields.iter().find_map(first_constant),
        _ => None,
    }
}

#[test]
fn physical_array_constants_copy_owned_bytes_without_descriptors() {
    let fixture: Fixture = serde_json::from_str(include_str!("arrays.json")).unwrap();
    let catalog = Catalog(&fixture.catalog, false);
    for observation in &fixture.initial {
        let expression = parse(&observation.conbin).unwrap();
        let node = first_constant(&expression).unwrap();
        let Field::Datum { bytes, .. } = node.field("constvalue").unwrap() else {
            panic!("physical constant")
        };
        let oid = u32::try_from(node.integer("consttype").unwrap()).unwrap();
        let ty = catalog.value_type_by_oid(oid).unwrap().unwrap();
        let original = Value::Datum(uqa_core::DatumValue::new(oid, 0, bytes.clone()));
        let copied = super::values::constant_with_source(&original, &ty, None, None).unwrap();
        assert_eq!(copied, *node, "{} copied Datum", observation.name);
        let mut malformed = copied;
        let (_, layout) = malformed
            .fields
            .iter_mut()
            .find(|(name, _)| name == "constlen")
            .unwrap();
        *layout = 4.into();
        let error = deparse::expression(&malformed.into(), &catalog, false).unwrap_err();
        assert_eq!(error.sqlstate(), Some("XX000"));
    }
}

#[test]
fn named_array_nodes_retain_physical_identity_across_catalog_changes() {
    let fixture: Fixture = serde_json::from_str(include_str!("arrays.json")).unwrap();
    let initial = Catalog(&fixture.catalog, true);
    let changed = Catalog(&fixture.changed_catalog, false);
    let layout = Catalog(&fixture.layout_catalog, false);
    let base = ColumnType::Integer;
    let schema = RowSchema::with_types(vec!["value".into()], vec![Some(base.clone())]);
    for (index, case) in fixture.cases.iter().enumerate() {
        let name = format!("probe_{index}");
        let before = fixture
            .initial
            .iter()
            .find(|value| value.name == name)
            .unwrap();
        let after = fixture
            .changed
            .iter()
            .find(|value| value.name == name)
            .unwrap();
        let crate::Statement::CreateDomain(mut domain) = crate::compile(&format!(
            "CREATE DOMAIN p AS integer CHECK ({})",
            case.check
        ))
        .unwrap()
        .remove(0) else {
            panic!("domain")
        };
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: Some(&base),
            types: Some(&initial),
            routines: &Routines,
        };
        let node = Field::Node(
            context
                .check(&domain.checks[0].expression)
                .unwrap_or_else(|error| panic!("{}: {error}", case.ty)),
        );
        check(&node, before, &initial);
        check(&node, after, &changed);
        let after_layout = fixture
            .layout
            .iter()
            .find(|value| value.name == name)
            .unwrap();
        check(&node, after_layout, &layout);

        let mut ty = initial.named(&case.ty).unwrap();
        while let ColumnType::Domain { base, .. } = ty {
            ty = *base;
        }
        let ty = ty.without_type_modifiers();
        let value = crate::expr::cast_value_with_type_resolution(
            &Value::Str(case.input.clone()),
            Some("unknown"),
            &ty.catalog_name(),
            Some(&initial),
        )
        .unwrap();
        let source =
            CompositeConstantSource::capture(&value, &ty, Some(&initial), Some(&initial)).unwrap();
        let Expr::And(parts) = &mut domain.checks[0].expression else {
            panic!("AND")
        };
        let Expr::IsNull { expr, .. } = &mut parts[1] else {
            panic!("NULL test")
        };
        let Expr::Cast { expr, .. } = expr.as_mut() else {
            panic!("cast")
        };
        // The projected value is deliberately different: only the retained source may supply bytes.
        **expr = Expr::TypedLiteral {
            value: Value::Null,
            ty: ty.catalog_name(),
            composite_source: Some(Box::new(source)),
        };
        let context = expressions::ExpressionContext {
            types: Some(&changed),
            ..context
        };
        let retained = Field::Node(context.check(&domain.checks[0].expression).unwrap());
        check(&retained, after, &changed);
        let context = expressions::ExpressionContext {
            types: Some(&layout),
            ..context
        };
        let retained = Field::Node(context.check(&domain.checks[0].expression).unwrap());
        check(&retained, after_layout, &layout);
    }
}
