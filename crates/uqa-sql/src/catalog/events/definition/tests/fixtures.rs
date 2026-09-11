//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    ast::{ColumnDef, FunctionBinding, TableHierarchy},
    binding::{
        snapshot::BindingSnapshot,
        stored_columns::{StoredColumnBindingContext, StoredColumnCatalog},
        stored_relations::{StoredQueryNamespace, StoredQuerySequences, StoredRelationCatalog},
        stored_routines::analysis::{CatalogRoutineAnalysisContext, CatalogRoutineScopes},
    },
    catalog::{
        regrole_dependencies::StoredRegroleResolver, security::table::TableAclPrivilege,
        stored_view::StoredView,
    },
    routines::{
        compilation::RoutineCompilationCatalog, merge_columns::StoredMergeColumnCatalog,
        registration::RoutineSupportAuthority, security::RoutineExecutionAuthority,
        RoutineResolution, SQLUserFunction,
    },
    semantics::{
        mutation_privileges::MutationPrivilegeCatalog,
        privileges::TargetSelectPrivilegeRequest,
        returning::{ReturningAnalysisContext, ReturningCatalog, ReturningScope},
        rules::action_binding::RuleSourceCatalog,
        view_privileges::ViewPrivilegeCatalog,
    },
    ColumnType, FunctionTypeResolver, RowSchema,
};
use std::sync::{Arc, Mutex};

