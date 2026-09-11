//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Sequence cache entries and nontransactional session observations.
use uqa_core::RelationIdentity;
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionSequenceValue {
    pub object_id: [u8; 16],
    pub value: i64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SessionSequenceCache {
    pub object_id: [u8; 16],
    pub definition_generation: [u8; 16],
    pub next_value: i64,
    pub remaining: i64,
    pub autonomous: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub struct SessionLastSequenceReference {
    pub relation: RelationIdentity,
    pub object_id: [u8; 16],
}

#[derive(Clone, Copy)]
pub struct NontransactionalSequenceValue {
    pub object_id: [u8; 16],
    pub current: i64,
    pub called: bool,
    pub log_count: i64,
    pub autonomous: bool,
}
