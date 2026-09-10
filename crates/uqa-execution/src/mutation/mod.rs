//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Physical mutation identities, row images, command overlays, and prepared action transport.

pub mod candidate;
pub mod deferred;
pub mod merge;
pub mod overlay;
pub mod prepared;
pub mod row_images;

pub mod locking;

pub mod generated;

pub mod expressions;
pub mod rows;
pub mod views;

pub mod triggers;

pub mod constraints;
pub mod errors;

pub mod conflict;

pub mod returning;

pub mod rules;

pub mod events;

pub mod identity;
pub mod publication;
pub mod vectors;

pub mod staging;

pub mod assignment;

pub mod referential;

pub mod insert;

pub mod preparation;
pub mod update;

pub mod command_scope;
pub mod point_update;

pub mod statement;

pub mod delete;

pub mod dispatch;
