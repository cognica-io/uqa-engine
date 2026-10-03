//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Enter sequence statements with fresh catalog inputs and emit notices after their transactions.
use super::{
    creation::{create_sequence, SequenceCreationContext},
    dispatch::{alter_sequence, SequenceAlterContext},
};
use crate::catalog::sequence::SequenceState;
use uqa_sql::{
    ast::{AlterSequence, CreateSequence},
    schema::sequences::definition::SequenceDefinition,
    SQLError, SQLResult,
};

pub type SequenceCreationWrite<'a> =
    Box<dyn FnOnce(&SequenceCreationContext<'_>) -> Result<bool, SQLError> + 'a>;

pub trait SequenceCreationTransactions {
    fn with_sequence_creation(&self, write: SequenceCreationWrite<'_>) -> Result<bool, SQLError>;
}

pub fn run_create_sequence(
    transactions: &dyn SequenceCreationTransactions,
    notices: &crate::query::NoticeQueue,
    statement: &CreateSequence,
) -> Result<SQLResult, SQLError> {
    if !transactions.with_sequence_creation(Box::new(|context| {
        create_sequence(
            context,
            &statement.name,
            created_sequence_state(statement),
            statement.if_not_exists,
            statement.persistence,
            &statement.ownership,
        )
    }))? {
        notices.push(
            uqa_sql::SQLNotice::notice(format!(
                "relation \"{}\" already exists, skipping",
                uqa_core::RelationIdentity::parse_reference(&statement.name)
                    .map_or_else(|_| statement.name.clone(), |(_, name)| name)
            ))
            .with_sqlstate("42P07"),
        );
    }
    Ok(SQLResult::empty())
}

/// The state of the sequence `statement` creates, whose first `nextval` returns its start, or the value its `RESTART` gives.
fn created_sequence_state(statement: &CreateSequence) -> SequenceState {
    let mut state = SequenceState::from_definition(SequenceDefinition::from_create(statement));
    if let uqa_sql::ast::SequenceRestart::With(value) = statement.restart {
        state.current = value;
    }
    state
}

pub type SequenceAlterWrite<'a> =
    Box<dyn FnOnce(&SequenceAlterContext<'_>) -> Result<bool, SQLError> + 'a>;

pub trait SequenceAlterTransactions {
    fn with_sequence_write(&self, write: SequenceAlterWrite<'_>) -> Result<bool, SQLError>;
}

pub fn run_alter_sequence(
    transactions: &dyn SequenceAlterTransactions,
    notices: &crate::query::NoticeQueue,
    statement: &AlterSequence,
) -> Result<SQLResult, SQLError> {
    if !transactions.with_sequence_write(Box::new(|context| alter_sequence(context, statement)))? {
        notices.push(uqa_sql::SQLNotice::notice(
            uqa_sql::catalog::resolution::missing_relation_notice(&statement.name)?,
        ));
    }
    Ok(SQLResult::empty())
}
