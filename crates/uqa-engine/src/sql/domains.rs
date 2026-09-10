//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Domain declaration binding and conversion-time constraint evaluation.

use uqa_core::Value;
use uqa_execution::RowSchema;
use uqa_sql::ast::{CreateDomain, Expr};
use uqa_sql::{ResultRow, SQLError};

use crate::domains::StoredDomain;
use crate::{Engine, RelationIdentity};

pub(crate) use uqa_sql::type_resolution::resolve_declared_column_type;

pub(super) fn create_domain(engine: &Engine, mut definition: CreateDomain) -> Result<(), SQLError> {
    engine.prepare_explicit_transaction_writer()?;
    definition.name = engine.try_relation_name_for_sql_create(&definition.name)?;
    let identity =
        RelationIdentity::from_legacy_name(&definition.name).map_err(SQLError::Internal)?;
    if super::resolve_catalog_column_type(engine, &definition.name).is_some()
        || engine
            .try_table(&definition.name)
            .map_err(|error| SQLError::Internal(error.to_string()))?
            .is_some()
    {
        return Err(domain_error(
            "42710",
            format!("type \"{}\" already exists", identity.name),
        ));
    }
    let scope = crate::capabilities::query_scope::new_for_catalog_binding(engine);
    let binding = uqa_execution::query::binding::binding_context(&scope)?;
    uqa_sql::schema::domains::prepare_domain_definition(
        &uqa_sql::schema::SchemaBindingContext {
            catalog: engine,
            binding: &binding,
        },
        engine,
        &mut definition,
    )?;
    let object_id = crate::new_nonzero_catalog_identity("domain", "object identity")
        .map_err(|error| SQLError::Internal(error.to_string()))?;
    let oid = super::catalog::domain_object_oid(&object_id);
    engine.publish_domain(StoredDomain {
        object_id,
        oid,
        identity,
        owner: engine.current_user_name(),
        definition,
    })
}

pub(crate) use uqa_sql::assignment::domain::cast_domain_value;
use uqa_sql::assignment::domain::domain_error;

impl uqa_sql::assignment::AssignmentContext for Engine {
    fn evaluate_domain_check(
        &self,
        expression: &Expr,
        row: &ResultRow,
        schema: &RowSchema,
    ) -> Result<Value, SQLError> {
        super::scalar::eval_lowered_expression_with_schema(self, expression, row, schema, &[])
    }
}
