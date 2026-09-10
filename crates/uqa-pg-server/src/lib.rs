//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! `PostgreSQL` connections with independent UQA Engine sessions.

mod connection;
mod results;
mod server;
mod startup;
mod transport;

pub use server::{Server, ServerConfig};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Protocol(#[from] uqa_pg_wire::PgWireError),
    #[error(transparent)]
    Sql(#[from] uqa_sql::SQLError),
}
