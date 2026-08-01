//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Persisted model and scoring-parameter state.

use super::{params, Catalog, OptionalExtension, Result};

impl Catalog {
    pub fn save_model(&self, name: &str, json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _models (name, body) VALUES (?1, ?2)",
                params![name, json],
            )?;
            Ok(())
        })
    }

    pub fn load_models(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, body FROM _models ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    pub fn load_model(&self, name: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            Ok(c.query_row(
                "SELECT body FROM _models WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    pub fn drop_model(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _models WHERE name = ?1", params![name])?;
            Ok(())
        })
    }

    /// Persist Bayesian calibration parameters for a named signal.
    /// Matches UQA behavior for `Catalog.save_scoring_params`.
    pub fn save_scoring_params(&self, name: &str, params_json: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute(
                "INSERT OR REPLACE INTO _scoring_params (name, params) VALUES (?1, ?2)",
                params![name, params_json],
            )?;
            Ok(())
        })
    }

    /// Load persisted scoring parameters for a single signal.
    pub fn load_scoring_params(&self, name: &str) -> Result<Option<String>> {
        self.conn.with(|c| {
            Ok(c.query_row(
                "SELECT params FROM _scoring_params WHERE name = ?1",
                params![name],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
    }

    /// Load every persisted `(name, params_json)` pair sorted by name.
    pub fn load_all_scoring_params(&self) -> Result<Vec<(String, String)>> {
        self.conn.with(|c| {
            let mut stmt = c.prepare("SELECT name, params FROM _scoring_params ORDER BY name")?;
            let rows =
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        })
    }

    /// Delete persisted scoring parameters for a single signal.
    pub fn drop_scoring_params(&self, name: &str) -> Result<()> {
        self.conn.with(|c| {
            c.execute("DELETE FROM _scoring_params WHERE name = ?1", params![name])?;
            Ok(())
        })
    }
}
