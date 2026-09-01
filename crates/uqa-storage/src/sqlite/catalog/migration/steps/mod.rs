//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered catalog-version steps.

use super::super::Result;

mod v01;
mod v02;
mod v03;
mod v04;
mod v05;
mod v06;
mod v07;
mod v08;
mod v09;
mod v10;
mod v11;
mod v12;
mod v13;
mod v14;
mod v15;
mod v16;
pub(super) mod v17;
mod v18;
mod v19;
mod v20;
mod v21;
pub(super) mod v22;
mod v23;
mod v24;
mod v25;
mod v26;
mod v27;

type MigrationFn = for<'a> fn(&rusqlite::Transaction<'a>) -> Result<()>;

#[derive(Clone, Copy)]
pub(super) enum MigrationAction {
    Sql(&'static str),
    Custom(MigrationFn),
}

#[derive(Clone, Copy)]
pub(super) struct MigrationStep {
    pub(super) version: u32,
    pub(super) action: MigrationAction,
}

impl MigrationStep {
    const fn sql(version: u32, sql: &'static str) -> Self {
        Self {
            version,
            action: MigrationAction::Sql(sql),
        }
    }

    const fn custom(version: u32, migrate: MigrationFn) -> Self {
        Self {
            version,
            action: MigrationAction::Custom(migrate),
        }
    }
}

/// Migrations applied in order. Each version is run in one transaction and the metadata schema-version row is bumped only after its step succeeds.
pub(super) const MIGRATIONS: [MigrationStep; 27] = [
    MigrationStep::sql(1, v01::SQL),
    MigrationStep::sql(2, v02::SQL),
    MigrationStep::sql(3, v03::SQL),
    MigrationStep::sql(4, v04::SQL),
    MigrationStep::sql(5, v05::SQL),
    MigrationStep::sql(6, v06::SQL),
    MigrationStep::sql(7, v07::SQL),
    MigrationStep::sql(8, v08::SQL),
    MigrationStep::sql(9, v09::SQL),
    MigrationStep::sql(10, v10::SQL),
    MigrationStep::sql(11, v11::SQL),
    MigrationStep::sql(12, v12::SQL),
    MigrationStep::sql(13, v13::SQL),
    MigrationStep::sql(14, v14::SQL),
    MigrationStep::sql(15, v15::SQL),
    MigrationStep::custom(16, v16::migrate),
    MigrationStep::custom(17, v17::migrate),
    MigrationStep::custom(18, v18::migrate),
    MigrationStep::sql(19, v19::SQL),
    MigrationStep::custom(20, v20::migrate),
    MigrationStep::sql(21, v21::SQL),
    MigrationStep::custom(22, v22::migrate),
    MigrationStep::custom(23, v23::migrate),
    MigrationStep::custom(24, v24::migrate),
    MigrationStep::custom(25, v25::migrate),
    MigrationStep::custom(26, v26::migrate),
    MigrationStep::custom(27, v27::migrate),
];
