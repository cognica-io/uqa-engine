//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Common-token projection for independent Japanese stream oracles.

use crate::AnalyzedText;
use serde_json::{json, Value};

pub(in crate::kuromoji) fn common_raw(output: &AnalyzedText) -> Value {
    let units = |text: Option<&str>| text.map(|text| text.encode_utf16().collect::<Vec<_>>());
    let tokens: Vec<_> = output.tokens().iter().map(|token| {
        let fields = token.japanese_morphology().map_or([None;6], |value| value.fields().map(|field| field.map(String::as_str)));
        let offsets = token.offsets().unwrap();
        json!({"term_utf16":token.term().utf16(),"start_utf16":offsets.utf16.start,"end_utf16":offsets.utf16.end,
            "position_increment":token.position_increment(),"position_length":token.position_length(),"keyword":token.is_keyword(),
            "part_of_speech_utf16":units(fields[0]),"base_form_utf16":units(fields[1]),"reading_utf16":units(fields[2]),"pronunciation_utf16":units(fields[3]),"inflection_type_utf16":units(fields[4]),"inflection_form_utf16":units(fields[5])})
    }).collect();
    let mut value = json!({"tokens":tokens,"final_offset_utf16":output.final_offsets().utf16.end,"final_position_increment":output.final_position_increment()});
    value.sort_all_objects();
    value
}
