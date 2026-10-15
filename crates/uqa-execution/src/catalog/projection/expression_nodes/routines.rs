//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Resolve retained routine identities through the existing SQL signature matcher.

use uqa_sql::{
    ast::FunctionBinding,
    catalog::{
        node_tree::expressions::{
            select_routine, ExpressionRoutines, RoutineDeclaration, RoutineIdentity,
        },
        type_metadata,
    },
    type_resolution::{
        resolve_fixed_builtin_call, ResolvedFunctionOverload, RoutineCallDescriptor,
        RoutineParameterDescriptor,
    },
    ColumnType, SQLError,
};

use super::super::builtin_routines::{BuiltinRoutineCatalogEntry, PG18_BUILTIN_ROUTINE_GROUPS};
use crate::catalog::context::CatalogContext;

pub(super) struct Routines<'a, 'b>(pub &'a CatalogContext<'b>);

impl ExpressionRoutines for Routines<'_, '_> {
    fn resolve(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        call: RoutineCallDescriptor<'_>,
    ) -> Result<RoutineIdentity, SQLError> {
        if let Some(selected) = resolve_fixed_builtin_call(
            name,
            binding,
            call.argument_names,
            call.argument_types,
            call.explicit_variadic,
            Some(self.0.routines),
        )? {
            return self.selected(selected.selected, call);
        }
        if let Some(selected) = self.0.routines.resolve_function_overload(
            name,
            binding,
            call.argument_names,
            call.argument_types,
            call.explicit_variadic,
        )? {
            return self.selected(selected, call);
        }
        let local = uqa_sql::parse_regobject_name(name)
            .and_then(|parts| parts.last().cloned())
            .ok_or_else(|| SQLError::Internal(format!("invalid stored routine name {name}")))?;
        let declarations = PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .filter(|entry| entry.name == local)
            .map(|entry| self.declaration(*entry))
            .collect::<Result<Vec<_>, _>>()?;
        select_routine(name, &declarations, call)
    }
}

impl Routines<'_, '_> {
    fn type_oid(&self, oid: i64) -> Result<ColumnType, SQLError> {
        let oid = u32::try_from(oid)
            .map_err(|_| SQLError::Internal("invalid routine type OID".into()))?;
        type_metadata::builtin_scalar_type(oid)
            .cloned()
            .or_else(|| {
                type_metadata::builtin_array_element(oid)
                    .cloned()
                    .map(|element| ColumnType::Array(Box::new(element)))
            })
            .or_else(|| super::super::regtypes::resolve_catalog_user_type_by_oid(self.0, oid))
            .ok_or_else(|| SQLError::Internal(format!("unknown stored routine type OID {oid}")))
    }

    fn declaration(
        &self,
        entry: BuiltinRoutineCatalogEntry,
    ) -> Result<RoutineDeclaration, SQLError> {
        let parameters = entry
            .argument_types
            .iter()
            .enumerate()
            .map(|(index, oid)| {
                let (type_name, column_type) =
                    if let Some(name) = type_metadata::routine_pseudo_type_name(*oid) {
                        (name.to_owned(), None)
                    } else {
                        let ty = self.type_oid(*oid)?;
                        (ty.catalog_name(), Some(ty))
                    };
                Ok(RoutineParameterDescriptor {
                    name: entry
                        .argument_names
                        .get(index)
                        .map(|name| (*name).to_owned()),
                    type_name,
                    column_type,
                    has_default: index >= entry.argument_types.len() - entry.default_arguments,
                    default_type: None,
                    variadic: entry.variadic_type() != 0 && index + 1 == entry.argument_types.len(),
                })
            })
            .collect::<Result<Vec<_>, SQLError>>()?;
        let return_type = type_metadata::routine_pseudo_type_name(entry.return_type)
            .map(str::to_owned)
            .map(Ok)
            .unwrap_or_else(|| self.type_oid(entry.return_type).map(|ty| ty.catalog_name()))?;
        Ok(RoutineDeclaration {
            oid: entry.oid,
            parameters,
            return_type,
        })
    }

    fn selected(
        &self,
        selected: ResolvedFunctionOverload,
        call: RoutineCallDescriptor<'_>,
    ) -> Result<RoutineIdentity, SQLError> {
        if selected.binding.builtin {
            let names = uqa_sql::parse_regobject_name(&selected.binding.name)
                .ok_or_else(|| SQLError::Internal("invalid selected builtin name".into()))?;
            let local = names.last().expect("parsed routine name");
            let oids = selected
                .binding
                .argument_types
                .iter()
                .map(|name| type_metadata::routine_type_oid(name))
                .collect::<Vec<_>>();
            let entry = PG18_BUILTIN_ROUTINE_GROUPS
                .iter()
                .flat_map(|group| group.iter())
                .find(|entry| entry.name == local && entry.argument_types == oids)
                .ok_or_else(|| {
                    SQLError::Internal(format!(
                        "selected builtin {} has no catalog signature",
                        selected.binding.name
                    ))
                })?;
            return select_routine(&selected.binding.name, &[self.declaration(*entry)?], call);
        }
        let catalog = self.0.catalog_read_view();
        let functions =
            uqa_sql::routines::lifecycle::lookup::lookup_bound_sql_functions_by_binding(
                &catalog.snapshot().definitions.sql_user_functions,
                &selected.binding,
            )
            .ok_or_else(|| {
                SQLError::Internal("selected routine is absent from retained catalog".into())
            })?;
        let function = functions
            .iter()
            .find(|function| {
                uqa_sql::routines::routine_signature_types(&function.def)
                    == selected.binding.argument_types
            })
            .ok_or_else(|| {
                SQLError::Internal(
                    "selected routine signature is absent from retained catalog".into(),
                )
            })?;
        Ok(RoutineIdentity {
            oid: super::super::pg_proc::user_routine_catalog_oid(function)?,
            invocation: *selected.binding.invocation.ok_or_else(|| {
                SQLError::Internal("selected routine has no invocation binding".into())
            })?,
            result_type: selected.return_type,
        })
    }
}
