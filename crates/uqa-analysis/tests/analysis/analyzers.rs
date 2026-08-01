use super::*;

// =====================================================================
// Analyzer
// =====================================================================

#[test]
fn default_analyzer_is_standard() {
    let standard = standard_analyzer("english");
    let result = standard.analyze("The Quick BROWN Fox").unwrap();
    assert!(!result.contains(&"the".to_string()));
    assert!(result.contains(&"quick".to_string()));
    assert!(result.contains(&"brown".to_string()));
    assert!(result.contains(&"fox".to_string()));
}

#[test]
fn whitespace_analyzer_run() {
    let a = whitespace_analyzer();
    assert_eq!(a.analyze("Hello World").unwrap(), vec!["hello", "world"]);
}

#[test]
fn standard_analyzer_run() {
    let a = standard_analyzer("english");
    let result = a.analyze("The quick brown fox").unwrap();
    assert!(!result.contains(&"the".to_string()));
    assert!(result.contains(&"quick".to_string()));
}

#[test]
fn standard_analyzer_stemming() {
    let a = standard_analyzer("english");
    let result = a.analyze("Running transformers efficiently").unwrap();
    assert!(result.contains(&"run".to_string()));
    assert!(result.contains(&"transform".to_string()));
}

#[test]
fn standard_analyzer_ascii_folding() {
    let a = standard_analyzer("english");
    let result = a.analyze("café résumé").unwrap();
    assert!(result.contains(&"cafe".to_string()));
    assert!(result.contains(&"resum".to_string()));
}

#[test]
fn standard_cjk_analyzer_run() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("hello world").unwrap();
    assert!(result.contains(&"he".to_string()));
    assert!(result.contains(&"hel".to_string()));
    assert!(result.contains(&"wo".to_string()));
    assert!(result.contains(&"wor".to_string()));
}

#[test]
fn standard_cjk_analyzer_stemming() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("Running").unwrap();
    assert!(result.contains(&"ru".to_string()));
    assert!(result.contains(&"run".to_string()));
}

#[test]
fn standard_cjk_analyzer_keep_short() {
    let a = standard_cjk_analyzer("english");
    let result = a.analyze("x marks").unwrap();
    assert!(result.contains(&"x".to_string()));
    assert!(result.contains(&"ma".to_string()));
    assert!(result.contains(&"mar".to_string()));
}

#[test]
fn keyword_analyzer_run() {
    let a = keyword_analyzer();
    assert_eq!(a.analyze("hello world").unwrap(), vec!["hello world"]);
}

#[test]
fn analyzer_custom_pipeline() {
    let a = Analyzer::new(
        Tokenizer::Standard,
        vec![TokenFilter::Lowercase, TokenFilter::PorterStem],
        vec![CharFilter::HTMLStrip],
    );
    let result = a.analyze("<p>Running Connections</p>").unwrap();
    assert!(result.contains(&"run".to_string()));
    assert!(result.contains(&"connect".to_string()));
}

#[test]
fn analyzer_serialization_roundtrip() {
    let a = Analyzer::new(
        Tokenizer::Standard,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Stop {
                language: "english".into(),
                custom_words: Vec::new(),
            },
        ],
        vec![CharFilter::HTMLStrip],
    );
    let json = serde_json::to_string(&a).unwrap();
    let back: Analyzer = serde_json::from_str(&json).unwrap();
    let text = "<p>The quick brown fox</p>";
    assert_eq!(back.analyze(text).unwrap(), a.analyze(text).unwrap());
}

#[test]
fn analyzer_json_roundtrip() {
    let a = standard_analyzer("english");
    let j = serde_json::to_string(&a).unwrap();
    let back: Analyzer = serde_json::from_str(&j).unwrap();
    let text = "The quick brown fox";
    assert_eq!(back.analyze(text).unwrap(), a.analyze(text).unwrap());
}

// =====================================================================
// Named Analyzer Registry
// =====================================================================

#[test]
fn registry_register_and_get() {
    use uqa_analysis::{drop_analyzer, get_analyzer, register_analyzer};
    let custom = Analyzer::new(Tokenizer::Letter, vec![TokenFilter::Lowercase], Vec::new());
    register_analyzer("rs_test_custom_reg".to_string(), custom).unwrap();
    let retrieved = get_analyzer("rs_test_custom_reg").unwrap();
    assert_eq!(
        retrieved.analyze("hello123world").unwrap(),
        vec!["hello", "world"]
    );
    drop_analyzer("rs_test_custom_reg").unwrap();
}

#[test]
fn registry_unknown_analyzer() {
    use uqa_analysis::get_analyzer;
    let r = get_analyzer("rs_nonexistent_analyzer_xyz");
    assert!(r.is_err());
}

#[test]
fn registry_drop_nonexistent() {
    use uqa_analysis::drop_analyzer;
    let r = drop_analyzer("rs_nonexistent_analyzer_xyz_drop");
    assert!(r.is_err());
}

#[test]
fn registry_list_includes_registered() {
    use uqa_analysis::{drop_analyzer, list_analyzers, register_analyzer};
    register_analyzer(
        "rs_test_listed".to_string(),
        Analyzer::new(Tokenizer::Whitespace, Vec::new(), Vec::new()),
    )
    .unwrap();
    let names = list_analyzers();
    assert!(names.contains(&"rs_test_listed".to_string()));
    drop_analyzer("rs_test_listed").unwrap();
}

#[test]
fn ngram_filter_validation_zero_min() {
    let f = TokenFilter::Ngram {
        min_gram: 0,
        max_gram: 1,
        keep_short: false,
    };
    assert!(matches!(
        f.filter(vec!["a".into()]),
        Err(AnalysisError::InvalidGramBounds {
            component: "n-gram token filter",
            min_gram: 0,
            max_gram: 1,
        })
    ));
}

#[test]
fn synonym_filter_chain_with_lowercase() {
    let mut m = BTreeMap::new();
    m.insert("car".to_string(), vec!["automobile".to_string()]);
    let pipeline = Analyzer::new(
        Tokenizer::Whitespace,
        vec![
            TokenFilter::Lowercase,
            TokenFilter::Synonym {
                synonyms: m,
                synonyms_path: None,
            },
        ],
        Vec::new(),
    );
    let result = pipeline.analyze("Used CAR for sale").unwrap();
    assert!(result.contains(&"car".to_string()));
    assert!(result.contains(&"automobile".to_string()));
}
