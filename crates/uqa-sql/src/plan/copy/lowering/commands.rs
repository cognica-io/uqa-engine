//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};
use crate::{
    ast::Statement,
    plan::{AggregateClassifier, CommandPlan},
};

impl Copying<'_> {
    #[expect(
        clippy::too_many_lines,
        reason = "exhaustive lowering preserves command payloads and their constructor fields"
    )]
    pub(super) fn lower_command(
        &mut self,
        statement: &Statement,
        aggregates: &dyn AggregateClassifier,
    ) -> Result<CommandPlan> {
        self.check()?;
        Ok(match statement {
            Statement::Select(_) | Statement::Values { .. } => {
                unreachable!("query lowering selected by statement owner")
            }
            Statement::CreateTableIfNotExists(value) => CommandPlan::CreateTableIfNotExists(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateIndex(value) => CommandPlan::CreateIndex(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::RenameIndex(value) => CommandPlan::RenameIndex(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::Drop(value) => {
                CommandPlan::Drop(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::AlterView(value) => {
                CommandPlan::AlterView(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::RefreshMaterializedView {
                name,
                concurrently,
                with_no_data,
            } => CommandPlan::RefreshMaterializedView {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                concurrently: self
                    .field("concurrently", |copying| concurrently.copy_syntax(copying))?,
                with_no_data: self
                    .field("with_no_data", |copying| with_no_data.copy_syntax(copying))?,
            },
            Statement::CreateSchema {
                name,
                if_not_exists,
                authorization,
            } => CommandPlan::CreateSchema {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                if_not_exists: self.field("if_not_exists", |copying| {
                    if_not_exists.copy_syntax(copying)
                })?,
                authorization: self.field("authorization", |copying| {
                    authorization.copy_syntax(copying)
                })?,
            },
            Statement::AlterSchemaOwner { name, new_owner } => CommandPlan::AlterSchemaOwner {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                new_owner: self.field("new_owner", |copying| new_owner.copy_syntax(copying))?,
            },
            Statement::RenameSchema { name, new_name } => CommandPlan::RenameSchema {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                new_name: self.field("new_name", |copying| new_name.copy_syntax(copying))?,
            },
            Statement::Notify { channel, payload } => CommandPlan::Notify {
                channel: self.field("channel", |copying| channel.copy_syntax(copying))?,
                payload: self.field("payload", |copying| payload.copy_syntax(copying))?,
            },
            Statement::Listen { channel } => CommandPlan::Listen {
                channel: self.field("channel", |copying| channel.copy_syntax(copying))?,
            },
            Statement::Unlisten { channel } => CommandPlan::Unlisten {
                channel: self.field("channel", |copying| channel.copy_syntax(copying))?,
            },
            Statement::SetVariable {
                name,
                value,
                local,
                is_default,
            } => CommandPlan::SetVariable {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                value: self.field("value", |copying| value.copy_syntax(copying))?,
                local: self.field("local", |copying| local.copy_syntax(copying))?,
                is_default: self.field("is_default", |copying| is_default.copy_syntax(copying))?,
            },
            Statement::ResetVariable { name } => CommandPlan::ResetVariable {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
            },
            Statement::ResetAllVariables => CommandPlan::ResetAllVariables,
            Statement::SetConstraints {
                constraints,
                deferred,
            } => CommandPlan::SetConstraints {
                constraints: self
                    .field("constraints", |copying| constraints.copy_syntax(copying))?,
                deferred: self.field("deferred", |copying| deferred.copy_syntax(copying))?,
            },
            Statement::ShowVariable { name } => CommandPlan::ShowVariable {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
            },
            Statement::Discard { target } => CommandPlan::Discard {
                target: self.field("target", |copying| target.copy_syntax(copying))?,
            },
            Statement::Load { library } => CommandPlan::Load {
                library: self.field("library", |copying| library.copy_syntax(copying))?,
            },
            Statement::Analyze { table } => CommandPlan::Analyze {
                table: self.field("table", |copying| table.copy_syntax(copying))?,
            },
            Statement::Vacuum(value) => {
                CommandPlan::Vacuum(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::LockTable(value) => {
                CommandPlan::LockTable(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::Truncate {
                tables,
                cascade,
                restart_identity,
            } => CommandPlan::Truncate {
                tables: self.field("tables", |copying| tables.copy_syntax(copying))?,
                cascade: self.field("cascade", |copying| cascade.copy_syntax(copying))?,
                restart_identity: self.field("restart_identity", |copying| {
                    restart_identity.copy_syntax(copying)
                })?,
            },
            Statement::Transaction(value) => CommandPlan::Transaction(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::FetchCursor(value) => CommandPlan::FetchCursor(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CloseCursor { name } => CommandPlan::CloseCursor {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
            },
            Statement::CreateSequence(value) => CommandPlan::CreateSequence(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateDomain(value) => CommandPlan::CreateDomain(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::AlterDomain(value) => CommandPlan::AlterDomain(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateShellType(value) => CommandPlan::CreateShellType(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CommentType(value) => CommandPlan::CommentType(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateEnum(value) => {
                CommandPlan::CreateEnum(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::CreateCompositeType(value) => CommandPlan::CreateCompositeType(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::AlterEnum(value) => {
                CommandPlan::AlterEnum(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::AlterTypeObject(value) => CommandPlan::AlterTypeObject(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::GrantType(value) => {
                CommandPlan::GrantType(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::AlterSequence(value) => CommandPlan::AlterSequence(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::Deallocate { name } => CommandPlan::Deallocate {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
            },
            Statement::CreateForeignWrapper(value) => CommandPlan::CreateForeignWrapper(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateForeignServer(value) => CommandPlan::CreateForeignServer(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateForeignTable(value) => CommandPlan::CreateForeignTable(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateForeignTableDefinition(value) => {
                CommandPlan::CreateForeignTableDefinition(
                    self.field("value0", |copying| value.copy_syntax(copying))?,
                )
            }
            Statement::AlterForeignTable(value) => CommandPlan::AlterForeignTable(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateFunction(value) => CommandPlan::CreateFunction(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::DropFunction(value) => CommandPlan::DropFunction(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::AlterRoutine(value) => CommandPlan::AlterRoutine(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::AlterRoutineOwner(value) => CommandPlan::AlterRoutineOwner(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::RenameRoutine(value) => CommandPlan::RenameRoutine(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::GrantRoutine(value) => CommandPlan::GrantRoutine(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::GrantTable(value) => {
                CommandPlan::GrantTable(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::GrantSequence(value) => CommandPlan::GrantSequence(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::GrantDatabase(value) => CommandPlan::GrantDatabase(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::GrantSchema(value) => CommandPlan::GrantSchema(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::GrantRole(value) => {
                CommandPlan::GrantRole(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::CreateRole(value) => {
                CommandPlan::CreateRole(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::AlterRole(value) => {
                CommandPlan::AlterRole(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::RenameRole(value) => {
                CommandPlan::RenameRole(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::DropRole(value) => {
                CommandPlan::DropRole(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::CreateTrigger(value) => CommandPlan::CreateTrigger(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::DropTrigger(value) => CommandPlan::DropTrigger(
                self.field("value0", |copying| value.copy_syntax(copying))?,
            ),
            Statement::CreateRule(value) => {
                CommandPlan::CreateRule(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::DropRule(value) => {
                CommandPlan::DropRule(self.field("value0", |copying| value.copy_syntax(copying))?)
            }
            Statement::DoBlock { language, body } => CommandPlan::DoBlock {
                language: self.field("language", |copying| language.copy_syntax(copying))?,
                body: self.field("body", |copying| body.copy_syntax(copying))?,
            },
            Statement::Empty => CommandPlan::Empty,
            Statement::CreateTable(value) => {
                CommandPlan::CreateTable(self.field("value0", |copying| {
                    copying.boxed(|copying| value.copy_syntax(copying))
                })?)
            }
            Statement::AlterTable(value) => {
                CommandPlan::AlterTable(self.field("value0", |copying| {
                    copying.boxed(|copying| value.copy_syntax(copying))
                })?)
            }
            Statement::Insert(statement) => {
                CommandPlan::Insert(self.field("value0", |copying| {
                    copying.boxed(|copying| copying.lower_insert(statement, aggregates))
                })?)
            }
            Statement::Update(statement) => {
                CommandPlan::Update(self.field("value0", |copying| {
                    copying.boxed(|copying| copying.lower_update(statement, aggregates))
                })?)
            }
            Statement::Delete(statement) => {
                CommandPlan::Delete(self.field("value0", |copying| {
                    copying.boxed(|copying| copying.lower_delete(statement, aggregates))
                })?)
            }
            Statement::Merge(statement) => CommandPlan::Merge(self.field("value0", |copying| {
                copying.boxed(|copying| copying.lower_merge(statement, aggregates))
            })?),
            Statement::CreateView {
                name,
                column_names,
                body,
                or_replace,
                persistence,
                options,
            } => {
                let query = self.field("query", |copying| {
                    copying.boxed(|copying| copying.lower_nested_query(body, aggregates))
                })?;
                CommandPlan::CreateView {
                    name: self.field("name", |copying| name.copy_syntax(copying))?,
                    column_names: self
                        .field("column_names", |copying| column_names.copy_syntax(copying))?,
                    query,
                    or_replace: *or_replace,
                    persistence: *persistence,
                    options: self.field("options", |copying| options.copy_syntax(copying))?,
                }
            }
            Statement::CreateMaterializedView {
                name,
                column_names,
                if_not_exists,
                with_no_data,
                options,
                body,
            } => CommandPlan::CreateMaterializedView {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                column_names: self
                    .field("column_names", |copying| column_names.copy_syntax(copying))?,
                if_not_exists: *if_not_exists,
                with_no_data: *with_no_data,
                options: self.field("options", |copying| options.copy_syntax(copying))?,
                query: self.field("query", |copying| {
                    copying.boxed(|copying| copying.lower_nested_query(body, aggregates))
                })?,
            },
            Statement::Explain { options, body } => CommandPlan::Explain {
                options: self.field("options", |copying| options.copy_syntax(copying))?,
                body: self.field("body", |copying| {
                    copying.boxed(|copying| copying.lower_statement(body, aggregates))
                })?,
                unbound_parameters: self.field("unbound_parameters", |_| Ok(None))?,
            },
            Statement::DeclareCursor(cursor) => CommandPlan::DeclareCursor {
                name: self.field("name", |copying| cursor.name.copy_syntax(copying))?,
                binary: cursor.binary,
                scroll: cursor.scroll,
                hold: cursor.hold,
                query: self.field("query", |copying| {
                    copying.boxed(|copying| copying.lower_nested_query(&cursor.query, aggregates))
                })?,
            },
            Statement::CreateTableAs {
                name,
                if_not_exists,
                column_names,
                with_no_data,
                select_into,
                persistence,
                on_commit,
                body,
            } => CommandPlan::CreateTableAs {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                if_not_exists: *if_not_exists,
                column_names: self
                    .field("column_names", |copying| column_names.copy_syntax(copying))?,
                with_no_data: *with_no_data,
                select_into: *select_into,
                persistence: *persistence,
                on_commit: *on_commit,
                query: self.field("query", |copying| {
                    copying.boxed(|copying| copying.lower_nested_query(body, aggregates))
                })?,
            },
            Statement::Prepare {
                name,
                parameter_types,
                body,
            } => {
                let body = self.field("body", |copying| {
                    copying.boxed(|copying| copying.lower_statement(body, aggregates))
                })?;
                CommandPlan::Prepare {
                    name: self.field("name", |copying| name.copy_syntax(copying))?,
                    parameter_types: self.field("parameter_types", |copying| {
                        parameter_types.copy_syntax(copying)
                    })?,
                    body,
                }
            }
            Statement::Execute { name, params } => CommandPlan::Execute {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                params: self.field("params", |copying| {
                    copying.map(params.iter(), |copying, expression| {
                        copying.lower_expression(expression, aggregates)
                    })
                })?,
            },
            Statement::Call { name, args } => CommandPlan::Call {
                name: self.field("name", |copying| name.copy_syntax(copying))?,
                args: self.field("args", |copying| {
                    copying.map(args.iter(), |copying, expression| {
                        copying.lower_expression(expression, aggregates)
                    })
                })?,
            },
        })
    }
}
