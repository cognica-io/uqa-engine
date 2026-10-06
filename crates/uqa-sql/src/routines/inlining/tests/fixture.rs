//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Catalog capabilities for caller-owned SQL body analysis.

use super::super::*;
use crate::{
    ast::{ColumnDef, Statement},
    binding::{snapshot::BindingSnapshot, stored_relations::StoredQueryNamespace},
    catalog::roles::{RoleIdentity, RoleReference},
    parser::ParserSettings,
    routines::{
        registration::RoutineSupportAuthority,
        resolution::{RoutineOverloadCatalog, RoutineOverloadContext, RoutineTypeSnapshot},
        SQLUserFunction,
    },
    type_resolution::{BuiltinFunctionOverload, FunctionTypeResolver, ResolvedFunctionOverload},
    SQLNotice,
};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

pub(super) struct Catalog {
    pub(super) functions: Vec<Arc<SQLUserFunction>>,
    pub(super) settings: Mutex<ParserSettings>,
    pub(super) notices: Mutex<Vec<SQLNotice>>,
    pub(super) evaluations: AtomicUsize,
    pub(super) allowed: AtomicBool,
}

impl Catalog {
    pub(super) fn new(sql: &str) -> Self {
        let functions = crate::compile(sql)
            .unwrap()
            .into_iter()
            .enumerate()
            .map(|(index, statement)| {
                let Statement::CreateFunction(definition) = statement else {
                    panic!("expected function")
                };
                let mut definition = *definition;
                for action in std::mem::take(&mut definition.config_actions) {
                    let crate::ast::RoutineConfigAction::Set { name, value } = action else {
                        panic!("fixture configuration must be an explicit SET");
                    };
                    definition.config.push((name, value));
                }
                definition.object_id = Some([u8::try_from(index + 1).unwrap(); 16]);
                definition.owner = Some(RoleIdentity::BOOTSTRAP);
                definition.name = format!("public.{}", definition.name);
                Arc::new(SQLUserFunction::new(definition, RoutineBody::Source))
            })
            .collect();
        Self {
            functions,
            settings: Mutex::new(ParserSettings::default()),
            notices: Mutex::default(),
            evaluations: AtomicUsize::new(0),
            allowed: AtomicBool::new(true),
        }
    }

    pub(super) fn context(&self) -> RoutineInliningContext<'_> {
        RoutineInliningContext {
            routines: self,
            types: self,
            parsers: self,
            catalog: self,
            authority: self,
            volatility: self,
            expressions: self,
        }
    }

    pub(super) fn binding(&self, name: &str, types: &[Option<ColumnType>]) -> FunctionBinding {
        RoutineOverloadContext { catalog: self }
            .resolve_static_sql_function_match(name, None, &vec![None; types.len()], types, false)
            .unwrap()
            .unwrap()
            .binding()
    }

    pub(super) fn take_notices(&self) -> Vec<SQLNotice> {
        std::mem::take(&mut *self.notices.lock().unwrap())
    }
}

