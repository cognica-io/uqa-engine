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
    notices: &parking_lot::Mutex<Vec<(String, String)>>,
    statement: &CreateSequence,
) -> Result<SQLResult, SQLError> {
    if !transactions.with_sequence_creation(Box::new(|context| {
        create_sequence(
            context,
            &statement.name,
            SequenceState::from_definition(SequenceDefinition::from_create(statement)),
            statement.if_not_exists,
            statement.persistence,
            &statement.ownership,
        )
    }))? {
        notices.lock().push((
            "NOTICE".into(),
            format!("relation \"{}\" already exists, skipping", statement.name),
        ));
    }
    Ok(SQLResult::empty())
}

pub type SequenceAlterWrite<'a> =
    Box<dyn FnOnce(&SequenceAlterContext<'_>) -> Result<bool, SQLError> + 'a>;

pub trait SequenceAlterTransactions {
    fn with_sequence_write(&self, write: SequenceAlterWrite<'_>) -> Result<bool, SQLError>;
}

pub fn run_alter_sequence(
    transactions: &dyn SequenceAlterTransactions,
    notices: &parking_lot::Mutex<Vec<(String, String)>>,
    statement: &AlterSequence,
) -> Result<SQLResult, SQLError> {
    if !transactions.with_sequence_write(Box::new(|context| alter_sequence(context, statement)))? {
        notices.lock().push((
            "NOTICE".into(),
            format!("relation \"{}\" does not exist, skipping", statement.name),
        ));
    }
    Ok(SQLResult::empty())
}
