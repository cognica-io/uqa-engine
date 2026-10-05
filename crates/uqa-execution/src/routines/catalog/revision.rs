//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! New routine catalog tuples keep object identity and receive a fresh replacement identity.

use std::sync::Arc;
use uqa_sql::{
    ast::{CreateFunction, FunctionBinding},
    routines::{lifecycle::RoutineRegistry, RoutineBody, SQLUserFunction},
    SQLError,
};

pub(in crate::routines) fn replacement(
    mut definition: CreateFunction,
    body: RoutineBody,
) -> Result<Arc<SQLUserFunction>, SQLError> {
    definition.catalog_revision = Some(
        crate::catalog::identity::new_nonzero_catalog_identity("routine", "tuple revision")
            .map_err(|error| SQLError::Internal(error.to_string()))?,
    );
    Ok(Arc::new(SQLUserFunction::new(definition, body)))
}

pub(in crate::routines) fn renamed(
    registry: &mut RoutineRegistry,
    name: &str,
    binding: &FunctionBinding,
) -> Result<(), SQLError> {
    let routine = registry
        .get_mut(name)
        .and_then(|overloads| {
            overloads.iter_mut().find(|routine| {
                binding.object_id.is_some() && routine.def.object_id == binding.object_id
            })
        })
        .ok_or_else(|| SQLError::Internal(format!("renamed routine `{name}` disappeared")))?;
    *routine = replacement(routine.def.clone(), routine.body.clone())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_replacements_keep_object_identity_but_change_reloadable_tuple_identity() {
        let uqa_sql::Statement::CreateFunction(mut definition) =
            uqa_sql::compile("CREATE FUNCTION f() RETURNS int LANGUAGE sql AS 'SELECT 1'")
                .unwrap()
                .remove(0)
        else {
            panic!("function");
        };
        definition.object_id = Some([7; 16]);
        let original = replacement(*definition, RoutineBody::Source).unwrap();
        let replaced = replacement(original.def.clone(), RoutineBody::Source).unwrap();
        assert_eq!(original.def.object_id, replaced.def.object_id);
        assert_ne!(original.catalog_revision(), replaced.catalog_revision());
        let stored = serde_json::to_string(&replaced.def).unwrap();
        let restored =
            SQLUserFunction::new(serde_json::from_str(&stored).unwrap(), RoutineBody::Source);
        assert_eq!(restored.catalog_revision(), replaced.catalog_revision());
        assert_eq!(
            restored.definition_version().unwrap(),
            replaced.definition_version().unwrap()
        );
        assert_ne!(
            original.definition_version().unwrap(),
            restored.definition_version().unwrap()
        );
    }
}
