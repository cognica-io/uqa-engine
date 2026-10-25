//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Exhaustive physical executor for the unified SQL plan.

use crate::schema::ctas::CreateTableAsExecution;
use uqa_sql::ast::{CreateForeignServer, CreateForeignTable};
use uqa_sql::catalog::roles::RoleReference;
use uqa_sql::plan::{
    CommandPlan, DeletePlan, ExpressionPlan, InsertPlan, MergePlan, QueryPlan, UnifiedPlan,
    UpdatePlan,
};
use uqa_sql::{SQLError, SQLParam, SQLResult};

use super::context::StatementExecutionContext;

mod completion;
mod explain;
mod prepared;
mod types;
use crate::schema::view_creation::{self, MaterializedViewRegistration, ViewRegistration};

use crate::query::output::QueryOutput;
use crate::query::statement::{
    consumer::QueryOutputMode, execute_query_plan_output, execute_query_plan_with_ctes,
};
use crate::scalar::plan::{analyze_physical_call_arguments, eval_physical_call_arguments};

/// Statement output consumed inside the transaction, retaining a bounded query spool instead of a public row vector.
pub enum StatementOutput {
    Query(QueryOutput),
    Command(SQLResult),
    Mutation(crate::mutation::output::MutationOutput),
}

/// Owns top-level plan orchestration. Relational, mutation, DDL, procedural,
/// and prepared-plan execution all enter through this exhaustive dispatcher;
/// leaf executors never choose a second top-level SQL path.
pub struct UnifiedPlanExecutor<'engine, 'params, S: Clone + 'static> {
    context: StatementExecutionContext<'engine, S>,
    params: &'params [SQLParam],
    nested_statement: bool,
    privilege_subject: Option<RoleReference>,
    source_sql: Option<String>,
}

