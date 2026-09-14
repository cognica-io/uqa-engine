//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean limit and error adapters for the shared rolling lattice.

use super::NoriLimits;
use crate::morphology::lattice::LatticeConfig;
use crate::nori::error::{check_limit, invalid};
use crate::{AnalysisError, AnalysisResult};

pub(super) use crate::morphology::lattice::{Node, WordId};

impl LatticeConfig for NoriLimits {
    fn check_positions(self, required: usize) -> AnalysisResult<()> {
        check_limit(
            "Nori lattice positions",
            required,
            self.max_lattice_positions,
        )?;
        Ok(())
    }

    fn check_candidates(self, required: usize) -> AnalysisResult<()> {
        check_limit(
            "Nori lattice candidates",
            required,
            self.max_lattice_candidates,
        )?;
        Ok(())
    }

    fn invalid(self, reason: &'static str) -> AnalysisError {
        invalid("Nori lattice", reason).into()
    }
}
