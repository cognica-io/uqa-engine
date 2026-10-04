//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The checks `transformColumnDefinition` makes of a column definition once `transformCreateStmt` or `transformAlterTableStmt` has found the relation: an array of SERIAL before the column's type is looked up, then the placement of constraint attributes, as `transformConstraintAttrs` checks it, and each clause against the clauses written before it.

use crate::ast::{ColumnClause, ColumnClauseKind, ColumnDeclaration};
use crate::SQLError;

/// The relation a column definition belongs to.
#[derive(Clone, Copy)]
pub struct ColumnDeclarationTarget<'a> {
    /// The relation's name as the statement writes it.
    pub table: &'a str,
    /// The relation is partitioned.
    pub partitioned: bool,
}

/// `array of serial is not implemented`, which `transformColumnDefinition` reports before it looks up the column's type.
pub fn check_serial_array(declaration: &ColumnDeclaration) -> Result<(), SQLError> {
    if declaration.serial_array {
        return Err(error("0A000", "array of serial is not implemented".into()));
    }
    Ok(())
}

/// The attribute placement and clause conflicts `transformColumnDefinition` reports for `column` after it looks up the column's type. A PRIMARY KEY or UNIQUE clause made DEFERRABLE or INITIALLY DEFERRED is reported as unsupported after every other check of the statement passes, so the result says whether the column writes one.
pub fn check_column_declaration(
    declaration: &ColumnDeclaration,
    column: &str,
    target: ColumnDeclarationTarget<'_>,
) -> Result<bool, SQLError> {
    let deferrable_key = check_constraint_attributes(&declaration.clauses)?;
    ClauseConflicts::new(declaration, column, target).check()?;
    Ok(deferrable_key)
}

/// `transformConstraintAttrs`: DEFERRABLE, NOT DEFERRABLE, INITIALLY DEFERRED and INITIALLY IMMEDIATE follow a PRIMARY KEY, UNIQUE or REFERENCES clause, ENFORCED and NOT ENFORCED a CHECK or REFERENCES clause, each at most once.
fn check_constraint_attributes(clauses: &[ColumnClause]) -> Result<bool, SQLError> {
    use ColumnClauseKind::{
        Check, Deferrable, Enforced, ForeignKey, InitiallyDeferred, InitiallyImmediate,
        NotDeferrable, NotEnforced, PrimaryKey, Unique,
    };
    let mut last = None;
    let mut deferrable = false;
    let mut initially_deferred = false;
    let mut saw_deferrability = false;
    let mut saw_initially = false;
    let mut saw_enforced = false;
    let mut deferrable_key = false;
    let syntax = |message: &str| Err(error("42601", message.into()));
    for clause in clauses {
        let supports_timing = matches!(last, Some(PrimaryKey | Unique | ForeignKey));
        match clause.kind {
            Deferrable | NotDeferrable => {
                let written = if clause.kind == Deferrable {
                    "DEFERRABLE"
                } else {
                    "NOT DEFERRABLE"
                };
                if !supports_timing {
                    return syntax(&format!("misplaced {written} clause"));
                }
                if saw_deferrability {
                    return syntax("multiple DEFERRABLE/NOT DEFERRABLE clauses not allowed");
                }
                saw_deferrability = true;
                deferrable = clause.kind == Deferrable;
                if !deferrable && saw_initially && initially_deferred {
                    return syntax("constraint declared INITIALLY DEFERRED must be DEFERRABLE");
                }
            }
            InitiallyDeferred | InitiallyImmediate => {
                let written = if clause.kind == InitiallyDeferred {
                    "INITIALLY DEFERRED"
                } else {
                    "INITIALLY IMMEDIATE"
                };
                if !supports_timing {
                    return syntax(&format!("misplaced {written} clause"));
                }
                if saw_initially {
                    return syntax("multiple INITIALLY IMMEDIATE/DEFERRED clauses not allowed");
                }
                saw_initially = true;
                initially_deferred = clause.kind == InitiallyDeferred;
                if initially_deferred {
                    // INITIALLY DEFERRED alone makes the constraint DEFERRABLE.
                    if !saw_deferrability {
                        deferrable = true;
                    } else if !deferrable {
                        return syntax("constraint declared INITIALLY DEFERRED must be DEFERRABLE");
                    }
                }
            }
            Enforced | NotEnforced => {
                let written = if clause.kind == Enforced {
                    "ENFORCED"
                } else {
                    "NOT ENFORCED"
                };
                if !matches!(last, Some(Check | ForeignKey)) {
                    return syntax(&format!("misplaced {written} clause"));
                }
                if saw_enforced {
                    return syntax("multiple ENFORCED/NOT ENFORCED clauses not allowed");
                }
                saw_enforced = true;
            }
            kind => {
                last = Some(kind);
                deferrable = false;
                initially_deferred = false;
                saw_deferrability = false;
                saw_initially = false;
                saw_enforced = false;
            }
        }
        deferrable_key |= deferrable && matches!(last, Some(PrimaryKey | Unique));
    }
    Ok(deferrable_key)
}

/// The nextval default that SERIAL adds after the written clauses.
static SERIAL_DEFAULT: ColumnClause = ColumnClause {
    kind: ColumnClauseKind::Default,
    name: None,
    no_inherit: false,
};

/// What a column's clauses have said about its nullability so far.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Nullability {
    Unspecified,
    Null,
    NotNull,
}