impl<'engine, 'params, S: Clone + Send + Sync + 'static> UnifiedPlanExecutor<'engine, 'params, S> {
    pub fn new(
        context: StatementExecutionContext<'engine, S>,
        params: &'params [SQLParam],
    ) -> Self {
        Self::with_nested_statement(context, params, false)
    }

    pub fn new_nested(
        context: StatementExecutionContext<'engine, S>,
        params: &'params [SQLParam],
    ) -> Self {
        Self::with_nested_statement(context, params, true)
    }

    pub fn with_nested_statement(
        context: StatementExecutionContext<'engine, S>,
        params: &'params [SQLParam],
        nested_statement: bool,
    ) -> Self {
        Self {
            context,
            params,
            nested_statement,
            privilege_subject: None,
            source_sql: None,
        }
    }

    /// Warn that `command`, whose effect lasts until its transaction ends, runs outside a transaction block, as `PostgreSQL`'s `WarnNoTransactionBlock` does. A statement nested in a function or in a multi-statement query has a transaction to last for and is not warned about.
    fn warn_outside_transaction_block(&self, command: &str) {
        if !self.nested_statement && !self.context.portals.state.in_transaction_block() {
            self.context.runtime.notices.push(
                uqa_sql::semantics::effects::transaction_blocks::no_transaction_block_warning(
                    command,
                ),
            );
        }
    }

    pub fn with_privilege_subject(mut self, subject: &RoleReference) -> Self {
        self.privilege_subject = Some(subject.clone());
        self
    }

    pub fn with_source_sql(mut self, sql: &str) -> Self {
        self.source_sql = Some(sql.to_string());
        self
    }

    pub fn with_optional_source_sql(mut self, sql: Option<&str>) -> Self {
        self.source_sql = sql.map(str::to_string);
        self
    }

    fn execute_query(&self, query: &QueryPlan) -> Result<SQLResult, SQLError> {
        if self.context.validation.transactions.transaction_depth() != 0 {
            crate::query::locking::lock_query_relations(
                self.context.queries.row_lock_context(),
                query,
            )?;
        }
        let mut ctes = self
            .context
            .queries
            .statement_scope(self.privilege_subject.as_ref());
        execute_query_plan_with_ctes(
            &self.context.queries.query_context(),
            query,
            self.params,
            &mut ctes,
        )
    }

    pub fn execute_query_to_spill(&self, plan: &UnifiedPlan) -> Result<QueryOutput, SQLError> {
        self.execute_query_output(plan, QueryOutputMode::SharedSpill)
    }

    fn execute_query_output(
        &self,
        plan: &UnifiedPlan,
        output: QueryOutputMode<'_, S>,
    ) -> Result<QueryOutput, SQLError> {
        super::validation::validate_plan(
            &self.context.validation,
            self.context.runtime.cancellation,
            plan,
        )?;
        let UnifiedPlan::Query(query) = plan else {
            return Err(SQLError::Unsupported(
                "SQL cursor accepts exactly one query statement".into(),
            ));
        };
        if self.context.validation.transactions.transaction_depth() != 0 {
            crate::query::locking::lock_query_relations(
                self.context.queries.row_lock_context(),
                query,
            )?;
        }
        let mut ctes = self
            .context
            .queries
            .statement_scope(self.privilege_subject.as_ref());
        execute_query_plan_output(
            &self.context.queries.query_context(),
            query,
            self.params,
            &mut ctes,
            output,
        )
    }

    fn execute_insert(
        &self,
        plan: &InsertPlan,
        destination: Option<crate::mutation::returning::ReturningDestination<'_>>,
    ) -> Result<crate::mutation::output::MutationOutput, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        crate::mutation::entry::run_insert(
            &self.context.mutations.mutation_context(),
            plan,
            self.params,
            None,
            destination,
        )
    }

    fn execute_update(
        &self,
        plan: &UpdatePlan,
        destination: Option<crate::mutation::returning::ReturningDestination<'_>>,
    ) -> Result<crate::mutation::output::MutationOutput, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        crate::mutation::entry::run_update(
            &self.context.mutations.mutation_context(),
            plan,
            self.params,
            None,
            destination,
        )
    }

    fn execute_delete(
        &self,
        plan: &DeletePlan,
        destination: Option<crate::mutation::returning::ReturningDestination<'_>>,
    ) -> Result<crate::mutation::output::MutationOutput, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        crate::mutation::entry::run_delete(
            &self.context.mutations.mutation_context(),
            plan,
            self.params,
            None,
            destination,
        )
    }

    fn apply_statement_privilege_subject(
        &self,
        statement_subject: &mut Option<RoleReference>,
        target_subject: &mut Option<RoleReference>,
    ) {
        let Some(subject) = self.privilege_subject.as_ref() else {
            return;
        };
        statement_subject.get_or_insert_with(|| subject.clone());
        target_subject.get_or_insert_with(|| subject.clone());
    }

    fn execute_create_view(
        &self,
        name: &str,
        column_names: &[String],
        query: &QueryPlan,
        or_replace: bool,
        persistence: uqa_sql::ast::RelationPersistence,
        options: &[(String, String)],
    ) -> Result<SQLResult, SQLError> {
        view_creation::register_view_plan(
            self.context.schemas.views,
            ViewRegistration {
                name,
                column_names,
                plan: query.clone(),
                or_replace,
                persistence,
                options,
                params: self.params,
            },
        )?;
        Ok(SQLResult::empty())
    }

    fn execute_show_variable(&self, name: &str) -> Result<SQLResult, SQLError> {
        super::show::show(self.context.validation.session, name)
    }

    fn execute_create_foreign_server(
        &self,
        statement: &CreateForeignServer,
    ) -> Result<SQLResult, SQLError> {
        crate::schema::foreign_creation::entry::register_foreign_server_statement(
            self.context.foreign,
            statement.clone(),
        )?;
        Ok(SQLResult::empty())
    }

    fn execute_create_foreign_table(
        &self,
        statement: &CreateForeignTable,
    ) -> Result<SQLResult, SQLError> {
        crate::schema::foreign_creation::entry::register_foreign_table_statement(
            self.context.foreign,
            statement.clone(),
        )?;
        Ok(SQLResult::empty())
    }

    fn execute_merge(
        &self,
        plan: &MergePlan,
        destination: Option<crate::mutation::returning::ReturningDestination<'_>>,
    ) -> Result<crate::mutation::output::MutationOutput, SQLError> {
        let mut plan = plan.clone();
        self.apply_statement_privilege_subject(
            &mut plan.statement_privilege_subject,
            &mut plan.target_privilege_subject,
        );
        crate::mutation::entry::run_merge(
            &self.context.mutations.mutation_context(),
            plan,
            self.params,
            None,
            destination,
        )
    }

    fn execute_call(
        &self,
        name: &str,
        arguments: &[ExpressionPlan],
    ) -> Result<SQLResult, SQLError> {
        uqa_sql::routines::call::validate_call_arguments(arguments)?;
        uqa_sql::routines::call::reject_set_call_arguments(
            self.context.routines.resolution,
            arguments,
            self.params,
        )?;
        let scope = self.context.queries.statement_scope(None);
        let (call_arguments, explicit_variadic) = analyze_physical_call_arguments(arguments)?;
        let argument_types = uqa_sql::routines::call::infer_call_argument_types(
            arguments,
            &call_arguments,
            &mut |argument| {
                crate::query::binding::bind_expression_plan_type(
                    self.context.routines.resolution,
                    argument,
                    self.params,
                    &scope,
                )
            },
        )?;
        self.context
            .queries
            .with_expression_context(&scope, self.params, &mut |context| {
                let args = eval_physical_call_arguments(arguments, context)?;
                crate::routines::invocation::run_call(
                    &self.context.routines.inputs.invocation_context(),
                    name,
                    &args,
                    &argument_types,
                    explicit_variadic,
                    self.nested_statement,
                )
            })
    }

    fn validate_command_target(&self, command: &CommandPlan) -> Result<(), SQLError> {
        if uqa_sql::semantics::virtual_relation_mutation_candidate(command) {
            let ctes = self.context.queries.statement_scope(None);
            if let Some(error) = crate::catalog::projection::virtual_relation_mutation_error(
                &ctes.catalog_read_view()?,
                &ctes.relation_name_resolution()?,
                command,
            )? {
                // Semantic errors precede the view's rewrite-time mutation rejection.
                crate::query::binding::analyze_command_parameters(
                    self.context.routines.resolution,
                    command,
                    self.params,
                    &ctes,
                )?;
                return Err(error);
            }
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "preserves SELECT schema and row identity"
    )]
    fn execute_command(&self, command: &CommandPlan) -> Result<SQLResult, SQLError> {
        self.validate_command_target(command)?;
        match command {
            CommandPlan::Empty => Ok(SQLResult::empty()),
            CommandPlan::CreateTable(statement) => {
                crate::schema::table_creation::entry::run_create_table(
                    self.context.schemas.creation,
                    statement.as_ref().clone(),
                )
            }
            CommandPlan::CreateTableIfNotExists(statement) => {
                crate::schema::table_creation::entry::run_create_table_if_not_exists(
                    self.context.schemas.creation,
                    statement.clone(),
                )
            }
            CommandPlan::CreateIndex(statement) => {
                crate::schema::indexes::creation::run_create_index(
                    &self.context.schemas.inputs.index_creation_context(),
                    statement.clone(),
                )
            }
            CommandPlan::RenameIndex(statement) => {
                crate::schema::table_alteration::entry::run_rename_index(
                    &self.context.schemas.inputs.table_alter_entry_context(),
                    statement,
                )
            }
            CommandPlan::Insert(plan) => self.execute_insert(plan, None)?.into_sql_result(),
            CommandPlan::Update(plan) => self.execute_update(plan, None)?.into_sql_result(),
            CommandPlan::Delete(plan) => self.execute_delete(plan, None)?.into_sql_result(),
            CommandPlan::Drop(statement) => crate::schema::removal::entry::run_drop_statement(
                self.context.schemas.removal,
                statement.clone(),
            ),
            CommandPlan::AlterRoutineOwner(statement) => {
                crate::routines::privileges::alter_sql_routine_owner(
                    &self.context.routines.inputs.privilege_context(),
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::RenameRoutine(statement) => {
                self.context
                    .routines
                    .transactions
                    .with_rename(Box::new(|context| {
                        crate::routines::rename::rename_sql_routine(context, statement)
                    }))?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantRoutine(statement) => {
                crate::routines::privileges::grant_sql_routine(
                    &self.context.routines.inputs.privilege_context(),
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantTable(statement) => {
                crate::catalog::security::table_grants::grant_table_privileges(
                    self.context.table_privileges,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantSequence(statement) => {
                self.context
                    .schemas
                    .inputs
                    .sequence_privilege_context()
                    .grant_sequence_privileges(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantDatabase(statement) => {
                crate::catalog::security::database_lifecycle::grant_database_privileges(
                    &self.context.schemas.inputs.database_privilege_context(),
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantSchema(statement) => {
                crate::schema::namespaces::privileges::grant_schema_privileges(
                    &self.context.schemas.inputs.schema_privilege_context(),
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantRole(statement) => {
                crate::catalog::security::role_lifecycle::grant_roles(
                    &self.context.roles,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateRole(statement) => {
                crate::catalog::security::role_lifecycle::create_role(
                    &self.context.roles,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterRole(statement) => {
                crate::catalog::security::role_lifecycle::alter_role(
                    &self.context.roles,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::RenameRole(statement) => {
                crate::catalog::security::role_lifecycle::rename_role(
                    &self.context.roles,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropRole(statement) => {
                crate::catalog::security::role_lifecycle::drop_roles(
                    &self.context.roles,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateTrigger(statement) => {
                self.context.events.register_trigger(statement.clone())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropTrigger(statement) => {
                self.context.events.drop_trigger_sql(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateRule(statement) => {
                self.context.events.register_rule(statement.clone())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropRule(statement) => {
                self.context.events.drop_rule_sql(statement)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterTable(statement) => {
                crate::schema::table_alteration::entry::run_alter_table(
                    &self.context.schemas.inputs.table_alter_entry_context(),
                    (**statement).clone(),
                )
            }
            CommandPlan::AlterForeignTable(statement) => {
                crate::schema::foreign_table_alteration::alter_foreign_table(
                    self.context.schemas.foreign_alteration,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterView(statement) => {
                crate::schema::view_alteration::alter_view(
                    self.context.schemas.view_alteration,
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateView {
                name,
                column_names,
                query,
                or_replace,
                persistence,
                options,
            } => self.execute_create_view(
                name,
                column_names,
                query,
                *or_replace,
                *persistence,
                options,
            ),
            CommandPlan::CreateMaterializedView {
                name,
                column_names,
                if_not_exists,
                with_no_data,
                options,
                query,
            } => {
                let populated_rows = view_creation::register_materialized_view_plan(
                    self.context.schemas.views,
                    MaterializedViewRegistration {
                        name,
                        column_names,
                        plan: (**query).clone(),
                        executable: None,
                        if_not_exists: *if_not_exists,
                        with_no_data: *with_no_data,
                        options,
                        params: self.params,
                    },
                )?;
                let completion = populated_rows.map_or_else(
                    || "CREATE MATERIALIZED VIEW".to_string(),
                    |rows| format!("SELECT {rows}"),
                );
                Ok(SQLResult::from_affected(populated_rows.unwrap_or(0))
                    .with_command_tag(completion))
            }
            CommandPlan::RefreshMaterializedView {
                name,
                concurrently,
                with_no_data,
            } => {
                view_creation::refresh_materialized_view(
                    self.context.schemas.views,
                    name,
                    *concurrently,
                    *with_no_data,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateSchema {
                name,
                if_not_exists,
                authorization,
            } => crate::schema::namespaces::create_schema(
                &self.context.schemas.inputs.schema_creation_context(),
                name.as_deref(),
                *if_not_exists,
                authorization.as_ref(),
            ),
            CommandPlan::AlterSchemaOwner { name, new_owner } => self
                .context
                .schemas
                .owners
                .with_owner_write(Box::new(|context| {
                    crate::schema::namespaces::alter_schema_owner(context, name, new_owner)?;
                    Ok(SQLResult::empty())
                })),
            CommandPlan::RenameSchema { name, new_name } => self
                .context
                .schemas
                .renames
                .with_rename_write(Box::new(|context| {
                    crate::schema::namespaces::rename::rename_schema(context, name, new_name)?;
                    Ok(SQLResult::empty())
                })),
            CommandPlan::Notify { channel, payload } => {
                self.context.notifications.notify(channel, payload)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Listen { channel } => {
                super::notifications::require_sql_listener_session(
                    self.context
                        .notifications
                        .notification_subscriptions_required(),
                )?;
                self.context.notifications.listen(channel)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Unlisten { channel } => {
                super::notifications::require_sql_listener_session(
                    self.context
                        .notifications
                        .notification_subscriptions_required(),
                )?;
                self.context.notifications.unlisten(channel.as_deref())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::SetVariable {
                name,
                value,
                local,
                is_default,
            } => {
                if *local {
                    self.warn_outside_transaction_block("SET LOCAL");
                }
                self.context.settings.set_runtime_parameter(
                    name,
                    (!is_default).then_some(value.as_str()),
                    *local,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::ResetVariable { name } => {
                self.context
                    .settings
                    .set_runtime_parameter(name, None, false)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::ResetAllVariables => {
                self.context.settings.reset_all_variables();
                Ok(SQLResult::empty())
            }
            CommandPlan::SetConstraints {
                constraints,
                deferred,
            } => {
                self.context.controls.set_constraints(
                    constraints,
                    *deferred,
                    self.nested_statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::ShowVariable { name } => self.execute_show_variable(name),
            CommandPlan::Discard { target } => {
                self.context.settings.discard(*target)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Load { library } => {
                self.context.settings.load_library(library)?;
                Ok(SQLResult::empty())
            }
            CommandPlan::Explain {
                body,
                options,
                unbound_parameters,
            } => self.execute_explain(body, options, unbound_parameters.as_deref()),
            CommandPlan::Analyze { table } => {
                let context = self.context.schemas.inputs.vacuum_execution_context();
                let targets = if let Some(requested) = table.as_deref() {
                    let Some((canonical, "table")) =
                        context.catalog.resolve_relation_kind(requested)?
                    else {
                        return Err(SQLError::UnknownTable(requested.to_string()));
                    };
                    context.privileges.ensure_maintain(&canonical)?;
                    vec![requested.to_string()]
                } else {
                    context.statistics.table_names("analyze")?
                };
                for target in targets {
                    context
                        .statistics
                        .analyze_target(&target, &[], true)
                        .map_err(|error| {
                            uqa_sql::catalog::errors::storage_error("ANALYZE", &error)
                        })?;
                }
                Ok(SQLResult::empty())
            }
            CommandPlan::LockTable(statement) => super::table_locks::execute(
                self.context.schemas.inputs.table_lock_context(),
                statement,
                self.nested_statement,
            ),
            CommandPlan::Vacuum(statement) => crate::maintenance::run_vacuum(
                &self.context.schemas.inputs.vacuum_execution_context(),
                statement,
            ),
            CommandPlan::Truncate {
                tables,
                cascade,
                restart_identity,
            } => crate::schema::truncate::execute(
                &self.context.schemas.inputs.truncate_context(),
                tables,
                *cascade,
                *restart_identity,
            ),
            CommandPlan::Transaction(statement) => {
                if let Some(completed) = super::transactions::complete_idle_transaction(
                    self.context.validation.transactions,
                    self.context.runtime.notices,
                    statement,
                ) {
                    return Ok(completed);
                }
                if matches!(
                    statement,
                    uqa_sql::ast::TransactionStmt::SetCharacteristics(_)
                ) {
                    self.warn_outside_transaction_block("SET TRANSACTION");
                }
                self.context
                    .controls
                    .run_transaction_statement(statement.clone())?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DeclareCursor {
                name,
                binary,
                scroll,
                hold,
                query,
            } => super::portal::declaration::declare_session_portal(
                &self
                    .context
                    .portals
                    .clone()
                    .with_source_sql(self.source_sql.as_deref()),
                self.params,
                name,
                *binary,
                *scroll,
                *hold,
                query,
            ),
            CommandPlan::FetchCursor(fetch) => {
                self.context.portals.state.fetch_session_portal(fetch)
            }
            CommandPlan::CloseCursor { name } => {
                if let Some(name) = name {
                    self.context.portals.state.close_session_portal(name)?;
                } else {
                    self.context.portals.state.close_all_session_portals();
                }
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateSequence(statement) => {
                crate::schema::sequences::entry::run_create_sequence(
                    self.context.schemas.sequence_creation,
                    self.context.runtime.notices,
                    statement,
                )
            }
            CommandPlan::CreateDomain(_)
            | CommandPlan::AlterDomain(_)
            | CommandPlan::CreateEnum(_)
            | CommandPlan::CommentType(_)
            | CommandPlan::CreateShellType(_)
            | CommandPlan::CreateCompositeType(_)
            | CommandPlan::AlterEnum(_)
            | CommandPlan::AlterTypeObject(_)
            | CommandPlan::GrantType(_) => self.execute_type_command(command),
            CommandPlan::AlterSequence(statement) => {
                crate::schema::sequences::entry::run_alter_sequence(
                    self.context.schemas.sequence_alteration,
                    self.context.runtime.notices,
                    statement,
                )
            }
            CommandPlan::CreateTableAs {
                name,
                if_not_exists,
                column_names,
                with_no_data,
                persistence,
                on_commit,
                query,
                ..
            } => crate::schema::ctas::entry::run_create_table_as(
                self.context.schemas.tables_as,
                CreateTableAsExecution {
                    name,
                    if_not_exists: *if_not_exists,
                    column_names,
                    with_no_data: *with_no_data,
                    persistence: *persistence,
                    on_commit: *on_commit,
                    query,
                    executable: None,
                    params: self.params,
                },
            ),
            CommandPlan::Prepare {
                name,
                parameter_types,
                body,
            } => self.execute_prepare(name, parameter_types, body),
            CommandPlan::Execute { name, params } => self.execute_prepared(name, params),
            CommandPlan::Deallocate { name } => self.execute_deallocate(name.as_deref()),
            CommandPlan::CreateForeignWrapper(statement) => {
                crate::schema::foreign_creation::entry::register_foreign_wrapper_statement(
                    self.context.foreign,
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateForeignServer(statement) => {
                self.execute_create_foreign_server(statement)
            }
            CommandPlan::CreateForeignTable(statement) => {
                self.execute_create_foreign_table(statement)
            }
            CommandPlan::CreateForeignTableDefinition(statement) => {
                crate::schema::foreign_creation::entry::register_deferred_foreign_table(
                    self.context.foreign,
                    statement.clone(),
                )
                .map(|()| SQLResult::empty())
            }
            CommandPlan::Merge(plan) => self.execute_merge(plan, None)?.into_sql_result(),
            CommandPlan::CreateFunction(definition) => {
                crate::routines::registration::register_sql_function(
                    &self.context.routines.inputs.registration_context(),
                    (**definition).clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DropFunction(statement) => {
                self.context
                    .routines
                    .transactions
                    .with_removal(Box::new(|context| {
                        crate::routines::removal::drop_sql_functions(context, statement)
                    }))?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterRoutine(statement) => {
                crate::routines::registration::alter_sql_routine(
                    &self.context.routines.inputs.registration_context(),
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::DoBlock { language, body } => crate::routines::invocation::run_do_block(
                &self.context.routines.inputs.anonymous_block_context(),
                language,
                body,
                self.nested_statement,
            ),
            CommandPlan::Call { name, args } => self.execute_call(name, args),
        }
    }
}
