//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Dependencies of user routines as `ProcedureCreate` records them: the schema, the language, the result and parameter types; then what a SQL-standard body uses, its parameters by type; then what the parameter defaults use once coerced to their parameter types.

use super::{ColumnScope, DependencyBuilder, References};
use crate::catalog::projection::regtypes::catalog_routine_type_oid;
use std::collections::BTreeSet;
use uqa_core::{RelationIdentity, Value};
use uqa_sql::ast::{CreateFunction, Expr, FunctionBinding, FunctionBody, Statement};
use uqa_sql::catalog::dependencies::{
    DependencyKind, ObjectAddress, LANGUAGE_CLASS, PROCEDURE_CLASS,
};
use uqa_sql::catalog::stored_ast::StoredAstVisitor;
use uqa_sql::SQLError;

impl DependencyBuilder<'_> {
    pub(super) fn record_routines(&mut self) -> Result<(), SQLError> {
        for function in self.catalog.all_sql_functions() {
            let definition = &function.def;
            let oid = super::catalog_oid(super::super::user_routine_catalog_oid(&function)?)?;
            let address = ObjectAddress::whole(PROCEDURE_CLASS, oid);
            let identity =
                RelationIdentity::from_legacy_name(&definition.name).map_err(SQLError::Internal)?;
            self.record_namespace(address, &identity.schema);
            let mut signature = References::default();
            if definition.language.eq_ignore_ascii_case("plpgsql") {
                signature.add(ObjectAddress::whole(
                    LANGUAGE_CLASS,
                    super::objects::PLPGSQL_LANGUAGE,
                ));
            }
            let result = super::super::pg_proc::routine_result_type_oid(self.catalog, definition);
            if let Ok(result) = u32::try_from(result) {
                signature.add_type(result);
            }
            for parameter in &definition.params {
                if let Ok(ty) =
                    u32::try_from(catalog_routine_type_oid(self.catalog, &parameter.type_name))
                {
                    signature.add_type(ty);
                }
            }
            self.recorder
                .record_references(address, signature, DependencyKind::Normal);
            if let FunctionBody::Statements(statements) = &definition.body {
                let mut body = References::default();
                for statement in statements {
                    self.collect_body_statement(definition, &identity.name, statement, &mut body)?;
                }
                self.recorder
                    .record_references(address, body, DependencyKind::Normal);
            }
            let mut defaults = References::default();
            for parameter in &definition.params {
                let Some(default) = &parameter.default else {
                    continue;
                };
                let target = crate::catalog::projection::resolve_catalog_column_type(
                    self.context,
                    &parameter.type_name,
                );
                if let Some(target) = target {
                    self.expressions().collect_assigned(
                        default,
                        &target,
                        (ColumnScope::None, &[]),
                        &mut defaults,
                    )?;
                } else {
                    self.expressions()
                        .collect(default, ColumnScope::None, &mut defaults)?;
                }
            }
            self.recorder
                .record_references(address, defaults, DependencyKind::Normal);
        }
        Ok(())
    }

    /// What one statement of a SQL-standard body references: the relations of its range tables and the columns it names, the types of its constants and coercions, its routines, and the parameters it uses by type.
    fn collect_body_statement(
        &self,
        definition: &CreateFunction,
        local_name: &str,
        statement: &Statement,
        references: &mut References,
    ) -> Result<(), SQLError> {
        let mut relations = Vec::new();
        let mut types = Vec::new();
        let mut routines: Vec<FunctionBinding> = Vec::new();
        let mut constants = Vec::new();
        let mut parameters = BTreeSet::new();
        for address in
            uqa_sql::binding::composite_dependencies::routine_statement_composite_dependencies(
                self.context.routines,
                definition,
                statement,
                &self.field_binding_context(),
            )?
        {
            references.add(address);
        }
        let mut expression = |node: &mut Expr| -> Result<(), SQLError> {
            match node {
                Expr::TypedLiteral {
                    value: Value::Int(oid),
                    ty,
                    ..
                } => constants.push((ty.clone(), *oid)),
                Expr::Param(position) => {
                    parameters.insert(*position);
                }
                _ => {}
            }
            Ok(())
        };
        let mut type_names = |name: &mut String| types.push(name.clone());
        let mut relation = |name: &mut String| -> Result<(), SQLError> {
            relations.push(name.clone());
            Ok(())
        };
        let mut routine = |_: &mut String,
                           binding: Option<&mut Option<FunctionBinding>>|
         -> Result<(), SQLError> {
            if let Some(Some(binding)) = binding {
                routines.push(binding.clone());
            }
            Ok(())
        };
        StoredAstVisitor {
            source: None,
            merge: None,
            expression: Some(&mut expression),
            projection: None,
            ty: Some(&mut type_names),
            relation: &mut relation,
            routine: &mut routine,
        }
        .bind_statement(&mut statement.clone())?;
        for name in &relations {
            if let Some(oid) = self.objects.relation_oid_by_name(name) {
                references.add_relation(oid);
            }
        }
        self.collect_body_columns(definition, local_name, statement, parameters, references)?;
        // A `MERGE` assignment coerces its value to a domain-typed target column; the coercion stays in the body after the column is dropped.
        uqa_sql::catalog::stored_ast::visit_stored_statement_merges(
            &mut statement.clone(),
            &mut |merge| {
                for binding in merge.target_column_bindings.values() {
                    for domain in &binding.domain_dependencies {
                        references.add_type(*domain);
                    }
                }
                Ok(())
            },
        )?;
        let expressions = self.expressions();
        for name in &types {
            if let Some(oid) = expressions.type_oid(name) {
                references.add_type(oid);
            }
        }
        for binding in &routines {
            if let Some(oid) = expressions.routine_oid(binding) {
                references.add_routine(oid);
            }
        }
        for (ty, oid) in constants {
            super::expressions::add_constant_reference(&ty, oid, references);
        }
        Ok(())
    }

    /// The columns a body statement names, and the types of the parameters it uses, including those its unresolved column names name.
    fn collect_body_columns(
        &self,
        definition: &CreateFunction,
        local_name: &str,
        statement: &Statement,
        mut parameters: BTreeSet<usize>,
        references: &mut References,
    ) -> Result<(), SQLError> {
        let sources = super::columns::StoredColumns::new(self);
        let columns = uqa_sql::binding::stored_columns::stored_statement_references(
            sources.binding_context(),
            statement,
        )?;
        for dependency in &columns.dependencies {
            let Some(oid) = self.objects.relation_oid(&dependency.relation) else {
                continue;
            };
            if let Some(number) = self
                .objects
                .relation(oid)
                .and_then(|relation| relation.column_number(&dependency.column))
            {
                references.add_column(oid, number);
            }
        }
        parameters.extend(parameter_references(
            definition,
            local_name,
            &columns.unresolved,
        ));
        for position in parameters {
            if let Some(parameter) = position
                .checked_sub(1)
                .and_then(|index| definition.params.get(index))
            {
                if let Ok(ty) =
                    u32::try_from(catalog_routine_type_oid(self.catalog, &parameter.type_name))
                {
                    references.add_type(ty);
                }
            }
        }
        Ok(())
    }
}

/// The positions of the parameters a body names: a name that no range table provides, unqualified or qualified by the routine's name, is one of the routine's parameters.
fn parameter_references<'a>(
    definition: &'a CreateFunction,
    local_name: &'a str,
    unresolved: &'a [(Option<String>, String)],
) -> impl Iterator<Item = usize> + 'a {
    unresolved.iter().filter_map(move |(qualifier, name)| {
        let own = qualifier
            .as_deref()
            .is_none_or(|qualifier| qualifier.eq_ignore_ascii_case(local_name));
        own.then(|| {
            definition
                .params
                .iter()
                .position(|parameter| !parameter.name.is_empty() && parameter.name == *name)
        })
        .flatten()
        .map(|index| index + 1)
    })
}
