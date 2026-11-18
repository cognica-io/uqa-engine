//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::{Copying, Result, SyntaxCopy};
use crate::plan::copy::stored_routine::Lists;

impl crate::ast::CreateFunction {
    pub(in crate::plan::copy) fn copy_definition_with(
        &self,
        copying: &mut Copying<'_>,
        lists: Lists,
        params: impl FnOnce(&mut Copying<'_>) -> Result<Vec<crate::ast::FunctionParam>>,
        body: impl FnOnce(&mut Copying<'_>) -> Result<crate::ast::FunctionBody>,
    ) -> Result<Self> {
        macro_rules! field {
            ($name:ident) => {
                copying.field(stringify!($name), |child| self.$name.copy_syntax(child))?
            };
        }
        copying.check()?;
        Ok(Self {
            object_id: field!(object_id),
            catalog_revision: field!(catalog_revision),
            catalog_oid: field!(catalog_oid),
            name: field!(name),
            or_replace: field!(or_replace),
            is_procedure: field!(is_procedure),
            params: copying.field("params", params)?,
            returns: field!(returns),
            return_type_reference: field!(return_type_reference),
            return_written_type: field!(return_written_type),
            language: field!(language),
            body: copying.field("body", body)?,
            body_origin: copying.field("body_origin", |child| {
                syntax_field!(child, self.body_origin, None)
            })?,
            sql_body_form: field!(sql_body_form),
            creation_search_path: copying.field("creation_search_path", |child| {
                lists.copy(&self.creation_search_path, child)
            })?,
            volatility: field!(volatility),
            strict: field!(strict),
            owner: field!(owner),
            security: field!(security),
            parallel: field!(parallel),
            support: field!(support),
            cost: field!(cost),
            rows: field!(rows),
            config: copying.field("config", |child| lists.copy(&self.config, child))?,
            config_actions: field!(config_actions),
            attribute_clauses: field!(attribute_clauses),
            execute_acl: copying.field("execute_acl", |child| {
                self.execute_acl
                    .as_ref()
                    .map(|acl| lists.copy(acl, child))
                    .transpose()
            })?,
        })
    }
}
impl SyntaxCopy for crate::ast::CreateFunction {
    fn copy_syntax(&self, copying: &mut Copying<'_>) -> Result<Self> {
        self.copy_definition_with(
            copying,
            Lists::Inline,
            |child| self.params.copy_syntax(child),
            |child| self.body.copy_syntax(child),
        )
    }
}

syntax_struct!(crate::ast::DropFunctionItem { name, arg_types });

syntax_struct!(crate::ast::DropFunctionStmt {
    is_procedure,
    if_exists,
    cascade,
    items
});

syntax_enum!(crate::ast::FunctionBody {
    Source(value0),
    Statements(value0)
});

syntax_struct!(crate::ast::FunctionParam {
    name,
    type_name,
    type_reference,
    written_type,
    mode,
    default,
    default_type
});

syntax_enum!(crate::ast::FunctionParamMode {
    In,
    Out,
    InOut,
    Variadic,
    Table
});

syntax_enum!(crate::ast::FunctionReturns {
    None,
    Scalar { type_name },
    SetOf { type_name },
    Table
});

syntax_enum!(crate::ast::FunctionVolatility {
    Immutable,
    Stable,
    Volatile
});

syntax_struct!(crate::ast::RoutineColumnTypeReference {
    schema,
    relation,
    column
});

syntax_enum!(crate::ast::RoutineDefaultType {
    Concrete(value0),
    Polymorphic(value0)
});

syntax_enum!(crate::ast::SQLBodyForm { Return, Atomic });
