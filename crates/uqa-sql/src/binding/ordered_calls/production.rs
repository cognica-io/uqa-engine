//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered-call candidates admit their declaration fields before sharing the ordinary matcher.

use super::{catalog, is_polymorphic, Kind};
use crate::type_resolution::{
    BuiltinFunctionOverload, ResolvedFunctionOverload, RoutineCallDescriptor,
};
use crate::{ast::FunctionBinding, ColumnType, FunctionTypeResolver, SQLError};
use uqa_core::memory::{Produced, ProductionControl, ProductionVec};

pub(in crate::binding) fn resolve_with_control(
    name: &str,
    binding: Option<&FunctionBinding>,
    names: &[Option<String>],
    types: &[Option<ColumnType>],
    variadic: bool,
    resolver: &dyn FunctionTypeResolver,
    control: &ProductionControl<'_>,
) -> Result<Option<(Produced<ResolvedFunctionOverload>, Kind)>, SQLError> {
    let lower = crate::semantics::builtin_function_dispatch_name_with_control(name, control)?;
    let mut declarations = ProductionVec::new(*control);
    let mut kinds = ProductionVec::new(*control);
    for declaration in catalog::SIGNATURES.lines() {
        control.check()?;
        let mut fields = declaration.split('|');
        if fields.next() != Some(lower.as_ref()) {
            continue;
        }
        let [arguments, result, proc_kind, aggregate_kind, direct] =
            std::array::from_fn(|_| fields.next().expect("catalog signature field"));
        let kind = match (proc_kind, aggregate_kind) {
            ("w", _) => Kind::Window,
            (_, "o") => Kind::OrderedSet {
                direct: direct.parse().expect("catalog direct count"),
                polymorphic: arguments.contains("anyelement"),
            },
            (_, "h") => {
                if types.is_empty() {
                    continue;
                }
                Kind::Hypothetical
            }
            _ => Kind::Aggregate,
        };
        let arguments = if matches!(kind, Kind::Hypothetical) {
            candidate_types(std::iter::repeat_n("any", types.len()), control)?
        } else {
            candidate_types(
                arguments.split(", ").filter(|name| !name.is_empty()),
                control,
            )?
        };
        declarations.push_produced(candidate(&lower, arguments, result, control)?)?;
        kinds.push_copy(kind)?;
    }
    if matches!(lower.as_ref(), "lower" | "upper" | "initcap") {
        declarations.push_produced(candidate(
            &lower,
            candidate_types(["text"].into_iter(), control)?,
            "text",
            control,
        )?)?;
        kinds.push_copy(Kind::Ordinary)?;
    }
    if declarations.is_empty() {
        return Ok(None);
    }
    let declarations = declarations.finish()?;
    let selected = match resolver.resolve_function_overload_with_builtins_and_control(
        name,
        binding,
        names,
        types,
        variadic,
        &declarations,
        control,
    )? {
        Some(selected) => selected,
        None => crate::type_resolution::resolve_local_builtin_call_with_control(
            name,
            binding,
            RoutineCallDescriptor {
                argument_names: names,
                argument_types: types,
                explicit_variadic: false,
            },
            &declarations,
            control,
        )?,
    };
    let kind = if selected.binding.builtin {
        let mut kind = None;
        for (declaration, candidate_kind) in declarations.iter().zip(kinds.iter()) {
            if crate::type_resolution::builtin_binding_matches_with_control(
                declaration,
                &selected.binding,
                control,
            )? {
                kind = Some(*candidate_kind);
                break;
            }
        }
        kind.ok_or_else(|| SQLError::Internal("selected aggregate lost its declaration".into()))?
    } else {
        Kind::Ordinary
    };
    Ok(Some((selected, kind)))
}

fn candidate_types<'a>(
    names: impl Iterator<Item = &'a str>,
    control: &ProductionControl<'_>,
) -> Result<Produced<Vec<ColumnType>>, SQLError> {
    let mut output = ProductionVec::new(*control);
    output.reserve(names.size_hint().0)?;
    for name in names {
        output.push_produced(parse_type(name, control)?)?;
    }
    Ok(output.finish()?)
}
fn parse_type(
    name: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<ColumnType>, SQLError> {
    if is_polymorphic(name) || matches!(name, "money" | "pg_lsn" | "tid" | "xid8") {
        let (name, memory) = control.copy_text(name)?.into_parts();
        Ok(control.finish(ColumnType::Named(name), memory)?)
    } else {
        ColumnType::from_sql_name_with_control(name, control)
    }
}
fn candidate(
    name: &str,
    arguments: Produced<Vec<ColumnType>>,
    result: &str,
    control: &ProductionControl<'_>,
) -> Result<Produced<BuiltinFunctionOverload>, SQLError> {
    let name = control.format(format_args!("pg_catalog.{name}"))?;
    let result = parse_type(result, control)?;
    let mut names = ProductionVec::new(*control);
    names.reserve(arguments.len())?;
    for _ in 0..arguments.len() {
        names.push_produced(control.finish(None, control.empty_reservation())?)?;
    }
    let names = names.finish()?;
    let (name, name_memory) = name.into_parts();
    let (argument_types, types_memory) = arguments.into_parts();
    let (argument_names, labels_memory) = names.into_parts();
    let (return_type, result_memory) = result.into_parts();
    Ok(control.finish(
        BuiltinFunctionOverload {
            variadic: false,
            name,
            argument_names,
            argument_types,
            default_arguments: 0,
            return_type,
        },
        control.combine(
            name_memory,
            control.combine(types_memory, control.combine(labels_memory, result_memory)),
        ),
    )?)
}
