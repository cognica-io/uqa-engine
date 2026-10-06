//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordinary WITHIN GROUP lookup keeps declared signatures and modifier checks separate from input coercion.

mod catalog;

use crate::ast::{ColumnType, FunctionBinding, FunctionOrderSyntax};
use crate::type_resolution::{BuiltinFunctionOverload, ResolvedFunctionOverload};
use crate::{FunctionTypeResolver, SQLError};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Ordinary,
    Aggregate,
    OrderedSet { direct: usize, polymorphic: bool },
    Hypothetical,
    Window,
}

pub(super) fn uses_ordered_arguments(
    syntax: FunctionOrderSyntax,
    name: &str,
    binding: Option<&FunctionBinding>,
    ordered: usize,
) -> bool {
    syntax == FunctionOrderSyntax::WithinGroup
        || (syntax == FunctionOrderSyntax::Legacy
            && ordered > 0
            && binding.is_none_or(|binding| binding.builtin)
            && is_ordered_set(name))
}

pub(super) fn is_ordered_set(name: &str) -> bool {
    matches!(
        crate::semantics::builtin_function_dispatch_name(name).as_str(),
        "mode" | "percentile_cont" | "percentile_disc"
    )
}

/// Resolve declared signatures together with SQL catalog routines. Ordered inputs
/// are already in `types`; they remain separate in the actual expression tree.
pub(super) fn resolve(
    name: &str,
    binding: Option<&FunctionBinding>,
    names: &[Option<String>],
    types: &[Option<ColumnType>],
    variadic: bool,
    resolver: &dyn FunctionTypeResolver,
) -> Result<Option<(ResolvedFunctionOverload, Kind)>, SQLError> {
    let lower = crate::semantics::builtin_function_dispatch_name(name);
    let mut declarations = Vec::new();
    let mut kinds = Vec::new();
    for declaration in catalog::SIGNATURES.lines() {
        let mut fields = declaration.split('|');
        if fields.next() != Some(lower.as_str()) {
            continue;
        }
        let [arguments_text, result, proc_kind, aggregate_kind, direct] =
            std::array::from_fn(|_| fields.next().expect("catalog signature field"));
        let mut arguments = if arguments_text.is_empty() {
            Vec::new()
        } else {
            arguments_text
                .split(", ")
                .map(parse_type)
                .collect::<Result<Vec<_>, _>>()?
        };
        let kind = match (proc_kind, aggregate_kind) {
            ("w", _) => Kind::Window,
            (_, "o") => Kind::OrderedSet {
                direct: direct.parse().expect("catalog direct count"),
                polymorphic: arguments_text.contains("anyelement"),
            },
            (_, "h") => {
                if types.is_empty() {
                    continue;
                }
                arguments.resize(types.len(), ColumnType::Named("any".into()));
                Kind::Hypothetical
            }
            _ => Kind::Aggregate,
        };
        declarations.push(BuiltinFunctionOverload {
            name: format!("pg_catalog.{lower}"),
            argument_names: vec![None; arguments.len()],
            argument_types: arguments,
            default_arguments: 0,
            return_type: parse_type(result)?,
        });
        kinds.push(kind);
    }
    // These ordinary text declarations share the same candidate ranking; no
    // unknown input is read until the selected call has passed its modifiers.
    if matches!(lower.as_str(), "lower" | "upper" | "initcap") {
        declarations.push(BuiltinFunctionOverload {
            name: format!("pg_catalog.{lower}"),
            argument_names: vec![None],
            argument_types: vec![ColumnType::Text],
            default_arguments: 0,
            return_type: ColumnType::Text,
        });
        kinds.push(Kind::Ordinary);
    }
    if declarations.is_empty() {
        return Ok(None);
    }
    let selected = resolver
        .resolve_function_overload_with_builtins(
            name,
            binding,
            names,
            types,
            variadic,
            &declarations,
        )?
        .map_or_else(
            || {
                crate::type_resolution::resolve_local_builtin_overload(
                    name,
                    binding,
                    names,
                    types,
                    &declarations,
                )
            },
            Ok,
        )?;
    let kind = if selected.binding.builtin {
        declarations
            .iter()
            .zip(kinds)
            .find(|(declaration, _)| {
                crate::type_resolution::builtin_binding_matches(declaration, &selected.binding)
            })
            .map(|(_, kind)| kind)
            .ok_or_else(|| SQLError::Internal("selected aggregate lost its declaration".into()))?
    } else {
        Kind::Ordinary
    };
    Ok(Some((selected, kind)))
}

fn parse_type(name: &str) -> Result<ColumnType, SQLError> {
    if is_polymorphic(name) || matches!(name, "money" | "pg_lsn" | "tid" | "xid8") {
        Ok(ColumnType::Named(name.into()))
    } else {
        ColumnType::from_sql_name(name)
    }
}

pub(super) fn is_polymorphic(name: &str) -> bool {
    matches!(
        name,
        "any" | "anyelement" | "anyarray" | "anynonarray" | "anyenum" | "anycompatible"
    )
}

