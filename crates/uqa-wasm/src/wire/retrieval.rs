//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use serde::ser::{Error as _, SerializeSeq};
use serde::{Serialize, Serializer};
use uqa_core::ScoredEntry;
use uqa_scoring::CalibrationReport;

use super::{finite_number, safe_u64, success};

pub(crate) struct SearchHits<'a>(pub(crate) &'a [ScoredEntry]);

impl Serialize for SearchHits<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Hit {
            doc_id: u64,
            score: f64,
        }
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for entry in self.0 {
            seq.serialize_element(&Hit {
                doc_id: safe_u64(entry.doc_id, "search document id").map_err(S::Error::custom)?,
                score: finite_number(entry.score, "search score").map_err(S::Error::custom)?,
            })?;
        }
        seq.end()
    }
}

pub(crate) fn calibration(report: &CalibrationReport) -> Result<String, String> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Bin {
        avg_actual: f64,
        avg_predicted: f64,
        count: u64,
    }
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Report {
        bins: Vec<Bin>,
        brier: f64,
        ece: f64,
        log_loss: f64,
    }
    let mut bins = Vec::with_capacity(report.bins.len());
    for bin in &report.bins {
        let avg_predicted = finite_number(bin.avg_predicted, "calibration avgPredicted")?;
        let avg_actual = finite_number(bin.avg_actual, "calibration avgActual")?;
        let count =
            u64::try_from(bin.count).map_err(|_| "calibration bin count exceeds the u64 bridge")?;
        bins.push(Bin {
            avg_actual,
            avg_predicted,
            count: safe_u64(count, "calibration bin count")?,
        });
    }
    let ece = finite_number(report.ece, "calibration ece")?;
    success(Report {
        bins,
        brier: finite_number(report.brier, "calibration brier")?,
        ece,
        log_loss: finite_number(report.log_loss, "calibration logLoss")?,
    })
}
