//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{
    catalog::services::CatalogSession,
    statement::{context::StatementEffects, transactions::StatementTransactions},
};
use parking_lot::Mutex;
use uqa_core::RelationIdentity;
use uqa_sql::{
    ast::{FunctionBinding, RelationPersistence, RuleEvent},
    catalog::{
        domain::StoredDomain,
        events::StoredRule,
        resolution::{RelationLookupMode, RelationNameResolution},
        session::PreparedStatementMetadata,
    },
    plan::QueryPlan,
    semantics::{
        effects::{QueryEffectCatalog, QueryEffectContext},
        rules::RuleCatalog,
    },
    ColumnType,
};

#[derive(Default)]
struct Inputs {
    events: Mutex<Vec<&'static str>>,
    read_only: bool,
    rules_fail: bool,
}

impl Inputs {
    fn context(&self) -> StatementValidationContext<'_> {
        StatementValidationContext {
            session: self,
            rules: self,
            effects: self,
            transactions: self,
        }
    }
    fn record(&self, event: &'static str) {
        self.events.lock().push(event);
    }
}

impl CatalogSession for Inputs {
    fn current_user(&self) -> String {
        panic!("validation must not read unrelated session values")
    }
    fn temporary_schema_name(&self) -> String {
        panic!("resolution already contains the temporary schema")
    }
    fn relation_name_resolution(&self) -> RelationNameResolution {
        self.record("resolution");
        RelationNameResolution {
            search_path: vec!["public".into()],
            temporary_schema: "pg_temp_1".into(),
            temporary_namespace_allocated: false,
            current_user: "postgres".into(),
            lookup_mode: RelationLookupMode::Dynamic,
        }
    }
    fn show_variable(&self, _: &str) -> Result<String, SQLError> {
        panic!("validation must not execute SHOW")
    }
    fn runtime_parameter_source(&self, _: &str) -> &'static str {
        panic!("validation must not enumerate settings")
    }
    fn prepared_statements(&self) -> Vec<PreparedStatementMetadata> {
        panic!("validation must not enumerate prepared statements")
    }
}

impl RuleCatalog for Inputs {
    fn relation_has_rules(&self, _: &str) -> Result<bool, SQLError> {
        panic!("unexpected rule lookup")
    }
    fn resolve_rule_relation(&self, _: &str) -> Result<RelationIdentity, SQLError> {
        panic!("empty rule sets need no RETURNING lookup")
    }
    fn rules_for(&self, _: &str, _: RuleEvent) -> Result<Vec<StoredRule>, SQLError> {
        self.record("rules");
        if self.rules_fail {
            Err(SQLError::Internal("rule catalog unavailable".into()))
        } else {
            Ok(Vec::new())
        }
    }
    fn resolve_mutation_target(&self, name: &str, _: bool) -> Result<String, SQLError> {
        self.record("target");
        Ok(name.into())
    }
}

impl StatementEffects for Inputs {
    fn query_effect_context(&self) -> QueryEffectContext<'_> {
        self.record("effects");
        QueryEffectContext {
            catalog: self,
            optimizer_effects: |_| false,
            graph_effects: |_| Ok(false),
        }
    }
}

