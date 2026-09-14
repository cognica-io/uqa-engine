//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Porter suffix rules over the word's retained character and consonant state.

use super::word::Word;
use crate::AnalysisResult;

pub(super) fn apply(word: &mut Word<'_>) -> AnalysisResult<()> {
    if word.len() > 2 {
        step_1a(word);
        word.check()?;
        step_1b(word)?;
        word.check()?;
        step_1c(word)?;
        word.check()?;
        step_2(word)?;
        word.check()?;
        step_3(word)?;
        word.check()?;
        step_4(word)?;
        word.check()?;
        step_5a(word)?;
        word.check()?;
        step_5b(word)?;
    }
    word.check()
}

fn step_1a(w: &mut Word<'_>) {
    if w.ends_with("sses") || w.ends_with("ies") {
        w.truncate(w.len() - 2);
    } else if !w.ends_with("ss") && w.ends_with("s") {
        w.truncate(w.len() - 1);
    }
}

fn step_1b(w: &mut Word<'_>) -> AnalysisResult<()> {
    if w.ends_with("eed") {
        if w.measure(w.len() - 3)? > 0 {
            w.truncate(w.len() - 1);
        }
        return Ok(());
    }
    let mut matched = false;
    for suffix in ["ed", "ing"] {
        if w.ends_with(suffix) && w.has_vowel(w.len() - suffix.len())? {
            w.truncate(w.len() - suffix.len());
            matched = true;
            break;
        }
    }
    if !matched {
        return Ok(());
    }
    if w.ends_with("at") || w.ends_with("bl") || w.ends_with("iz") {
        w.push('e'.into())?;
    } else if w.double_consonant() && !w[w.len() - 1].is_one_of(&['l', 's', 'z']) {
        w.truncate(w.len() - 1);
    } else if w.measure(w.len())? == 1 && w.cvc(w.len()) {
        w.push('e'.into())?;
    }
    Ok(())
}

fn step_1c(w: &mut Word<'_>) -> AnalysisResult<()> {
    if w.ends_with("y") && w.has_vowel(w.len() - 1)? {
        w.replace_suffix(1, "i")?;
    }
    Ok(())
}

fn apply_replacement_table(w: &mut Word<'_>, table: &[(&str, &str)]) -> AnalysisResult<()> {
    for (suffix, replacement) in table {
        if w.ends_with(suffix) {
            if w.measure(w.len() - suffix.len())? > 0 {
                w.replace_suffix(suffix.len(), replacement)?;
            }
            break;
        }
    }
    Ok(())
}

fn step_2(w: &mut Word<'_>) -> AnalysisResult<()> {
    apply_replacement_table(
        w,
        &[
            ("ational", "ate"),
            ("tional", "tion"),
            ("enci", "ence"),
            ("anci", "ance"),
            ("izer", "ize"),
            ("abli", "able"),
            ("alli", "al"),
            ("entli", "ent"),
            ("eli", "e"),
            ("ousli", "ous"),
            ("ization", "ize"),
            ("ation", "ate"),
            ("ator", "ate"),
            ("alism", "al"),
            ("iveness", "ive"),
            ("fulness", "ful"),
            ("ousness", "ous"),
            ("aliti", "al"),
            ("iviti", "ive"),
            ("biliti", "ble"),
        ],
    )
}

fn step_3(w: &mut Word<'_>) -> AnalysisResult<()> {
    apply_replacement_table(
        w,
        &[
            ("icate", "ic"),
            ("ative", ""),
            ("alize", "al"),
            ("iciti", "ic"),
            ("ical", "ic"),
            ("ful", ""),
            ("ness", ""),
        ],
    )
}

fn step_4(w: &mut Word<'_>) -> AnalysisResult<()> {
    const SUFFIXES: &[&str] = &[
        "al", "ance", "ence", "er", "ic", "able", "ible", "ant", "ement", "ment", "ent", "ion",
        "ou", "ism", "ate", "iti", "ous", "ive", "ize",
    ];
    for suffix in SUFFIXES {
        if w.ends_with(suffix) {
            let stem_len = w.len() - suffix.len();
            if w.measure(stem_len)? > 1
                && (*suffix != "ion" || (stem_len > 0 && w[stem_len - 1].is_one_of(&['s', 't'])))
            {
                w.truncate(stem_len);
            }
            break;
        }
    }
    Ok(())
}

fn step_5a(w: &mut Word<'_>) -> AnalysisResult<()> {
    if w.ends_with("e") {
        let stem_len = w.len() - 1;
        let measure = w.measure(stem_len)?;
        if measure > 1 || (measure == 1 && !w.cvc(stem_len)) {
            w.truncate(stem_len);
        }
    }
    Ok(())
}

fn step_5b(w: &mut Word<'_>) -> AnalysisResult<()> {
    if w.measure(w.len())? > 1 && w.double_consonant() && w[w.len() - 1] == 'l' {
        w.truncate(w.len() - 1);
    }
    Ok(())
}
