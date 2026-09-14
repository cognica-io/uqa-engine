//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Korean profile and diagnostics for shared Java simple lowercase.

use super::Work;
use crate::nori::error::invalid;
use crate::nori::NoriDictionary;
use crate::AnalysisResult;

pub(in crate::nori) fn apply(
    input: &mut [u16],
    model: Option<&NoriDictionary>,
    work: &mut Work<'_>,
) -> AnalysisResult<()> {
    let model = model.ok_or_else(|| invalid("Nori lowercase", "missing Unicode profile"))?;
    crate::morphology::filter::lowercase::apply(input, &model.unicode, work, || {
        invalid("Nori lowercase", "simple mapping changes UTF-16 width").into()
    })
}