#[derive(Clone, Copy)]
pub(super) struct Modifiers {
    pub direct: usize,
    pub ordered: usize,
    pub within_group: bool,
    pub distinct: bool,
    pub filtered: bool,
}

pub(super) fn validate(
    name: &str,
    kind: Kind,
    modifiers: Modifiers,
    names: &[Option<String>],
    types: &[Option<ColumnType>],
) -> Result<(), SQLError> {
    let Modifiers {
        direct,
        ordered,
        within_group,
        distinct,
        filtered,
    } = modifiers;
    let wrong = |message| SQLError::Routine {
        sqlstate: "42809".into(),
        message,
    };
    match kind {
        Kind::Ordinary => {
            let modifier = if distinct { Some("DISTINCT") }
            else if within_group { Some("WITHIN GROUP") }
            else if ordered > 0 { Some("ORDER BY") }
            else if filtered { Some("FILTER") } else { None };
            if let Some(modifier) = modifier {
                return Err(wrong(format!("{modifier} specified, but {name} is not an aggregate function")));
            }
        }
        Kind::Aggregate if within_group => return Err(wrong(format!(
            "{name} is not an ordered-set aggregate, so it cannot have WITHIN GROUP"
        ))),
        Kind::OrderedSet { .. } | Kind::Hypothetical if !within_group => return Err(wrong(format!(
            "WITHIN GROUP is required for ordered-set aggregate {name}"
        ))),
        Kind::OrderedSet { direct: expected, .. } if direct != expected => return Err(invalid_split(name, names, types, format!(
            "There is an ordered-set aggregate {name}, but it requires {expected} direct argument{}, not {direct}.", if expected == 1 { "" } else { "s" }
        ))),
        Kind::Hypothetical if direct != ordered => return Err(invalid_split(name, names, types, format!(
            "To use the hypothetical-set aggregate {name}, the number of hypothetical direct arguments (here {direct}) must match the number of ordering columns (here {ordered})."
        ))),
        Kind::Window => return Err(wrong(format!("window function {name} requires an OVER clause"))),
        _ => {},
    }
    // ParseFuncOrColumn checks the selected routine's modifiers before
    // enforce_generic_type_consistency, and reads implicit inputs afterwards.
    if matches!(
        kind,
        Kind::OrderedSet {
            polymorphic: true,
            ..
        }
    ) && types.last().is_some_and(Option::is_none)
    {
        return Err(SQLError::Routine {
            sqlstate: "42804".into(),
            message: "could not determine polymorphic type because input has type unknown".into(),
        });
    }
    Ok(())
}

fn invalid_split(
    name: &str,
    names: &[Option<String>],
    types: &[Option<ColumnType>],
    hint: String,
) -> SQLError {
    let mut error = crate::type_resolution::function_resolution_error(
        "42883",
        name,
        names,
        types,
        "does not exist",
    );
    if let SQLError::Diagnostic {
        hint: selected_hint,
        ..
    } = &mut error
    {
        *selected_hint = Some(hint);
    }
    error
}

impl super::SchemaScope {
    pub(super) fn bind_ordered_function_for_storage(
        &mut self,
        routines: &dyn crate::routines::RoutineResolution,
        expression: &mut crate::ScalarExpr,
        schema: &crate::RowSchema,
        subqueries: &[crate::plan::QueryPlan],
        params: &[crate::SQLParam],
        outer: Option<&crate::RowSchema>,
    ) -> Result<(), SQLError> {
        let resolver = self.query_function_type_resolver(
            routines, expression, schema, subqueries, params, outer,
        )?;
        let crate::ScalarExpr::Func {
            name,
            binding,
            args,
            order_syntax,
            order_by,
            distinct,
            filter,
        } = expression
        else {
            unreachable!("function binding")
        };
        let within_group =
            uses_ordered_arguments(*order_syntax, name, binding.as_ref(), order_by.len());
        let (mut names, mut types, variadic) =
            crate::function_call_argument_signature(args, schema, params, Some(&resolver))?;
        if within_group {
            for order in order_by.iter() {
                names.push(None);
                types.push(
                    crate::common_context_expression_type(
                        &order.expr,
                        schema,
                        params,
                        Some(&resolver),
                    )?
                    .and_then(|ty| {
                        crate::effective_overload_argument_type_with_params(
                            &order.expr,
                            Some(ty),
                            params,
                        )
                    }),
                );
            }
        }
        if let Some((selected, kind)) =
            resolve(name, binding.as_ref(), &names, &types, variadic, &resolver)?
        {
            validate(
                name,
                kind,
                Modifiers {
                    direct: args.len(),
                    ordered: order_by.len(),
                    within_group,
                    distinct: *distinct,
                    filtered: filter.is_some(),
                },
                &names,
                &types,
            )?;
            self.record_routine_dependency(&selected.binding);
            *binding = Some(selected.binding);
            if within_group && *order_syntax == FunctionOrderSyntax::Legacy {
                *order_syntax = FunctionOrderSyntax::WithinGroup;
            }
        }
        Ok(())
    }
}