pub(super) struct Catalog {
    pub events: Mutex<Vec<String>>,
    pub kind: &'static str,
    pub allow_owner: bool,
    pub allow_trigger: bool,
    pub routines: Vec<Arc<SQLUserFunction>>,
}
impl Default for Catalog {
    fn default() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            kind: "table",
            allow_owner: true,
            allow_trigger: true,
            routines: vec![routine()],
        }
    }
}
impl Catalog {
    pub fn record(&self, event: impl Into<String>) {
        self.events.lock().unwrap().push(event.into());
    }
    pub fn context(&self) -> EventAnalysisContext<'_> {
        EventAnalysisContext {
            catalog: self,
            relations: self,
            sources: self,
            routines: self,
            authority: self,
            privileges: self,
            foreign_privileges: self,
            columns: StoredColumnBindingContext {
                sources: self,
                merge: self,
            },
            returning: ReturningAnalysisContext {
                catalog: self,
                routines: self,
                aggregates: self,
                scope: self,
            },
            stored_routines: CatalogRoutineAnalysisContext {
                scopes: self,
                routines: self,
            },
            namespaces: self,
            sequences: self,
            regroles: self,
        }
    }
}
fn routine() -> Arc<SQLUserFunction> {
    let Statement::CreateFunction(mut def) = crate::compile("CREATE FUNCTION public.handler() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$").unwrap().remove(0) else {panic!("expected routine")};
    def.owner = "owner".into();
    let compiled = crate::routines::CompiledFunctionBody::PLpgSQL(
        crate::plpgsql::parse_function(&def).unwrap(),
    );
    Arc::new(SQLUserFunction {
        def: *def,
        compiled,
    })
}
impl EventRelationCatalog for Catalog {
    fn event_relation_owner(
        &self,
        relation: &RelationIdentity,
    ) -> Result<(String, &'static str), SQLError> {
        self.record(format!("owner:{}", relation.qualified_name()));
        Ok(("owner".into(), self.kind))
    }
    fn view_kind(&self, _: &RelationIdentity) -> Option<StoredViewKind> {
        self.record("view-kind");
        match self.kind {
            "view" => Some(StoredViewKind::View),
            "materialized view" => Some(StoredViewKind::Materialized),
            _ => None,
        }
    }
    fn view(&self, _: &RelationIdentity) -> Option<StoredView> {
        panic!("unexpected view entry read")
    }
    fn foreign_columns(&self, _: &RelationIdentity) -> Option<Vec<ColumnDef>> {
        panic!("unexpected foreign entry read")
    }
    fn restored_catalog_view_definition(&self, _: &str) -> Result<Option<StoredView>, SQLError> {
        panic!("unexpected restored view read")
    }
    fn stored_view_schema(&self, _: &StoredView) -> Result<RowSchema, SQLError> {
        panic!("unexpected view schema")
    }
    fn loaded_table_hierarchy(&self, _: &RelationIdentity) -> Option<TableHierarchy> {
        panic!("no transition declarations require hierarchy metadata")
    }
}
impl EventForeignPrivileges for Catalog {
    fn ensure_foreign_table_privilege(
        &self,
        _: &str,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("unexpected foreign privilege check")
    }
}
impl StoredRelationCatalog for Catalog {
    fn resolve_age_label_relation_name(&self, _: &str) -> Result<Option<String>, SQLError> {
        Ok(None)
    }
    fn resolve_visible_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        self.record(format!("visible:{name}"));
        Ok(RelationResolution::Found(
            if name.contains('.') {
                name.to_string()
            } else {
                format!("public.{name}")
            },
            self.kind,
        ))
    }
    fn resolve_loaded_visible_relation_kind(
        &self,
        _: &str,
    ) -> Result<RelationResolution, SQLError> {
        panic!("unexpected loaded-name lookup")
    }
    fn resolve_bound_relation_kind(&self, name: &str) -> Result<RelationResolution, SQLError> {
        self.record(format!("bound:{name}"));
        Ok(RelationResolution::Found(
            if name.contains('.') {
                name.to_string()
            } else {
                format!("public.{name}")
            },
            self.kind,
        ))
    }
}
impl FunctionTypeResolver for Catalog {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        panic!("unexpected function type query")
    }
}
impl RoutineResolution for Catalog {
    fn lookup_visible_sql_functions(
        &self,
        name: &str,
    ) -> Result<Option<Vec<Arc<SQLUserFunction>>>, SQLError> {
        self.record(format!("routine-visible:{name}"));
        Ok(Some(self.routines.clone()))
    }
    fn lookup_bound_sql_functions(&self, name: &str) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.record(format!("routine-bound:{name}"));
        Some(self.routines.clone())
    }
    fn lookup_bound_sql_functions_by_binding(
        &self,
        binding: &FunctionBinding,
    ) -> Option<Vec<Arc<SQLUserFunction>>> {
        self.record(format!("routine-id:{}", binding.name));
        None
    }
}
impl RoutineSupportAuthority for Catalog {
    fn current_user_is_superuser(&self) -> bool {
        self.record("superuser");
        false
    }
}
impl RoutineExecutionAuthority for Catalog {
    fn current_user_name(&self) -> String {
        self.record("current-user");
        "reader".into()
    }
    fn current_user_has_role_privileges(&self, role: &str) -> bool {
        self.record(format!("inherits:{role}"));
        self.allow_owner
    }
}
impl ViewPrivilegeCatalog for Catalog {
    fn view_definition(&self, _: &str) -> Result<Option<StoredView>, SQLError> {
        panic!("unexpected visible view")
    }
    fn current_user_name(&self) -> String {
        RoutineExecutionAuthority::current_user_name(self)
    }
    fn ensure_view_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &str,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("unexpected view privilege")
    }
    fn ensure_view_column_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &str,
        _: &str,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("unexpected view column privilege")
    }
    fn ensure_any_view_column_privilege_for(
        &self,
        _: &str,
        _: &StoredView,
        _: &str,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("unexpected view column privilege")
    }
    fn ensure_target_select(
        &self,
        _: TargetSelectPrivilegeRequest<'_, '_>,
    ) -> Result<(), SQLError> {
        panic!("unexpected SELECT privilege")
    }
}
impl MutationPrivilegeCatalog for Catalog {
    fn bound_table_column_names(&self, _: &str) -> Result<Vec<String>, SQLError> {
        panic!("unexpected bound columns")
    }
    fn ensure_table_privilege_for(
        &self,
        table: &str,
        subject: &str,
        privilege: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        assert!(matches!(privilege, TableAclPrivilege::Trigger));
        self.record(format!("trigger-privilege:{table}:{subject}"));
        if self.allow_trigger {
            Ok(())
        } else {
            Err(SQLError::Routine {
                sqlstate: "42501".into(),
                message: "permission denied for table items".into(),
            })
        }
    }
    fn ensure_column_privilege_for(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("unexpected column privilege")
    }
    fn ensure_any_column_privilege_for(
        &self,
        _: &str,
        _: &str,
        _: TableAclPrivilege,
    ) -> Result<(), SQLError> {
        panic!("unexpected column privilege")
    }
}
impl ReturningCatalog for Catalog {
    fn try_describe_table_row_type(&self, table: &str) -> Result<Option<Vec<ColumnDef>>, String> {
        self.record(format!("columns:{table}"));
        let Statement::CreateTable(table) = crate::compile("CREATE TABLE items (id integer)")
            .unwrap()
            .remove(0)
        else {
            panic!("expected table")
        };
        Ok(Some(table.columns))
    }
    fn try_table_columns(&self, _: &str) -> Result<Vec<String>, String> {
        panic!("unexpected untyped columns")
    }
    fn view_schema(&self, _: &str) -> Result<Option<RowSchema>, SQLError> {
        panic!("unexpected returning view schema")
    }
}
impl ReturningScope for Catalog {
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError> {
        panic!("unexpected RETURNING scope")
    }
}
impl crate::plan::AggregateClassifier for Catalog {
    fn is_registered_aggregate(&self, _: &str) -> bool {
        false
    }
}
impl RuleSourceCatalog for Catalog {
    fn query_source_columns(&self, _: &str, _: bool) -> Result<Option<Vec<String>>, SQLError> {
        panic!("unexpected action source")
    }
    fn rule_relation_columns(&self, name: &str) -> Result<Vec<(String, ColumnType)>, SQLError> {
        self.context().rule_relation_columns(name)
    }
}
impl StoredColumnCatalog for Catalog {
    fn stored_relation_column_names(&self, _: &str) -> Result<Option<Vec<String>>, SQLError> {
        panic!("unexpected stored columns")
    }
}
impl StoredMergeColumnCatalog for Catalog {
    fn stored_merge_target_definitions(&self, _: &str) -> Option<Vec<ColumnDef>> {
        panic!("unexpected MERGE metadata")
    }
}
impl StoredRegroleResolver for Catalog {
    fn resolve_stored_regrole(&self, _: &str) -> Result<Option<i64>, SQLError> {
        panic!("unexpected stored regrole")
    }
}
impl StoredQuerySequences for Catalog {
    fn query_sequence(&self, _: &str) -> Result<String, String> {
        panic!("unexpected sequence")
    }
    fn loaded_query_sequence(&self, _: &str) -> Result<String, String> {
        panic!("unexpected loaded sequence")
    }
}
impl RoutineCompilationCatalog for Catalog {
    fn has_registered_aggregate_function(&self, _: &str) -> bool {
        false
    }
    fn binding_snapshot(&self) -> Result<BindingSnapshot, SQLError> {
        panic!("unexpected detached compilation scope")
    }
    fn stored_query_namespace(&self) -> StoredQueryNamespace {
        panic!("unexpected query namespace")
    }
}
impl CatalogRoutineScopes for Catalog {
    fn with_catalog_scope(
        &self,
        _: crate::binding::statements::StatementAnalysisOperation<'_>,
    ) -> Result<(), SQLError> {
        panic!("declaration rejection must precede catalog routine binding")
    }
}
