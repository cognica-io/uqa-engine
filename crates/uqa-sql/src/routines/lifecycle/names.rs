//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Search-path and namespace authorization rules for routine names.

use crate::catalog::roles::RoleReference;
use crate::{catalog::security::BoundSchemaSecurity, SQLError};
use uqa_core::RelationIdentity;
mod production;
pub use production::{
    require_routine_schema_usage_with_control, routine_lookup_keys_with_control,
    routine_volatility_lookup_keys_with_control,
};

/// Immutable routine address and declared input types, independent of invocation coercions.
pub struct RoutineCatalogIdentity {
    pub oid: u32,
    pub relation: RelationIdentity,
    pub argument_types: Vec<i64>,
    pub kind: char,
}

pub trait RoutineNameCatalog {
    fn schema_security(&self, schema: &str) -> Option<BoundSchemaSecurity>;
    fn current_role(&self) -> RoleReference;
    fn search_path(&self) -> Vec<String>;
    fn search_path_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<uqa_core::memory::Produced<Vec<String>>, SQLError> {
        control.check()?;
        let path = self.search_path();
        if control.budget().is_none() {
            return Ok(control.finish(path, None)?);
        }
        crate::plan::copy::copy_syntax_with_control(&path, control)
    }
    /// The canonical name of this session's temporary namespace after its allocation.
    fn temporary_schema(&self) -> Option<String>;
    fn temporary_schema_with_control(
        &self,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<Option<uqa_core::memory::Produced<String>>, SQLError> {
        control.check()?;
        self.temporary_schema()
            .map(|name| {
                if control.budget().is_none() {
                    Ok(control.finish(name, None)?)
                } else {
                    control.copy_text(&name).map_err(Into::into)
                }
            })
            .transpose()
    }
    fn schema_exists_with_control(
        &self,
        schema: &str,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        Ok(self.schema_security(schema).is_some())
    }
    fn require_schema_usage(&self, schema: &str, role: &RoleReference) -> Result<(), SQLError>;
    fn schema_has_usage(&self, schema: &str, role: &RoleReference) -> bool;
    fn require_schema_usage_with_control(
        &self,
        schema: &str,
        role: &RoleReference,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<(), SQLError> {
        control.check()?;
        self.require_schema_usage(schema, role)
    }
    fn schema_has_usage_with_control(
        &self,
        schema: &str,
        role: &RoleReference,
        control: &uqa_core::memory::ProductionControl<'_>,
    ) -> Result<bool, SQLError> {
        control.check()?;
        Ok(self.schema_has_usage(schema, role))
    }
    /// `format_type_be` of a routine argument type: a user-defined type, recorded by identity, is spelled by its current name.
    fn routine_type_display(&self, type_name: &str) -> String;
    /// `format_procedure` of an existing routine, including search-path qualification and its current input type names. Call before retaining the routine registry's write guard.
    fn routine_identity_display(&self, oid: u32) -> Result<String, SQLError>;
}

pub fn routine_lookup_keys(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
) -> Result<Vec<String>, SQLError> {
    routine_lookup_keys_with_control(
        catalog,
        name,
        &uqa_core::memory::ProductionControl::uncontrolled(),
    )
    .map(|keys| {
        keys.into_uncontrolled()
            .expect("ordinary routine namespace keys")
    })
}

/// Catalog command lookup includes implicit `pg_catalog` in `PostgreSQL`'s search-path position.
pub fn routine_lookup_keys_with_builtins(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
) -> Result<Vec<String>, SQLError> {
    routine_lookup_keys(catalog, name)
}

/// `LookupFuncName` matches declared input OIDs exactly, ignores procedures, and does not expand defaults or variadics.
pub fn exact_function(
    catalog: &dyn RoutineNameCatalog,
    identities: &[RoutineCatalogIdentity],
    name: &str,
    argument_types: &[i64],
    display_types: &[String],
) -> Result<usize, SQLError> {
    for key in routine_lookup_keys_with_builtins(catalog, name)? {
        if let Some(index) = identities.iter().position(|identity| {
            identity.relation.qualified_name() == key
                && identity.kind != 'p'
                && identity.argument_types == argument_types
        }) {
            return Ok(index);
        }
    }
    Err(SQLError::Routine {
        sqlstate: "42883".into(),
        message: format!(
            "function {} does not exist",
            super::routine_signature_display(catalog, name, display_types)
        ),
    })
}

/// Defer namespace errors during recursive argument analysis; definitive binding checks again.
pub fn routine_lookup_keys_for_analysis(
    catalog: &dyn RoutineNameCatalog,
    name: &str,
) -> Result<Option<Vec<String>>, SQLError> {
    match routine_lookup_keys(catalog, name) {
        Err(error) if crate::routines::is_routine_namespace_lookup_error(&error) => Ok(None),
        result => result.map(Some),
    }
}

#[cfg(test)]
mod tests;
