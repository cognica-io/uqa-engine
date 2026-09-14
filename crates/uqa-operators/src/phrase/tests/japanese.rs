//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::AnalyzerPhase;

fn japanese(config: &str) -> Option<Analyzer> {
    match serde_json::from_str::<Analyzer>(config) {
        Ok(config) => Some(config),
        Err(error) => {
            assert!(error
                .to_string()
                .contains("unknown variant `kuromoji_tokenizer`"));
            None
        }
    }
}

#[test]
fn japanese_nbest_phrases_preserve_paths_stop_holes_and_independent_search_revisions() {
    let Some(analyzer) =
        japanese(r#"{"tokenizer":{"type":"kuromoji_tokenizer","n_best_cost":1000}}"#)
    else {
        return;
    };
    let mut index = fixture(
        analyzer,
        &[
            (1, "関西国際空港"),
            (2, "関西 国際 空港"),
            (3, "関西 空港 国際"),
            (4, "関西 国際 の 空港"),
        ],
    );
    let original = index.index_analyzer_revision("body").unwrap();
    let search_revision = uqa_analysis::get_analyzer("kuromoji")
        .unwrap()
        .compile()
        .unwrap();
    index
        .set_field_analyzer_revision("body", search_revision, AnalyzerPhase::Search)
        .unwrap();
    assert_eq!(
        search(&index, "関西国際空港")
            .iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>(),
        [1, 2]
    );
    assert_eq!(
        search(&index, "関西 国際 の 空港")
            .iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>(),
        [4]
    );
    assert!(search(&index, "の は です").is_empty());
    let full = index
        .get_occurrences(1, "body", &TokenTermKey::from_text("関西国際空港"))
        .unwrap();
    assert_eq!(full[0].position_length, 3);
    assert_eq!(
        index
            .index_analyzer_revision("body")
            .unwrap()
            .descriptor()
            .fingerprint(),
        original.descriptor().fingerprint()
    );
}

#[test]
fn japanese_completion_phrases_match_whole_alternatives_without_crossing_order() {
    let Ok(analyzer) = uqa_analysis::get_analyzer("kuromoji_completion") else {
        return;
    };
    let mut index = fixture(
        analyzer,
        &[(1, "東京 京都"), (2, "京都 東京"), (3, "東京 大阪 京都")],
    );
    index
        .set_field_analyzer_revision(
            "body",
            whitespace_analyzer().compile().unwrap(),
            AnalyzerPhase::Search,
        )
        .unwrap();
    // Pinned completion case analyzer-index-16 emits toukyou and kyouto.
    assert_eq!(
        search(&index, "toukyou kyouto")
            .iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>(),
        [1]
    );
    assert_eq!(
        search(&index, "kyouto toukyou")
            .iter()
            .map(|row| row.doc_id)
            .collect::<Vec<_>>(),
        [2]
    );
    assert!(search(&index, "toukyou toukyou").is_empty());
}
