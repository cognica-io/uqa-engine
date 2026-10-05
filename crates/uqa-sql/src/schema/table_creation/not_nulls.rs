//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The NOT NULL constraints of a new table, which `AddRelationNotNullConstraints` creates once the table's CHECK constraints exist. The declared constraints come first, in the order `transformCreateStmt` collected them, one constraint per column: a later declaration of a column must agree with the earlier one on NO INHERIT and may name the constraint the earlier one left unnamed. A declaration names a column of the relation, a NO INHERIT declaration cannot stand on a column whose parents give a constraint, a given name is new among the relation's constraints, and a chosen name follows `ChooseConstraintName`. The constraints only parents give follow, each keeping its first parent's name unless the relation holds it already.

use crate::ast::{ColumnDef, CreateTable, NotNullDeclaration};
use crate::schema::columns::POSTGRES_SYSTEM_COLUMNS;
use crate::schema::constraint_metadata::{
    assign_constraint_name, identity::materialize_not_null_identity, CatalogIdentityAllocator,
    CatalogOidClass, ConstraintMetadataError,
};
use crate::SQLError;
use std::collections::BTreeSet;

use super::declaration::{CreateTableAnalysisContext, InheritedDefinitions};

/// A NOT NULL constraint the parents give a column: the first parent's constraint name and how many parents give it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InheritedNotNull {
    pub column: String,
    pub name: Option<String>,
    pub parents: usize,
}

impl InheritedNotNull {
    /// Record a parent's constraint on `column` after the ones earlier parents gave: the first parent's name is kept, as `AddRelationNotNullConstraints` keeps the first name it comes across.
    pub fn record(inherited: &mut Vec<Self>, column: &str, name: Option<&str>) {
        if let Some(existing) = inherited
            .iter_mut()
            .find(|existing| existing.column == column)
        {
            if existing.name.is_none() {
                existing.name = name.map(str::to_owned);
            }
            existing.parents += 1;
            return;
        }
        inherited.push(Self {
            column: column.to_owned(),
            name: name.map(str::to_owned),
            parents: 1,
        });
    }
}

/// Create the NOT NULL constraints of `table` as `AddRelationNotNullConstraints` does, after its CHECK constraints: the declared constraints take their names and OIDs in declaration order, then the constraints only parents give. `relation_oid` is the relation's OID, which the `pg_constraint_conrelid_contypid_conname_index` violation of a given name the relation holds reports.
pub fn define_not_null_constraints(
    context: &CreateTableAnalysisContext<'_>,
    table: &mut CreateTable,
    inherited: &InheritedDefinitions,
    relation_oid: u32,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    let relation = uqa_core::RelationIdentity::from_legacy_name(&table.name)
        .map_err(SQLError::Internal)?
        .name;
    let declared = merge_declarations(
        &table.columns,
        &relation,
        std::mem::take(&mut table.not_null_declarations),
    )?;
    let mut names = Names {
        relation_oid,
        held: held_constraint_names(table, inherited),
        used: context
            .index_names
            .automatic_constraint_names(&table.name)?,
        chosen: BTreeSet::new(),
        relation,
    };
    names.used.extend(names.held.iter().cloned());
    let mut constrained = BTreeSet::new();
    for (index, declaration) in declared {
        let column = &mut table.columns[index];
        let parents_give = inherited
            .not_nulls
            .iter()
            .any(|constraint| constraint.column == column.name);
        if parents_give && declaration.no_inherit {
            return Err(SQLError::Diagnostic {
                sqlstate: "42804".into(),
                message: format!(
                    "cannot define not-null constraint with NO INHERIT on column \"{}\"",
                    column.name
                ),
                detail: Some("The column has an inherited not-null constraint.".into()),
                hint: None,
            });
        }
        let name = match declaration.name {
            Some(name) => names.given(name, allocate)?,
            None => names.choose(&column.name)?,
        };
        constrained.insert(column.name.clone());
        store(
            column,
            name,
            Locality {
                is_local: true,
                no_inherit: declaration.no_inherit,
                explicit: declaration.explicit,
            },
            allocate,
        )?;
    }
    for constraint in &inherited.not_nulls {
        if constrained.contains(&constraint.column) {
            continue;
        }
        let Some(column) = table
            .columns
            .iter_mut()
            .find(|column| column.name == constraint.column)
        else {
            return Err(SQLError::Internal(format!(
                "inherited NOT NULL constraint of column `{}` has no column",
                constraint.column
            )));
        };
        let name = names.inherited(constraint.name.as_deref(), &column.name)?;
        constrained.insert(column.name.clone());
        store(
            column,
            name,
            Locality {
                is_local: false,
                no_inherit: false,
                explicit: false,
            },
            allocate,
        )?;
    }
    if let Some(column) = table
        .columns
        .iter()
        .find(|column| column.not_null && !constrained.contains(&column.name))
    {
        return Err(SQLError::Internal(format!(
            "NOT NULL constraint of column `{}` was not declared",
            column.name
        )));
    }
    Ok(())
}

