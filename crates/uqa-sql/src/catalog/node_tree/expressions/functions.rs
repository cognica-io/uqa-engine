//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Keep a routine's declared identity separate from its supplied call arguments.

use super::{
    ColumnType, Expr, ExpressionContext, Field, FunctionBinding, Node, RoutineIdentity, SQLError,
    TypedNode,
};
use crate::ast::{
    FunctionCallSyntax, FunctionDispatch, RoutineInvocationBinding, RoutineVariadicMode,
};
use crate::catalog::type_metadata::{pg_type_collation_oid, pg_type_oid};
use crate::expr::{call_argument_name, call_argument_value, variadic_argument_value};
use crate::type_resolution::RoutineCallDescriptor;

impl RoutineIdentity {
    /// A fixed positional signature with no defaults or variadic expansion.
    #[must_use]
    pub fn positional(oid: i64, argument_types: Vec<ColumnType>, result_type: ColumnType) -> Self {
        let names = argument_types
            .iter()
            .map(ColumnType::catalog_name)
            .collect::<Vec<_>>();
        Self {
            oid,
            invocation: RoutineInvocationBinding {
                argument_positions: (0..names.len()).collect(),
                argument_targets: names.clone(),
                argument_sources: Vec::new(),
                parameter_types: names,
                return_type: Some(result_type.catalog_name()),
                variadic_mode: RoutineVariadicMode::None,
            },
            result_type,
        }
    }
}

impl ExpressionContext<'_> {
    pub(super) fn function(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        arguments: &[Expr],
        syntax: FunctionCallSyntax,
    ) -> Result<TypedNode, SQLError> {
        let types = arguments
            .iter()
            .map(|arg| self.expression_type(call_argument_value(arg)))
            .collect::<Result<Vec<_>, _>>()?;
        let names = arguments
            .iter()
            .map(|arg| call_argument_name(arg).map(|name| name.map(str::to_owned)))
            .collect::<Result<Vec<_>, _>>()?;
        let helper = if let Some(dispatch @ FunctionDispatch::PatternEscape { similar }) =
            binding.and_then(|binding| binding.dispatch)
        {
            if !(1..=2).contains(&types.len()) {
                return Err(SQLError::Internal("invalid pattern escape operands".into()));
            }
            let inputs = [
                types.first().and_then(Option::as_ref),
                types.get(1).and_then(Option::as_ref),
            ];
            let ty = crate::type_resolution::pattern::select_escape(
                similar,
                &inputs[..types.len()],
                &uqa_core::memory::ProductionControl::uncontrolled(),
            )?;
            let mut selected = FunctionBinding::dispatched(dispatch);
            selected.argument_types = vec![ty.sql_name(); types.len()];
            Some(selected)
        } else {
            None
        };
        let name = helper
            .as_ref()
            .map_or(name, |binding| binding.name.as_str());
        let routine = self.routines.resolve(
            name,
            helper.as_ref().or(binding),
            RoutineCallDescriptor {
                argument_names: &names,
                argument_types: &types,
                explicit_variadic: arguments
                    .last()
                    .is_some_and(|arg| variadic_argument_value(arg).is_some()),
            },
        )?;
        let (arguments, variadic) =
            self.routine_arguments(arguments, &names, &routine.invocation)?;
        Ok(TypedNode {
            node: Node::new(
                "FUNCEXPR",
                [
                    ("funcid", routine.oid.into()),
                    ("funcresulttype", pg_type_oid(&routine.result_type).into()),
                    ("funcretset", false.into()),
                    ("funcvariadic", variadic.into()),
                    (
                        "funcformat",
                        if syntax == FunctionCallSyntax::Extract {
                            3.into()
                        } else {
                            0.into()
                        },
                    ),
                    (
                        "funccollid",
                        pg_type_collation_oid(&routine.result_type).into(),
                    ),
                    ("inputcollid", 0.into()),
                    ("args", Field::List(arguments)),
                    ("location", (-1).into()),
                ],
            ),
            ty: Some(routine.result_type),
        })
    }

    fn routine_arguments(
        &self,
        arguments: &[Expr],
        names: &[Option<String>],
        invocation: &RoutineInvocationBinding,
    ) -> Result<(Vec<Field>, bool), SQLError> {
        if invocation.argument_targets.len() != arguments.len()
            || invocation.argument_positions.len() != arguments.len()
        {
            return Err(SQLError::Internal(
                "catalog invocation differs from supplied arguments".into(),
            ));
        }
        let packed = match invocation.variadic_mode {
            RoutineVariadicMode::Expanded { parameter_index }
                if invocation
                    .parameter_types
                    .get(parameter_index)
                    .is_some_and(|ty| ty != "any") =>
            {
                Some(parameter_index)
            }
            _ => None,
        };
        let mut fields = Vec::with_capacity(arguments.len());
        let mut elements = Vec::new();
        for (index, argument) in arguments.iter().enumerate() {
            let position = invocation.argument_positions[index];
            if position >= invocation.parameter_types.len() {
                return Err(SQLError::Internal(
                    "invalid catalog invocation parameter".into(),
                ));
            }
            let value = call_argument_value(argument);
            let target = &invocation.argument_targets[index];
            let node = if target == "unknown" {
                let Expr::Literal(value) = value else {
                    return Err(SQLError::Internal(
                        "unknown routine argument is not a literal".into(),
                    ));
                };
                super::values::unknown_constant(value)?
            } else {
                self.encode(value, Some(&self.resolve_type(target)?))?.node
            };
            if packed == Some(position) {
                if names[index].is_some() {
                    return Err(SQLError::Internal(
                        "expanded variadic argument has a name".into(),
                    ));
                }
                elements.push(node.into());
            } else if let Some(name) = &names[index] {
                fields.push(named_argument(node, name, position)?.into());
            } else {
                fields.push(node.into());
            }
        }
        if let Some(position) = packed {
            let ty = self.resolve_type(&invocation.parameter_types[position])?;
            fields.push(variadic_array(&ty, elements)?.into());
        }
        Ok((
            fields,
            packed.is_some()
                || matches!(
                    invocation.variadic_mode,
                    RoutineVariadicMode::Explicit { .. }
                ),
        ))
    }
}

fn named_argument(node: Node, name: &str, position: usize) -> Result<Node, SQLError> {
    let position = i64::try_from(position)
        .map_err(|_| super::super::invalid("routine argument position exceeds catalog integer"))?;
    Ok(Node::new(
        "NAMEDARGEXPR",
        [
            ("arg", node.into()),
            ("name", Field::Atom(name.into())),
            ("argnumber", position.into()),
            ("location", (-1).into()),
        ],
    ))
}

fn variadic_array(ty: &ColumnType, elements: Vec<Field>) -> Result<Node, SQLError> {
    let ColumnType::Array(element) = ty else {
        return Err(SQLError::Internal(
            "expanded routine parameter is not an array".into(),
        ));
    };
    Ok(Node::new(
        "ARRAYEXPR",
        [
            ("array_typeid", pg_type_oid(ty).into()),
            ("array_collid", pg_type_collation_oid(ty).into()),
            ("element_typeid", pg_type_oid(element).into()),
            ("elements", Field::List(elements)),
            ("multidims", false.into()),
            ("list_start", (-1).into()),
            ("list_end", (-1).into()),
            ("location", (-1).into()),
        ],
    ))
}
