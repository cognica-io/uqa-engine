//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Type commands use the same schema transaction capabilities as the enclosing statement.

use super::UnifiedPlanExecutor;
use uqa_sql::{plan::CommandPlan, SQLError, SQLResult};

impl<S: Clone + Send + Sync + 'static> UnifiedPlanExecutor<'_, '_, S> {
    pub(super) fn execute_type_command(
        &self,
        command: &CommandPlan,
    ) -> Result<SQLResult, SQLError> {
        match command {
            CommandPlan::CreateDomain(statement) => {
                crate::schema::domains::create_domain(
                    &self.context.schemas.inputs.domain_creation_context(),
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterDomain(statement) => {
                crate::schema::domains::alteration::alter_domain(
                    &self.context.schemas.inputs.domain_alter_context(),
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateEnum(statement) => {
                crate::schema::enums::create_enum(
                    &self.context.schemas.inputs.enum_type_context(),
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CommentType(statement) => {
                crate::schema::types::comments::comment(
                    &self.context.schemas.inputs.type_lifecycle_context(),
                    statement.kind,
                    &statement.name,
                    statement.description.as_deref(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateShellType(name) => {
                crate::schema::shell_types::create_shell(
                    &self.context.schemas.inputs.shell_type_context(),
                    name,
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::CreateCompositeType(statement) => {
                crate::schema::composites::create_composite_type(
                    &self.context.schemas.inputs.composite_type_context(),
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterEnum(statement) => {
                crate::schema::enums::alter_enum(
                    &self.context.schemas.inputs.enum_type_context(),
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::AlterTypeObject(statement) => {
                if let uqa_sql::ast::AlterTypeObjectAction::RenameAttribute { from, to } =
                    &statement.action
                {
                    return crate::schema::composites::renaming::rename_attribute(
                        &self
                            .context
                            .schemas
                            .inputs
                            .type_lifecycle_context()
                            .composite_attributes,
                        &self.context.schemas.inputs.table_alter_entry_context(),
                        &statement.name,
                        from,
                        to,
                    );
                }
                crate::schema::types::alter_type_object(
                    &self.context.schemas.inputs.type_lifecycle_context(),
                    statement.clone(),
                )?;
                Ok(SQLResult::empty())
            }
            CommandPlan::GrantType(statement) => {
                crate::schema::types::grant_type(
                    &self.context.schemas.inputs.type_lifecycle_context(),
                    statement,
                )?;
                Ok(SQLResult::empty())
            }
            _ => Err(SQLError::Internal(
                "non-type command reached type execution".into(),
            )),
        }
    }
}
