//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! The options a sequence declaration writes, as written.

use super::ColumnType;

/// The options a `CREATE SEQUENCE`, or the declaration of an identity column, writes, kept as written. `PostgreSQL` reads their values and derives the omitted ones when it creates the sequence, in an order of its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SequenceDeclaration {
    /// `AS type`.
    pub data_type: Option<ColumnType>,
    /// `INCREMENT [BY] value`.
    pub increment: Option<SequenceOptionValue>,
    /// `CYCLE` or `NO CYCLE`.
    pub cycle: Option<bool>,
    /// `MAXVALUE value`, or `NO MAXVALUE` as an absent value.
    pub max_value: Option<SequenceOptionValue>,
    /// `MINVALUE value`, or `NO MINVALUE` as an absent value.
    pub min_value: Option<SequenceOptionValue>,
    /// `START [WITH] value`.
    pub start: Option<SequenceOptionValue>,
    /// `RESTART [WITH value]`, a bare `RESTART` as an absent value: the value the first `nextval` returns, instead of the start.
    pub restart: Option<SequenceOptionValue>,
    /// `CACHE value`.
    pub cache: Option<SequenceOptionValue>,
    /// `OWNED BY`, as the written name: `NONE`, or a relation and one of its columns.
    pub owned_by: Option<Vec<String>>,
}

/// A sequence option's value as the parser keeps it, before it is read as a 64-bit integer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SequenceOptionValue {
    /// No value: `NO MINVALUE`, `NO MAXVALUE` or a bare `RESTART`.
    Absent,
    /// An integer the parser read.
    Integer(i64),
    /// A number the parser kept as written: one outside the 32-bit range, or with a fraction or an exponent.
    Text(String),
}
