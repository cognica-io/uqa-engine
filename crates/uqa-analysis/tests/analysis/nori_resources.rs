//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! One decoded reference model shared by the Nori integration tests.

use std::sync::{Arc, OnceLock};
use uqa_analysis::nori::{DictionaryLimits, NoriDictionary};

pub(super) fn model() -> &'static Arc<NoriDictionary> {
    static MODEL: OnceLock<Arc<NoriDictionary>> = OnceLock::new();
    MODEL.get_or_init(|| {
        NoriDictionary::from_bytes(uqa_nori_data::BUNDLE, DictionaryLimits::default()).unwrap()
    })
}