impl uqa_sql::FunctionTypeResolver for Inputs {
    fn resolve_function_type(
        &self,
        _: &str,
        _: Option<&FunctionBinding>,
        _: &[Option<String>],
        _: &[Option<ColumnType>],
        _: bool,
    ) -> Result<Option<ColumnType>, SQLError> {
        Ok(None)
    }
}
impl uqa_sql::routines::RoutineResolution for Inputs {}
impl uqa_sql::expr::EngineHook for Inputs {
    fn nextval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("validation must not advance sequences")
    }
    fn currval(&self, _: &str) -> Result<i64, SQLError> {
        panic!("validation must not evaluate sequence functions")
    }
    fn setval(&self, _: &str, _: i64, _: bool) -> Result<i64, SQLError> {
        panic!("validation must not set sequences")
    }
}
impl QueryEffectCatalog for Inputs {
    fn registered_runtime_function_may_mutate_engine(&self, _: &str) -> bool {
        false
    }
    fn domain_by_oid(&self, _: u32) -> Option<StoredDomain> {
        None
    }
    fn sequence_persistence(&self, _: &str) -> Result<Option<RelationPersistence>, String> {
        Ok(None)
    }
    fn table_persistence(&self, _: &str) -> Result<Option<RelationPersistence>, String> {
        Ok(Some(RelationPersistence::Permanent))
    }
    fn view_plan(&self, _: &str) -> Result<Option<QueryPlan>, SQLError> {
        Ok(None)
    }
    fn lookup_prepared(&self, _: &str) -> Option<UnifiedPlan> {
        None
    }
}
impl StatementTransactions for Inputs {
    fn transaction_depth(&self) -> usize {
        panic!("validation must not inspect the command transaction frame")
    }
    fn current_transaction_is_read_only(&self) -> bool {
        self.record("read-only");
        self.read_only
    }
    fn mark_transaction_snapshot_set(&self) {
        self.record("snapshot");
    }
    fn abort_after_error(&self, _: SQLError) -> SQLError {
        panic!("the caller owns error cleanup")
    }
    fn rollback(&self) -> Result<(), SQLError> {
        panic!("validation must not roll back its caller")
    }
}

fn plan(sql: &str) -> UnifiedPlan {
    UnifiedPlan::lower(uqa_sql::compile(sql).unwrap().remove(0))
}

#[test]
fn cancellation_precedes_namespace_rule_and_transaction_access() {
    let inputs = Inputs::default();
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let query = plan("WITH moved AS (DELETE FROM items RETURNING id) SELECT id FROM moved");
    assert!(validate_plan(&inputs.context(), &cancellation, &query)
        .unwrap_err()
        .to_string()
        .contains("cancel"));
    assert!(inputs.events.lock().is_empty());
}

#[test]
fn cte_syntax_and_rule_errors_precede_read_only_observation_and_snapshot_marking() {
    let cancellation = CancellationToken::default();
    let inputs = Inputs {
        read_only: true,
        rules_fail: true,
        ..Inputs::default()
    };
    let nested = plan("SELECT * FROM (WITH moved AS (DELETE FROM items RETURNING id) SELECT id FROM moved) nested");
    let error = validate_plan(&inputs.context(), &cancellation, &nested).unwrap_err();
    assert!(
        matches!(error, SQLError::Unsupported(message) if message == "WITH clause containing a data-modifying statement must be at the top level")
    );
    assert_eq!(*inputs.events.lock(), ["resolution"]);
    inputs.events.lock().clear();
    let query = plan("WITH moved AS (DELETE FROM items RETURNING id) SELECT id FROM moved");
    let error = validate_plan(&inputs.context(), &cancellation, &query).unwrap_err();
    assert!(matches!(error, SQLError::Internal(message) if message == "rule catalog unavailable"));
    assert_eq!(*inputs.events.lock(), ["resolution", "target", "rules"]);
}

#[test]
fn effects_are_captured_after_cte_rules_and_snapshot_marking_requires_success() {
    let cancellation = CancellationToken::default();
    let inputs = Inputs::default();
    let query = plan("WITH moved AS (DELETE FROM items RETURNING id) SELECT id FROM moved");
    validate_plan(&inputs.context(), &cancellation, &query).unwrap();
    assert_eq!(
        *inputs.events.lock(),
        [
            "resolution",
            "target",
            "rules",
            "effects",
            "read-only",
            "snapshot"
        ]
    );
    let reader = Inputs {
        read_only: true,
        ..Inputs::default()
    };
    let error = validate_plan(
        &reader.context(),
        &cancellation,
        &plan("CREATE TABLE items (id integer)"),
    )
    .unwrap_err();
    assert!(matches!(error, SQLError::Routine { sqlstate, .. } if sqlstate == "25006"));
    assert_eq!(
        *reader.events.lock(),
        ["resolution", "effects", "read-only"]
    );
}