impl RoutineTypeCatalog for Catalog {
    fn try_describe_table(&self, _: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        Ok(None)
    }
    fn resolve_catalog_column_type(&self, name: &str) -> Option<ColumnType> {
        ColumnType::from_sql_name(name).ok()
    }
    fn resolve_catalog_column_type_name(&self, name: &str) -> Result<ColumnType, SQLError> {
        ColumnType::from_sql_name(name)
    }
    fn resolve_catalog_user_type_by_oid(&self, _: u32) -> Option<ColumnType> {
        None
    }
    fn require_type_usage(&self, _: &ColumnType) -> Result<(), SQLError> {
        Ok(())
    }
    fn format_type(&self, ty: &ColumnType) -> Result<String, SQLError> {
        Ok(ty.regtype_name())
    }
}
impl RoutineParserCatalog for Catalog {
    fn plpgsql_catalog(&self) -> Result<crate::plpgsql::PlpgsqlCatalog, SQLError> {
        panic!("SQL candidate never compiles PL/pgSQL")
    }
    fn parser_settings(&self) -> ParserSettings {
        *self.settings.lock().unwrap()
    }
    fn parser_notice(&self, notice: SQLNotice) {
        self.notices.lock().unwrap().push(notice);
    }
}
impl crate::schema::dependencies::oid_alias::OidAliasInput for Catalog {
    fn resolve_oid_alias_input(&self, _: &ColumnType, _: &str) -> Result<Option<i64>, SQLError> {
        Ok(None)
    }
}
impl RoutineCompilationCatalog for Catalog {
    fn has_registered_aggregate_function(&self, _: &str) -> bool {
        false
    }
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError> {
        Ok(crate::binding::fixture::empty_binding_context().into())
    }
    fn stored_query_namespace(&self) -> StoredQueryNamespace {
        panic!("inline candidate never publishes dependencies")
    }
}
impl RoutineSupportAuthority for Catalog {
    fn current_user_is_superuser(&self) -> bool {
        self.allowed.load(Ordering::SeqCst)
    }
}
impl RoutineExecutionAuthority for Catalog {
    fn current_role(&self) -> RoleReference {
        RoleReference::Named("uqa".into())
    }
    fn current_user_has_role_identity_privileges(&self, _: RoleIdentity) -> bool {
        self.allowed.load(Ordering::SeqCst)
    }
}
impl RoutinePlanExpressions for Catalog {
    fn evaluate_constant_routine(&self, _: &ScalarExpr) -> Result<uqa_core::Value, SQLError> {
        self.evaluations.fetch_add(1, Ordering::SeqCst);
        Ok(uqa_core::Value::Int(37))
    }
}
impl VolatilityCatalog for Catalog {
    fn host_function_volatility(&self, _: &str) -> Option<crate::ast::FunctionVolatility> {
        None
    }
    fn routine_volatilities(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
    ) -> Option<Vec<crate::ast::FunctionVolatility>> {
        let functions = match binding {
            Some(binding) => {
                RoutineResolution::lookup_bound_sql_functions_by_binding(self, binding)
            }
            None => self.lookup_sql_routine_candidates(name).unwrap(),
        }?;
        Some(
            functions
                .iter()
                .map(|function| function.def.volatility)
                .collect(),
        )
    }
    fn view_query(&self, _: &str) -> Result<Option<crate::plan::QueryPlan>, SQLError> {
        Ok(None)
    }
}
impl RoutineOverloadCatalog for Catalog {
    fn routine_type_snapshot(&self) -> RoutineTypeSnapshot {
        Arc::new(BTreeMap::new())
    }
    fn routine_search_path(&self) -> Vec<String> {
        vec!["public".into()]
    }
    fn has_registered_scalar_function(&self, _: &str) -> bool {
        false
    }
    fn lookup_sql_routine_candidates(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        let functions = self
            .functions
            .iter()
            .filter(|function| {
                function.def.name == name || function.def.name == format!("public.{name}")
            })
            .cloned()
            .collect::<Vec<_>>();
        Ok((!functions.is_empty()).then_some(functions))
    }
    fn lookup_bound_sql_routine_candidates_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        RoutineResolution::lookup_bound_sql_functions_by_binding(self, binding)
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        RoutineResolution::lookup_bound_sql_functions_by_binding(self, binding)
    }
}
impl FunctionTypeResolver for Catalog {
    fn is_scalar_function_binding(&self, binding: &FunctionBinding) -> Result<bool, SQLError> {
        RoutineOverloadContext { catalog: self }.is_scalar_function_binding(binding)
    }
    fn resolve_type_name(&self, name: &str) -> Result<Option<ColumnType>, SQLError> {
        self.resolve_catalog_column_type_name(name).map(Some)
    }
    fn resolve_function_type(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        RoutineOverloadContext { catalog: self }
            .resolve_function_type(name, binding, names, types, variadic)
    }
    fn resolve_function_overload(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        RoutineOverloadContext { catalog: self }
            .resolve_function_overload(name, binding, names, types, variadic)
    }
    fn resolve_function_overload_with_builtins(
        &self,
        name: &str,
        binding: Option<&FunctionBinding>,
        names: &[Option<String>],
        types: &[Option<ColumnType>],
        variadic: bool,
        builtins: &[BuiltinFunctionOverload],
    ) -> Result<Option<ResolvedFunctionOverload>, SQLError> {
        RoutineOverloadContext { catalog: self }.resolve_function_overload_with_builtins(
            name, binding, names, types, variadic, builtins,
        )
    }
}
impl RoutineResolution for Catalog {
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        let functions = self
            .functions
            .iter()
            .filter(|function| function.def.object_id == binding.object_id)
            .cloned()
            .collect::<Vec<_>>();
        (!functions.is_empty()).then_some(functions)
    }
    fn lookup_visible_sql_functions(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        self.lookup_sql_routine_candidates(name)
    }
}