/// The clauses that `transformColumnDefinition` allows once each and not together.
#[derive(Default)]
struct SeenClauses {
    default: bool,
    identity: bool,
    generated: bool,
}

/// The state `transformColumnDefinition` keeps while it reads a column's clauses.
struct ClauseConflicts<'a> {
    declaration: &'a ColumnDeclaration,
    column: &'a str,
    target: ColumnDeclarationTarget<'a>,
    need_not_null: bool,
    disallow_no_inherit: bool,
    nullability: Nullability,
    not_null: Option<&'a ColumnClause>,
    seen: SeenClauses,
}

impl<'a> ClauseConflicts<'a> {
    fn new(
        declaration: &'a ColumnDeclaration,
        column: &'a str,
        target: ColumnDeclarationTarget<'a>,
    ) -> Self {
        // A SERIAL column and a column with an identity or a primary key need a not-null constraint that NO INHERIT cannot describe.
        let disallow_no_inherit = declaration.serial
            || declaration.clauses.iter().any(|clause| {
                matches!(
                    clause.kind,
                    ColumnClauseKind::Identity | ColumnClauseKind::PrimaryKey
                )
            });
        Self {
            declaration,
            column,
            target,
            need_not_null: declaration.serial,
            disallow_no_inherit,
            nullability: Nullability::Unspecified,
            not_null: None,
            seen: SeenClauses::default(),
        }
    }

    fn check(mut self) -> Result<(), SQLError> {
        let serial = self.declaration.serial.then_some(&SERIAL_DEFAULT);
        let declaration = self.declaration;
        for clause in declaration.clauses.iter().chain(serial) {
            self.read(clause)?;
            self.check_combinations()?;
        }
        Ok(())
    }

    fn read(&mut self, clause: &'a ColumnClause) -> Result<(), SQLError> {
        match clause.kind {
            ColumnClauseKind::Null => {
                if self.nullability == Nullability::NotNull || self.need_not_null {
                    return Err(self.conflicting_nullability());
                }
                self.nullability = Nullability::Null;
            }
            ColumnClauseKind::NotNull => self.read_not_null(clause)?,
            ColumnClauseKind::Default => {
                if self.seen.default {
                    return Err(self.column_error("multiple default values specified"));
                }
                self.seen.default = true;
            }
            ColumnClauseKind::Identity => {
                if self.seen.identity {
                    return Err(self.column_error("multiple identity specifications"));
                }
                self.seen.identity = true;
                match self.nullability {
                    Nullability::Unspecified => self.need_not_null = true,
                    Nullability::Null => return Err(self.conflicting_nullability()),
                    Nullability::NotNull => {}
                }
            }
            ColumnClauseKind::Generated => {
                if self.seen.generated {
                    return Err(self.column_error("multiple generation clauses specified"));
                }
                self.seen.generated = true;
            }
            ColumnClauseKind::PrimaryKey => {
                if self.nullability == Nullability::Null {
                    return Err(self.conflicting_nullability());
                }
                self.need_not_null = true;
            }
            _ => {}
        }
        Ok(())
    }

    fn read_not_null(&mut self, clause: &'a ColumnClause) -> Result<(), SQLError> {
        if self.target.partitioned && clause.no_inherit {
            return Err(error(
                "0A000",
                "not-null constraints on partitioned tables cannot be NO INHERIT".into(),
            ));
        }
        if self.nullability == Nullability::Null {
            return Err(self.conflicting_nullability());
        }
        if self.disallow_no_inherit && clause.no_inherit {
            return Err(self.conflicting_no_inherit());
        }
        if self.nullability != Nullability::NotNull {
            self.nullability = Nullability::NotNull;
            self.need_not_null = false;
            self.not_null = Some(clause);
        } else if let Some(previous) = self.not_null {
            if let (Some(previous_name), Some(name)) = (&previous.name, &clause.name) {
                if previous_name != name {
                    return Err(error(
                        "XX000",
                        format!(
                            "conflicting not-null constraint names \"{previous_name}\" and \"{name}\""
                        ),
                    ));
                }
            }
            if previous.no_inherit != clause.no_inherit {
                return Err(self.conflicting_no_inherit());
            }
        }
        Ok(())
    }

    fn check_combinations(&self) -> Result<(), SQLError> {
        if self.seen.default && self.seen.identity {
            return Err(self.column_error("both default and identity specified"));
        }
        if self.seen.default && self.seen.generated {
            return Err(self.column_error("both default and generation expression specified"));
        }
        if self.seen.identity && self.seen.generated {
            return Err(self.column_error("both identity and generation expression specified"));
        }
        Ok(())
    }

    fn column_error(&self, what: &str) -> SQLError {
        error(
            "42601",
            format!(
                "{what} for column \"{}\" of table \"{}\"",
                self.column, self.target.table
            ),
        )
    }

    fn conflicting_nullability(&self) -> SQLError {
        error(
            "42601",
            format!(
                "conflicting NULL/NOT NULL declarations for column \"{}\" of table \"{}\"",
                self.column, self.target.table
            ),
        )
    }

    fn conflicting_no_inherit(&self) -> SQLError {
        error(
            "42601",
            format!(
                "conflicting NO INHERIT declarations for not-null constraints on column \"{}\"",
                self.column
            ),
        )
    }
}

fn error(sqlstate: &str, message: String) -> SQLError {
    SQLError::Routine {
        sqlstate: sqlstate.into(),
        message,
    }
}
