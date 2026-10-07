//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Adapt catalog routine identities to SQL's exact foreign-wrapper lookup.

use super::ForeignCreationContext;
use crate::catalog::projection::{
    catalog_routine_type_oid, user_routine_catalog_oid, PG18_BUILTIN_ROUTINE_GROUPS,
};
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::FunctionBinding,
    routines::lifecycle::names::{exact_function, RoutineCatalogIdentity},
    schema::foreign_wrappers::ForeignWrapperRoutine,
    SQLError,
};

impl ForeignCreationContext<'_> {
    pub(super) fn lookup_wrapper_function(
        &self,
        name: &str,
        arguments: &[i64],
        display: &[String],
    ) -> Result<ForeignWrapperRoutine, SQLError> {
        let catalog = self.identities.catalog.catalog_snapshot();
        let mut identities = Vec::new();
        let mut routines = Vec::new();
        for routine in catalog
            .snapshot()
            .definitions
            .sql_user_functions
            .values()
            .flatten()
        {
            let argument_types = uqa_sql::routines::routine_signature_types(&routine.def);
            let oids: Vec<_> = argument_types
                .iter()
                .map(|ty| catalog_routine_type_oid(&catalog, ty))
                .collect();
            if oids != arguments {
                continue;
            }
            let oid = u32::try_from(user_routine_catalog_oid(routine)?)
                .map_err(|e| SQLError::Internal(e.to_string()))?;
            identities.push(RoutineCatalogIdentity {
                oid,
                relation: RelationIdentity::from_legacy_name(&routine.def.name)
                    .map_err(SQLError::Internal)?,
                argument_types: oids,
                kind: if routine.def.is_procedure { 'p' } else { 'f' },
            });
            routines.push(ForeignWrapperRoutine {
                oid,
                return_oid: catalog_routine_type_oid(
                    &catalog,
                    uqa_sql::routines::declaration::result_type_name(&routine.def),
                ),
                binding: FunctionBinding {
                    object_id: routine.def.object_id,
                    name: routine.def.name.clone(),
                    argument_types,
                    builtin: false,
                    dispatch: None,
                    invocation: None,
                    resolution_error: None,
                },
            });
        }
        for routine in PG18_BUILTIN_ROUTINE_GROUPS
            .iter()
            .flat_map(|group| group.iter())
            .copied()
            .chain(crate::catalog::projection::native_foreign_handlers())
            .filter(|routine| routine.argument_types == arguments)
        {
            let oid = u32::try_from(routine.oid).map_err(|e| SQLError::Internal(e.to_string()))?;
            let relation = RelationIdentity::new("pg_catalog", routine.name);
            routines.push(ForeignWrapperRoutine {
                oid,
                return_oid: routine.return_type,
                binding: FunctionBinding {
                    object_id: None,
                    name: relation.qualified_name(),
                    argument_types: display.to_vec(),
                    builtin: true,
                    dispatch: None,
                    invocation: None,
                    resolution_error: None,
                },
            });
            identities.push(RoutineCatalogIdentity {
                oid,
                relation,
                argument_types: arguments.to_vec(),
                kind: routine.kind.chars().next().unwrap_or('f'),
            });
        }
        let index = exact_function(self.routine_names, &identities, name, arguments, display)?;
        Ok(routines.swap_remove(index))
    }
}
