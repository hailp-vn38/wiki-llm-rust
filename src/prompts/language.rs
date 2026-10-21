//! Standard Language Directives & Script Detection for LLM Wiki Prompts.
//!
//! Ported from `nashsu/llm_wiki` (`src/lib/output-language.ts` & `detect-language.ts`),
//! adapted for `llm-wiki-rs` so that all prompt frameworks remain in strict English
//! (for maximum instruction-following adherence across LLMs) while enforcing output prose
//! in the target language and preserving proper nouns / technical identifiers.

use serde::{Deserialize, Serialize};

/// Target output language configuration for LLM Wiki prompts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum OutputLanguage {
    /// Detect automatically from the source / query text (falls back to English).
    #[default]
    Auto,
    /// Vietnamese (`"Vietnamese"`).
    Vietnamese,
    /// English (`"English"`).
    English,
    /// Simplified Chinese (`"Simplified Chinese"`).
    ChineseSimplified,
    /// Traditional Chinese (`"Traditional Chinese"`).
    ChineseTraditional,
    /// Japanese (`"Japanese"`).
    Japanese,
    /// Korean (`"Korean"`).
    Korean,
    /// Custom language prompt name (e.g. `"French"`, `"German"`, `"Spanish"`).
    Custom(String),
}

impl OutputLanguage {
    /// Parse a language code or name (e.g. `"vi"`, `"en"`, `"zh"`, `"auto"`, `"Vietnamese"`).
    pub fn from_code_or_name(raw: &str) -> Self {
        let lower = raw.trim().to_ascii_lowercase();
        match lower.as_str() {
            "" | "auto" => Self::Auto,
            "vi" | "vie" | "vietnamese" | "tiếng việt" | "tieng viet" => Self::Vietnamese,
            "en" | "eng" | "english" => Self::English,
            "zh" | "zh-cn" | "zh-hans" | "chinese" | "simplified chinese" => {
                Self::ChineseSimplified
            }
            "zh-tw" | "zh-hant" | "traditional chinese" => Self::ChineseTraditional,
            "ja" | "jpn" | "japanese" => Self::Japanese,
            "ko" | "kor" | "korean" => Self::Korean,
            _ => Self::Custom(raw.trim().to_string()),
        }
    }

    /// Resolve the human-readable prompt language name, using `fallback_sample` when `Self::Auto`.
    pub fn resolve_prompt_name(&self, fallback_sample: &str) -> String {
        match self {
            Self::Auto => detect_language_prompt_name(fallback_sample).to_string(),
            Self::Vietnamese => "Vietnamese".to_string(),
            Self::English => "English".to_string(),
            Self::ChineseSimplified => "Simplified Chinese".to_string(),
            Self::ChineseTraditional => "Traditional Chinese".to_string(),
            Self::Japanese => "Japanese".to_string(),
            Self::Korean => "Korean".to_string(),
            Self::Custom(name) => {
                if name.trim().is_empty() {
                    detect_language_prompt_name(fallback_sample).to_string()
                } else {
                    name.trim().to_string()
                }
            }
        }
    }
}

/// Lightweight heuristic detector for common scripts and Vietnamese diacritics.
pub fn detect_language_prompt_name(sample: &str) -> &'static str {
    let mut vi_diacritics = 0usize;
    let mut han_chars = 0usize;
    let mut kana_chars = 0usize;
    let mut hangul_chars = 0usize;
    let mut cyrillic_chars = 0usize;

    const VI_CHARS: &str = "àáảãạăằắẳẵặâầấẩẫậèéẻẽẹêềếểễệìíỉĩịòóỏõọôồốổỗộơờớởỡợùúủũụưừứửữựỳýỷỹỵđÀÁẢÃẠĂẰẮẲẴẶÂẦẤẨẪẬÈÉẺẼẸÊỀẾỂỄỆÌÍỈĨỊÒÓỎÕỌÔỒỐỔỖỘƠỜỚỞỠỢÙÚỦŨỤƯỪỨỬỮỰỲÝỶỸỴĐ";

    for ch in sample.chars().take(4_000) {
        if VI_CHARS.contains(ch) {
            vi_diacritics += 1;
        } else if ('\u{3040}'..='\u{30ff}').contains(&ch) {
            kana_chars += 1;
        } else if ('\u{ac00}'..='\u{d7af}').contains(&ch) {
            hangul_chars += 1;
        } else if ('\u{4e00}'..='\u{9fff}').contains(&ch) {
            han_chars += 1;
        } else if ('\u{0400}'..='\u{04ff}').contains(&ch) {
            cyrillic_chars += 1;
        }
    }

    if vi_diacritics >= 3 {
        "Vietnamese"
    } else if kana_chars >= 3 {
        "Japanese"
    } else if hangul_chars >= 3 {
        "Korean"
    } else if han_chars >= 5 {
        "Simplified Chinese"
    } else if cyrillic_chars >= 5 {
        "Russian"
    } else {
        "English"
    }
}

/// Build the mandatory language directive block injected into system prompts.
///
/// Mirrors `buildLanguageDirective()` from `nashsu/llm_wiki`:
/// - Forces natural-language prose, titles, and section headings into the target language.
/// - Preserves proper nouns, model/tool/library names, code identifiers, file paths, and URLs in their original form.
pub fn build_language_directive(lang: &OutputLanguage, fallback_sample: &str) -> String {
    let prompt_lang = lang.resolve_prompt_name(fallback_sample);
    [
        format!("## ⚠️ MANDATORY OUTPUT LANGUAGE: {prompt_lang}"),
        String::new(),
        format!("Write surrounding natural-language prose in **{prompt_lang}**."),
        format!("All generated prose, including prose titles and section headings, must be in {prompt_lang}."),
        "Do not translate, transliterate, or describe proper nouns and technical identifiers unless the source already uses a well-established localized form.".to_string(),
        "Preserve organization names, product names, model names, dataset names, tool/library names, acronyms, code identifiers, file names, URLs, paper titles, citation strings, and technical terms that have no widely-used localized equivalent in their standard original form.".to_string(),
        format!("The source material or wiki content may be in a different language; use it as evidence, but keep generated prose in {prompt_lang}."),
        "This language rule overrides weaker style instructions, but it does not override the proper-noun and technical-identifier preservation rule above.".to_string(),
    ]
    .join("\n")
}

/// Short language reminder placed at the very end of generation prompts (recency bias tie-breaker).
pub fn build_language_reminder(lang: &OutputLanguage, fallback_sample: &str) -> String {
    let prompt_lang = lang.resolve_prompt_name(fallback_sample);
    format!(
        "REMINDER: Write prose in {prompt_lang}; preserve names, acronyms, identifiers, URLs, file names, and paper titles in their standard original form."
    )
}
