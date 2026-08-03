//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::fmt::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use criterion::SamplingMode;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Fixture {
    pub manifest: Manifest,
}

#[derive(Deserialize)]
pub struct Manifest {
    pub rows: u64,
    pub seed: u64,
    pub work_mem: String,
    pub schema_sql: String,
    pub queries: Queries,
    pub criterion: CriterionConfig,
}

#[derive(Deserialize)]
pub struct Queries {
    pub q1: String,
    pub q6: String,
    pub scan: String,
}

#[derive(Deserialize)]
pub struct CriterionConfig {
    pub sample_size: usize,
    pub warm_up_ms: u64,
    pub measurement_ms: u64,
    pub sampling_mode: CriterionSamplingMode,
    pub point_estimator: PointEstimator,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CriterionSamplingMode {
    Linear,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PointEstimator {
    Slope,
}

impl Fixture {
    pub fn load() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../benchmarks/analytical/manifest.json");
        let manifest = serde_json::from_slice(
            &std::fs::read(&path)
                .unwrap_or_else(|error| panic!("read {}: {error}", path.display())),
        )
        .unwrap_or_else(|error| panic!("parse {}: {error}", path.display()));
        Self { manifest }
    }

    pub fn warm_up(&self) -> Duration {
        Duration::from_millis(self.manifest.criterion.warm_up_ms)
    }

    pub fn measurement(&self) -> Duration {
        Duration::from_millis(self.manifest.criterion.measurement_ms)
    }

    pub fn sampling_mode(&self) -> SamplingMode {
        match (
            self.manifest.criterion.sampling_mode,
            self.manifest.criterion.point_estimator,
        ) {
            (CriterionSamplingMode::Linear, PointEstimator::Slope) => SamplingMode::Linear,
        }
    }

    pub fn expected_scan_rows(&self) -> usize {
        (0..self.manifest.rows)
            .filter(|id| 1 + ((*id ^ self.manifest.seed) % 50) < 40)
            .count()
    }

    pub fn insert_sql(&self) -> String {
        let mut sql = String::with_capacity(self.manifest.rows as usize * 55);
        sql.push_str(
            "INSERT INTO lineitem \
             (id, return_flag, line_status, quantity, extended_price, discount, ship_day) VALUES ",
        );
        let flags = ["A", "N", "R"];
        let statuses = ["F", "O"];
        for id in 0..self.manifest.rows {
            let sample = id ^ self.manifest.seed;
            if id != 0 {
                sql.push_str(", ");
            }
            write!(
                sql,
                "({id}, '{}', '{}', {}, {}, {}, {})",
                flags[sample as usize % flags.len()],
                statuses[(sample as usize / flags.len()) % statuses.len()],
                1 + sample % 50,
                10_000 + sample % 90_000,
                sample % 11,
                sample % 2_500,
            )
            .expect("write fixture SQL");
        }
        sql
    }
}