/// Resolve each declaration's column and keep one declaration per column, as the first pass of `AddRelationNotNullConstraints` does.
fn merge_declarations(
    columns: &[ColumnDef],
    relation: &str,
    declarations: Vec<NotNullDeclaration>,
) -> Result<Vec<(usize, NotNullDeclaration)>, SQLError> {
    let mut kept: Vec<(usize, NotNullDeclaration)> = Vec::new();
    for declaration in declarations {
        let Some(index) = columns
            .iter()
            .position(|column| column.name == declaration.column)
        else {
            if POSTGRES_SYSTEM_COLUMNS.contains(&declaration.column.as_str()) {
                return Err(error(
                    "0A000",
                    format!(
                        "cannot add not-null constraint on system column \"{}\"",
                        declaration.column
                    ),
                ));
            }
            return Err(error(
                "42703",
                format!(
                    "column \"{}\" of relation \"{relation}\" does not exist",
                    declaration.column
                ),
            ));
        };
        if let Some((_, existing)) = kept.iter_mut().find(|(existing, _)| *existing == index) {
            if existing.no_inherit != declaration.no_inherit {
                return Err(error(
                    "42601",
                    format!(
                        "conflicting NO INHERIT declaration for not-null constraint on column \"{}\"",
                        declaration.column
                    ),
                ));
            }
            match (&existing.name, &declaration.name) {
                (Some(first), Some(second)) if first != second => {
                    return Err(error(
                        "42601",
                        format!(
                            "conflicting not-null constraint names \"{first}\" and \"{second}\""
                        ),
                    ));
                }
                (None, Some(name)) => existing.name = Some(name.clone()),
                _ => {}
            }
            existing.explicit |= declaration.explicit;
            continue;
        }
        kept.push((index, declaration));
    }
    Ok(kept)
}

/// The namespace and CHECK constraints already held when a foreign table's NOT NULL constraints are stored.
pub struct ForeignNotNullContext<'a> {
    pub relation: &'a uqa_core::RelationIdentity,
    pub relation_oid: u32,
    pub checks: &'a [crate::ast::TableCheck],
    pub names: &'a crate::schema::constraint_metadata::ConstraintNameScope,
}

/// Store foreign-table declarations with the same target, merge, name and identity rules as ordinary tables. Foreign tables have no inherited constraints or supported keys.
pub fn define_foreign_not_null_constraints(
    context: ForeignNotNullContext<'_>,
    columns: &mut [ColumnDef],
    declarations: Vec<NotNullDeclaration>,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    let declared = merge_declarations(columns, &context.relation.name, declarations)?;
    let held = columns
        .iter()
        .filter_map(|column| column.check_name.clone().filter(|_| column.check.is_some()))
        .chain(context.checks.iter().filter_map(|check| check.name.clone()))
        .chain(context.names.events.iter().cloned())
        .collect();
    let mut names = Names {
        relation: context.relation.name.clone(),
        relation_oid: context.relation_oid,
        held,
        used: context.names.schema.clone(),
        chosen: BTreeSet::new(),
    };
    names.used.extend(names.held.iter().cloned());
    for (index, declaration) in declared {
        let name = match declaration.name {
            Some(name) => names.given(name, allocate)?,
            None => names.choose(&declaration.column)?,
        };
        store(
            &mut columns[index],
            name,
            Locality {
                is_local: true,
                no_inherit: declaration.no_inherit,
                explicit: declaration.explicit,
            },
            allocate,
        )?;
    }
    Ok(())
}

