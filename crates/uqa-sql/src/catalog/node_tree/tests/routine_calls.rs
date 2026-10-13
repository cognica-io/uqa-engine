//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independent declared signatures, supplied call nodes and SQL spellings.

use super::{deparse, expressions, parse, Field, Routines};
use crate::{
    ast::FunctionBinding,
    catalog::type_metadata,
    schema::ColumnTypeSchema,
    type_resolution::{RoutineCallDescriptor, RoutineParameterDescriptor},
    ColumnType, FunctionTypeResolver, SQLError, Statement,
};
use expressions::{ExpressionRoutines, RoutineDeclaration, RoutineIdentity};

struct Calls(Vec<(String, RoutineDeclaration)>);

impl Calls {
    fn captured(fixture: &serde_json::Value) -> Self {
        Self(
            fixture["routines"]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| {
                    let types = row["proargtypes"]
                        .as_str()
                        .unwrap()
                        .split(' ')
                        .map(|oid| oid.parse::<i64>().unwrap())
                        .collect::<Vec<_>>();
                    let names = row["proargnames"]
                        .as_str()
                        .map(|names| {
                            names
                                .trim_matches(['{', '}'])
                                .split(',')
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default();
                    let defaults = row["pronargdefaults"]
                        .as_str()
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    let variadic = row["provariadic"].as_str().unwrap() != "0";
                    let parameters = types
                        .iter()
                        .enumerate()
                        .map(|(index, &oid)| {
                            let column_type =
                                type_metadata::builtin_scalar_type(u32::try_from(oid).unwrap())
                                    .cloned()
                                    .or_else(|| {
                                        type_metadata::builtin_array_element(
                                            u32::try_from(oid).unwrap(),
                                        )
                                        .cloned()
                                        .map(|ty| ColumnType::Array(Box::new(ty)))
                                    });
                            RoutineParameterDescriptor {
                                name: names.get(index).map(|name| (*name).to_owned()),
                                type_name: type_metadata::routine_pseudo_type_name(oid)
                                    .map_or_else(
                                        || column_type.as_ref().unwrap().catalog_name(),
                                        str::to_owned,
                                    ),
                                column_type,
                                has_default: index >= types.len() - defaults,
                                default_type: None,
                                variadic: variadic && index + 1 == types.len(),
                            }
                        })
                        .collect();
                    let oid = row["prorettype"].as_str().unwrap().parse::<i64>().unwrap();
                    let return_type = type_metadata::routine_pseudo_type_name(oid).map_or_else(
                        || {
                            type_metadata::builtin_scalar_type(u32::try_from(oid).unwrap())
                                .unwrap()
                                .catalog_name()
                        },
                        str::to_owned,
                    );
                    (
                        row["proname"].as_str().unwrap().to_owned(),
                        RoutineDeclaration {
                            oid: row["oid"].as_str().unwrap().parse().unwrap(),
                            parameters,
                            return_type,
                        },
                    )
                })
                .collect(),
        )
    }
}

impl ExpressionRoutines for Calls {
    fn resolve(
        &self,
        name: &str,
        _: Option<&FunctionBinding>,
        call: RoutineCallDescriptor<'_>,
    ) -> Result<RoutineIdentity, SQLError> {
        let declaration = &self
            .0
            .iter()
            .find(|(candidate, _)| candidate == name)
            .expect("captured routine")
            .1;
        expressions::select_routine(name, std::slice::from_ref(declaration), call)
    }
}

impl FunctionTypeResolver for Calls {
    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        self.resolve(
            name,
            binding,
            RoutineCallDescriptor {
                argument_names: names,
                argument_types: types,
                explicit_variadic: variadic,
            },
        )
        .map(|routine| Some(routine.result_type))
    }
}

impl deparse::ExpressionNames for Calls {
    fn column(&self, attribute: i64) -> Result<String, SQLError> {
        Ok(match attribute {
            1 => "n",
            2 => "a",
            _ => panic!("fixture column"),
        }
        .into())
    }
    fn routine(&self, oid: i64) -> Result<Vec<String>, SQLError> {
        Ok(vec![self
            .0
            .iter()
            .find(|(_, routine)| routine.oid == oid)
            .unwrap()
            .0
            .clone()])
    }
    fn type_name(&self, oid: i64, modifier: i64) -> Result<String, SQLError> {
        deparse::ExpressionNames::type_name(&Routines, oid, modifier)
    }
}

#[test]
fn supplied_routine_nodes_match_postgresql() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("routine_calls.json")).unwrap();
    let calls = Calls::captured(&fixture);
    for (create, cases, check) in [
        ("create_checks", "checks", true),
        ("create_shapes", "shapes", true),
        ("create_defaults", "defaults", false),
    ] {
        let Statement::CreateTable(table) = crate::compile(fixture[create].as_str().unwrap())
            .unwrap()
            .remove(0)
        else {
            panic!("fixture table")
        };
        let schema = ColumnTypeSchema::new(&table.columns);
        let context = expressions::ExpressionContext {
            schema: &schema,
            domain_value: None,
            types: Some(&calls),
            routines: &calls,
        };
        for case in fixture[cases].as_array().unwrap() {
            let name = case["name"].as_str().unwrap();
            let node = if check {
                let constraint = table
                    .checks
                    .iter()
                    .find(|check| check.name.as_deref() == Some(name))
                    .unwrap();
                context.check(&constraint.expr).unwrap()
            } else {
                let column = table
                    .columns
                    .iter()
                    .find(|column| column.name == name)
                    .unwrap();
                context
                    .expression(column.default.as_ref().unwrap(), Some(&column.ty))
                    .unwrap()
            };
            let actual = Field::Node(node);
            assert_eq!(
                actual,
                parse(case["node"].as_str().unwrap()).unwrap(),
                "{name}"
            );
            for pretty in [false, true] {
                let expected = case[if pretty && check { "pretty" } else { "sql" }]
                    .as_str()
                    .unwrap();
                assert_eq!(
                    deparse::expression(&actual, &calls, pretty).unwrap(),
                    expected,
                    "{name}"
                );
            }
        }
    }
}