/// The constraints the relation holds when its NOT NULL constraints are created: its CHECK constraints, inherited and declared, and the keys and foreign keys a partition clones.
fn held_constraint_names(
    table: &CreateTable,
    inherited: &InheritedDefinitions,
) -> BTreeSet<String> {
    let mut held = super::declaration::cloned_constraint_names(table, inherited);
    held.extend(table.checks.iter().filter_map(|check| check.name.clone()));
    held.extend(
        table
            .columns
            .iter()
            .filter(|column| column.check.is_some())
            .filter_map(|column| column.check_name.clone()),
    );
    held
}

/// The names the statement's NOT NULL constraints take, as `AddRelationNotNullConstraints` and `ChooseConstraintName` choose them: `held` are the constraints the relation holds already, `used` the names of every constraint in the schema and of the constraints created so far, and `chosen` the NOT NULL names of this statement.
struct Names {
    relation: String,
    relation_oid: u32,
    held: BTreeSet<String>,
    used: BTreeSet<String>,
    chosen: BTreeSet<String>,
}

impl Names {
    /// A name the statement gives: not one it gave another NOT NULL constraint, and not one a constraint of the relation holds, which `pg_constraint`'s unique index reports once `CreateConstraintEntry` has drawn the constraint's OID.
    fn given(
        &mut self,
        name: String,
        allocate: &mut CatalogIdentityAllocator<'_>,
    ) -> Result<String, SQLError> {
        if self.chosen.contains(&name) {
            return Err(crate::schema::check_inheritance::duplicate_check(
                &self.relation,
                &name,
            ));
        }
        if self.held.contains(&name) {
            let object_id = allocate
                .allocate_object_id("NOT NULL constraint")
                .map_err(ConstraintMetadataError::into_sql_error)?;
            allocate
                .allocate_catalog_oid(CatalogOidClass::Constraint, &object_id)
                .map_err(ConstraintMetadataError::into_sql_error)?;
            return Err(SQLError::Diagnostic {
                sqlstate: "23505".into(),
                message: "duplicate key value violates unique constraint \"pg_constraint_conrelid_contypid_conname_index\"".into(),
                detail: Some(format!(
                    "Key (conrelid, contypid, conname)=({}, 0, {name}) already exists.",
                    self.relation_oid
                )),
                hint: None,
            });
        }
        Ok(self.take(name))
    }

    /// An inherited constraint keeps its first parent's name unless the relation holds it or the statement chose it.
    fn inherited(&mut self, preferred: Option<&str>, column: &str) -> Result<String, SQLError> {
        match preferred {
            Some(name) if !self.chosen.contains(name) && !self.held.contains(name) => {
                Ok(self.take(name.to_owned()))
            }
            _ => self.choose(column),
        }
    }

    /// `ChooseConstraintName` with the `not_null` label: the first of `relation_column_not_null`, `relation_column_not_null1`, ... that no constraint uses.
    fn choose(&mut self, column: &str) -> Result<String, SQLError> {
        let mut target = None;
        assign_constraint_name(
            &mut target,
            (&self.relation, column, "not_null"),
            &mut self.used,
        )
        .map_err(ConstraintMetadataError::into_sql_error)?;
        let name = target
            .ok_or_else(|| SQLError::Internal("NOT NULL constraint name was not chosen".into()))?;
        self.chosen.insert(name.clone());
        Ok(name)
    }

    fn take(&mut self, name: String) -> String {
        self.used.insert(name.clone());
        self.chosen.insert(name.clone());
        name
    }
}

/// How a NOT NULL constraint stands on its column: declared by the statement or given only by parents, inheritable or not, and written by the statement or implied by a key, SERIAL or identity.
struct Locality {
    is_local: bool,
    no_inherit: bool,
    explicit: bool,
}

/// `StoreRelNotNull`: the constraint's name, locality and inheritance on its column, validated as CREATE TABLE validates every constraint it creates, and its catalog identity.
fn store(
    column: &mut ColumnDef,
    name: String,
    locality: Locality,
    allocate: &mut CatalogIdentityAllocator<'_>,
) -> Result<(), SQLError> {
    column.not_null = true;
    column.not_null_explicit = locality.explicit;
    column.not_null_name = Some(name);
    column.not_null_no_inherit = locality.no_inherit;
    column.not_null_validated = true;
    column.not_null_is_local = locality.is_local;
    column.not_null_identity = None;
    materialize_not_null_identity(column, allocate)
        .map_err(ConstraintMetadataError::into_sql_error)?;
    Ok(())
}

fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}

#[cfg(test)]
mod tests;
