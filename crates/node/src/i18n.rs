//! Language packs.
//!
//! A pack is one JSON file. Five ship inside the binary; any others are
//! picked up from `<data dir>/locales/*.json` at request time, so adding a
//! language means dropping a file in and reloading the page — no rebuild, no
//! restart, and nothing to register.
//!
//! Packs are served merged over English, so a partial translation shows
//! English for whatever it has not covered yet rather than a missing-key
//! placeholder. The listing reports coverage so a translator can see what is
//! left.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Packs compiled into the binary. `en` is the canonical key set: every other
/// pack is checked against it, and anything it does not define falls back.
const BUILT_IN: &[(&str, &str)] = &[
    ("en", include_str!("ui/locales/en.json")),
    ("ja", include_str!("ui/locales/ja.json")),
    ("es", include_str!("ui/locales/es.json")),
    ("pt", include_str!("ui/locales/pt.json")),
    ("ar", include_str!("ui/locales/ar.json")),
];

/// The pack format this build understands.
pub const FORMAT_VERSION: u32 = 1;

/// Largest pack file accepted from disk.
const MAX_PACK_BYTES: u64 = 512 * 1024;
/// Most installed packs loaded from the data directory.
const MAX_INSTALLED_PACKS: usize = 64;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalePack {
    /// BCP 47 language tag, e.g. `ja` or `pt-BR`.
    pub locale: String,
    /// The language's name in that language, for the picker.
    pub name: String,
    /// Its name in English, so an operator can read the list.
    pub english_name: String,
    /// `ltr` or `rtl`.
    pub direction: Direction,
    pub format_version: u32,
    /// ISO 3166 countries where this language is spoken, used to pick a
    /// default from the browser's time zone. Optional: a pack that lists
    /// none is still perfectly usable, it just will not be auto-selected by
    /// region.
    #[serde(default)]
    pub regions: Vec<String>,
    pub strings: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Ltr,
    Rtl,
}

impl Direction {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Ltr => "ltr",
            Self::Rtl => "rtl",
        }
    }
}

/// Where a pack came from, so the UI can say which are shipped and which the
/// operator added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackSource {
    BuiltIn,
    Installed,
}

/// One row of the locale listing.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocaleSummary {
    pub locale: String,
    pub name: String,
    pub english_name: String,
    pub direction: Direction,
    pub source: PackSource,
    /// Fraction of the English key set this pack defines, `0.0..=1.0`.
    pub coverage: f64,
    pub regions: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum LocaleError {
    #[error("{path} is not valid JSON: {message}")]
    Malformed { path: String, message: String },
    #[error("{path} declares format version {found}, this build understands {FORMAT_VERSION}")]
    UnsupportedVersion { path: String, found: u32 },
    #[error("{path} is {size} bytes, the limit is {MAX_PACK_BYTES}")]
    TooLarge { path: String, size: u64 },
    #[error("{path} has an empty or malformed locale tag")]
    BadTag { path: String },
}

/// The English pack, parsed once. Every other pack falls back to it.
pub fn base() -> &'static LocalePack {
    static BASE: OnceLock<LocalePack> = OnceLock::new();
    BASE.get_or_init(|| {
        parse(BUILT_IN[0].1, "built-in en").expect("the built-in English pack must parse")
    })
}

fn built_ins() -> &'static BTreeMap<String, LocalePack> {
    static PACKS: OnceLock<BTreeMap<String, LocalePack>> = OnceLock::new();
    PACKS.get_or_init(|| {
        BUILT_IN
            .iter()
            .map(|(code, body)| {
                let pack = parse(body, &format!("built-in {code}"))
                    .unwrap_or_else(|e| panic!("built-in pack {code} must parse: {e}"));
                (pack.locale.clone(), pack)
            })
            .collect()
    })
}

fn parse(body: &str, path: &str) -> Result<LocalePack, LocaleError> {
    let pack: LocalePack = serde_json::from_str(body).map_err(|e| LocaleError::Malformed {
        path: path.to_string(),
        message: e.to_string(),
    })?;
    if pack.format_version != FORMAT_VERSION {
        return Err(LocaleError::UnsupportedVersion {
            path: path.to_string(),
            found: pack.format_version,
        });
    }
    if !is_valid_tag(&pack.locale) {
        return Err(LocaleError::BadTag {
            path: path.to_string(),
        });
    }
    Ok(pack)
}

/// A conservative BCP 47 subset: letters, digits and hyphens, no path
/// characters, because this tag reaches a URL and a filename.
fn is_valid_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 35
        && tag.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !tag.starts_with('-')
        && !tag.ends_with('-')
}

/// Every pack available right now: the built-in ones, plus anything readable
/// in `dir`. An installed pack with the same tag as a built-in replaces it,
/// so a translation can be corrected without waiting for a release.
pub fn load(dir: &Path) -> BTreeMap<String, (LocalePack, PackSource)> {
    let mut packs: BTreeMap<String, (LocalePack, PackSource)> = built_ins()
        .iter()
        .map(|(code, pack)| (code.clone(), (pack.clone(), PackSource::BuiltIn)))
        .collect();

    let Ok(entries) = std::fs::read_dir(dir) else {
        return packs;
    };
    let mut installed = 0;
    for entry in entries.flatten() {
        if installed >= MAX_INSTALLED_PACKS {
            tracing::warn!(
                limit = MAX_INSTALLED_PACKS,
                "ignoring further language packs"
            );
            break;
        }
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        match read_pack(&path) {
            Ok(pack) => {
                installed += 1;
                tracing::info!(locale = %pack.locale, path = %path.display(), "loaded a language pack");
                packs.insert(pack.locale.clone(), (pack, PackSource::Installed));
            }
            Err(e) => tracing::warn!(error = %e, "ignoring a language pack"),
        }
    }
    packs
}

fn read_pack(path: &Path) -> Result<LocalePack, LocaleError> {
    let display = path.display().to_string();
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size > MAX_PACK_BYTES {
        return Err(LocaleError::TooLarge {
            path: display,
            size,
        });
    }
    let body = std::fs::read_to_string(path).map_err(|e| LocaleError::Malformed {
        path: display.clone(),
        message: e.to_string(),
    })?;
    parse(&body, &display)
}

/// The CLDR plural categories. A language may need forms English does not:
/// Arabic distinguishes six, Japanese one. A pack may therefore define
/// `key_two` or `key_few` even though English only has `key_one` and
/// `key_other`, and the browser's `Intl.PluralRules` picks between them.
const PLURAL_CATEGORIES: [&str; 6] = ["zero", "one", "two", "few", "many", "other"];

/// Is this a key the UI might ask for?
///
/// Either English defines it, or it is a plural form of a key English
/// pluralises. Anything else is a typo or a leftover, and is dropped.
pub fn is_known_key(key: &str) -> bool {
    if base().strings.contains_key(key) {
        return true;
    }
    plural_base(key).is_some_and(|stem| base().strings.contains_key(&format!("{stem}_other")))
}

/// `conn.peers_few` -> `conn.peers`, for any CLDR category.
fn plural_base(key: &str) -> Option<String> {
    let (stem, category) = key.rsplit_once('_')?;
    PLURAL_CATEGORIES
        .contains(&category)
        .then(|| stem.to_string())
}

/// A pack's strings with English filling every gap, so the UI always has a
/// complete map and never renders a bare key.
pub fn merged_strings(pack: &LocalePack) -> BTreeMap<String, String> {
    let mut strings = base().strings.clone();
    for (key, value) in &pack.strings {
        // Only keys the UI could ask for: a stale pack must not inject
        // anything else.
        if is_known_key(key) && !value.trim().is_empty() {
            strings.insert(key.clone(), value.clone());
        }
    }
    strings
}

/// How much of the English key set a pack covers.
pub fn coverage(pack: &LocalePack) -> f64 {
    let total = base().strings.len();
    if total == 0 {
        return 1.0;
    }
    let covered = base()
        .strings
        .keys()
        .filter(|key| {
            pack.strings
                .get(*key)
                .is_some_and(|value| !value.trim().is_empty())
        })
        .count();
    covered as f64 / total as f64
}

pub fn summarise(packs: &BTreeMap<String, (LocalePack, PackSource)>) -> Vec<LocaleSummary> {
    packs
        .values()
        .map(|(pack, source)| LocaleSummary {
            locale: pack.locale.clone(),
            name: pack.name.clone(),
            english_name: pack.english_name.clone(),
            direction: pack.direction,
            source: *source,
            coverage: coverage(pack),
            regions: pack.regions.clone(),
        })
        .collect()
}

/// The language the machine running this node is set to, as a BCP 47 tag.
///
/// One more local signal for choosing a default, alongside what the browser
/// asks for. Nothing is looked up anywhere: this is the operating system's
/// own setting.
pub fn system_locale() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(tag) = std::env::var(key).ok().and_then(|v| normalise_posix(&v)) {
            return Some(tag);
        }
    }
    macos_locale()
}

/// `ja_JP.UTF-8` and `pt_BR@euro` both become `ja-JP` / `pt-BR`. `C` and
/// `POSIX` mean "no preference expressed".
fn normalise_posix(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "C" || value == "POSIX" {
        return None;
    }
    let tag: String = value
        .split(['.', '@'])
        .next()
        .unwrap_or_default()
        .replace('_', "-");
    is_valid_tag(&tag).then_some(tag)
}

#[cfg(target_os = "macos")]
fn macos_locale() -> Option<String> {
    // A node started from Finder rather than a terminal inherits no LANG,
    // but the system preference is still readable.
    let output = std::process::Command::new("defaults")
        .args(["read", "-g", "AppleLocale"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    normalise_posix(String::from_utf8_lossy(&output.stdout).trim())
}

#[cfg(not(target_os = "macos"))]
fn macos_locale() -> Option<String> {
    None
}

/// The pack that best serves `tag`, by exact match then by base language.
pub fn best_match<'a>(
    packs: &'a BTreeMap<String, (LocalePack, PackSource)>,
    tag: &str,
) -> Option<&'a LocalePack> {
    let wanted = tag.to_ascii_lowercase();
    if let Some((pack, _)) = packs
        .values()
        .find(|(p, _)| p.locale.to_ascii_lowercase() == wanted)
    {
        return Some(pack);
    }
    let base = wanted.split('-').next().unwrap_or_default();
    packs
        .values()
        .find(|(p, _)| p.locale.to_ascii_lowercase().split('-').next() == Some(base))
        .map(|(pack, _)| pack)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_pack_parses() {
        let packs = built_ins();
        assert_eq!(packs.len(), BUILT_IN.len());
        for (code, _) in BUILT_IN {
            assert!(packs.contains_key(*code), "missing {code}");
        }
    }

    #[test]
    fn english_is_complete_by_definition() {
        assert_eq!(coverage(base()), 1.0);
        assert!(base().strings.len() > 100);
    }

    #[test]
    fn every_shipped_translation_is_complete() {
        // A shipped pack with gaps would show English in the middle of a
        // sentence. Community packs may be partial; ours may not.
        for (code, pack) in built_ins() {
            let missing: Vec<&String> = base()
                .strings
                .keys()
                .filter(|key| !pack.strings.contains_key(*key))
                .collect();
            assert!(
                missing.is_empty(),
                "the {code} pack is missing {} keys, first few: {:?}",
                missing.len(),
                &missing[..missing.len().min(5)]
            );
        }
    }

    #[test]
    fn no_shipped_pack_defines_a_key_english_does_not() {
        // A key English has dropped is dead weight, and a typo in a key is
        // otherwise invisible. Extra plural forms are the one exception.
        for (code, pack) in built_ins() {
            let extra: Vec<&String> = pack
                .strings
                .keys()
                .filter(|key| !is_known_key(key))
                .collect();
            assert!(
                extra.is_empty(),
                "the {code} pack defines unknown keys: {extra:?}"
            );
        }
    }

    #[test]
    fn a_language_may_add_the_plural_forms_it_needs() {
        // Arabic has six categories where English has two.
        assert!(is_known_key("conn.peers_one"));
        assert!(is_known_key("conn.peers_few"));
        assert!(is_known_key("conn.peers_many"));
        assert!(is_known_key("viewer.search.results_two"));
        // But only for keys that are actually plural, and only real
        // categories.
        assert!(!is_known_key("nav.browse_few"));
        assert!(!is_known_key("conn.peers_plural"));
        assert!(!is_known_key("made.up.key"));
    }

    #[test]
    fn arabic_provides_more_plural_forms_than_english() {
        let arabic = &built_ins()["ar"];
        for stem in ["conn.peers", "viewer.search.results", "admin.peers.known"] {
            let forms: Vec<&str> = PLURAL_CATEGORIES
                .iter()
                .filter(|c| arabic.strings.contains_key(&format!("{stem}_{c}")))
                .copied()
                .collect();
            assert_eq!(
                forms.len(),
                PLURAL_CATEGORIES.len(),
                "{stem} should have every Arabic plural form, has {forms:?}"
            );
        }
    }

    #[test]
    fn placeholders_match_the_english_they_replace() {
        // `{count}` in English must still be `{count}` in every translation,
        // or the number silently disappears.
        let placeholders = |text: &str| -> std::collections::BTreeSet<String> {
            let mut out = std::collections::BTreeSet::new();
            let mut rest = text;
            while let Some(open) = rest.find('{') {
                rest = &rest[open + 1..];
                if let Some(close) = rest.find('}') {
                    out.insert(rest[..close].to_string());
                    rest = &rest[close + 1..];
                }
            }
            out
        };

        for (code, pack) in built_ins() {
            for (key, translated) in &pack.strings {
                // An extra plural form is compared against the English
                // `_other`, which is the form it stands in for.
                let english = base().strings.get(key).or_else(|| {
                    plural_base(key).and_then(|stem| base().strings.get(&format!("{stem}_other")))
                });
                let Some(english) = english else { continue };
                assert_eq!(
                    placeholders(english),
                    placeholders(translated),
                    "{code}: placeholders differ for {key:?}\n  en: {english}\n  {code}: {translated}"
                );
            }
        }
    }

    #[test]
    fn arabic_is_right_to_left_and_the_others_are_not() {
        let packs = built_ins();
        assert_eq!(packs["ar"].direction, Direction::Rtl);
        for code in ["en", "ja", "es", "pt"] {
            assert_eq!(packs[code].direction, Direction::Ltr, "{code}");
        }
    }

    #[test]
    fn each_pack_names_itself_in_its_own_language() {
        let packs = built_ins();
        assert_eq!(packs["ja"].name, "日本語");
        assert_eq!(packs["en"].name, "English");
        for (code, pack) in packs {
            assert!(!pack.name.trim().is_empty(), "{code} has no name");
            assert!(
                !pack.english_name.trim().is_empty(),
                "{code} has no English name"
            );
        }
    }

    #[test]
    fn merging_fills_gaps_from_english_and_ignores_unknown_keys() {
        let mut partial = base().clone();
        partial.locale = "xx".into();
        partial.strings.clear();
        partial
            .strings
            .insert("nav.browse".into(), "Parcourir".into());
        partial
            .strings
            .insert("not.a.real.key".into(), "ignored".into());
        // An empty string is a gap, not a translation.
        partial.strings.insert("nav.library".into(), "  ".into());

        let merged = merged_strings(&partial);
        assert_eq!(merged["nav.browse"], "Parcourir");
        assert_eq!(merged["nav.library"], base().strings["nav.library"]);
        assert_eq!(merged.len(), base().strings.len());
        assert!(!merged.contains_key("not.a.real.key"));
    }

    #[test]
    fn coverage_counts_only_real_translations() {
        let mut partial = base().clone();
        partial.strings.clear();
        assert_eq!(coverage(&partial), 0.0);
        partial.strings.insert("nav.browse".into(), "x".into());
        assert!(coverage(&partial) > 0.0 && coverage(&partial) < 0.1);
    }

    #[test]
    fn locale_tags_that_could_escape_a_path_are_refused() {
        for tag in [
            "",
            "../etc",
            "en/../..",
            "a".repeat(64).as_str(),
            "-en",
            "en-",
            "en_US",
        ] {
            assert!(!is_valid_tag(tag), "{tag:?} should be refused");
        }
        for tag in ["en", "ja", "pt-BR", "zh-Hant-TW"] {
            assert!(is_valid_tag(tag), "{tag:?} should be accepted");
        }
    }

    #[test]
    fn a_pack_dropped_into_the_directory_is_picked_up() {
        let dir = tempfile::tempdir().unwrap();
        let pack = serde_json::json!({
            "locale": "tlh",
            "name": "tlhIngan Hol",
            "englishName": "Klingon",
            "direction": "ltr",
            "formatVersion": 1,
            "strings": { "nav.browse": "wIv" }
        });
        std::fs::write(dir.path().join("tlh.json"), pack.to_string()).unwrap();

        let packs = load(dir.path());
        let (loaded, source) = packs.get("tlh").expect("the dropped pack should load");
        assert_eq!(source, &PackSource::Installed);
        assert_eq!(merged_strings(loaded)["nav.browse"], "wIv");
        // …and the built-in ones are still there.
        assert!(packs.contains_key("ja"));
    }

    #[test]
    fn an_installed_pack_can_correct_a_shipped_one() {
        let dir = tempfile::tempdir().unwrap();
        let pack = serde_json::json!({
            "locale": "ja",
            "name": "日本語 (修正版)",
            "englishName": "Japanese (corrected)",
            "direction": "ltr",
            "formatVersion": 1,
            "strings": { "nav.browse": "さがす" }
        });
        std::fs::write(dir.path().join("ja.json"), pack.to_string()).unwrap();

        let packs = load(dir.path());
        let (loaded, source) = &packs["ja"];
        assert_eq!(source, &PackSource::Installed);
        assert_eq!(merged_strings(loaded)["nav.browse"], "さがす");
    }

    #[test]
    fn broken_packs_are_skipped_rather_than_breaking_the_node() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("broken.json"), "{ not json").unwrap();
        std::fs::write(
            dir.path().join("future.json"),
            serde_json::json!({
                "locale": "xx", "name": "X", "englishName": "X",
                "direction": "ltr", "formatVersion": 99, "strings": {}
            })
            .to_string(),
        )
        .unwrap();
        std::fs::write(dir.path().join("notes.txt"), "ignored").unwrap();

        let packs = load(dir.path());
        assert!(!packs.contains_key("xx"));
        assert_eq!(packs.len(), BUILT_IN.len());
    }

    #[test]
    fn posix_locales_become_bcp47_tags() {
        assert_eq!(normalise_posix("ja_JP.UTF-8").as_deref(), Some("ja-JP"));
        assert_eq!(normalise_posix("pt_BR@euro").as_deref(), Some("pt-BR"));
        assert_eq!(normalise_posix("en").as_deref(), Some("en"));
        assert_eq!(normalise_posix("ar_EG.utf8").as_deref(), Some("ar-EG"));
        // "No preference" rather than a language called C.
        assert_eq!(normalise_posix("C"), None);
        assert_eq!(normalise_posix("POSIX"), None);
        assert_eq!(normalise_posix(""), None);
        assert_eq!(normalise_posix("   "), None);
    }

    #[test]
    fn a_tag_matches_its_pack_exactly_or_by_language() {
        let packs = load(Path::new("/nonexistent"));
        assert_eq!(
            best_match(&packs, "ja").map(|p| p.locale.as_str()),
            Some("ja")
        );
        // Brazil and Portugal both get the Portuguese pack.
        assert_eq!(
            best_match(&packs, "pt-BR").map(|p| p.locale.as_str()),
            Some("pt")
        );
        assert_eq!(
            best_match(&packs, "PT-pt").map(|p| p.locale.as_str()),
            Some("pt")
        );
        assert_eq!(
            best_match(&packs, "es-MX").map(|p| p.locale.as_str()),
            Some("es")
        );
        assert!(best_match(&packs, "de-DE").is_none());
    }

    #[test]
    fn every_shipped_pack_claims_the_regions_it_serves() {
        let packs = built_ins();
        assert_eq!(packs["ja"].regions, vec!["JP"]);
        for (code, expected) in [("es", "MX"), ("pt", "BR"), ("ar", "EG"), ("en", "US")] {
            assert!(
                packs[code].regions.iter().any(|r| r == expected),
                "{code} should claim {expected}"
            );
        }
        // A country belongs to one pack, or the choice would be arbitrary.
        let mut seen: BTreeMap<&str, &str> = BTreeMap::new();
        for (code, pack) in packs {
            for region in &pack.regions {
                if let Some(other) = seen.insert(region, code) {
                    panic!("{region} is claimed by both {other} and {code}");
                }
            }
        }
    }

    #[test]
    fn a_pack_without_regions_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("xx.json"),
            serde_json::json!({
                "locale": "xx", "name": "X", "englishName": "X",
                "direction": "ltr", "formatVersion": 1, "strings": {}
            })
            .to_string(),
        )
        .unwrap();
        let packs = load(dir.path());
        assert!(packs["xx"].0.regions.is_empty());
    }

    #[test]
    fn a_missing_directory_is_not_an_error() {
        let packs = load(Path::new("/nonexistent/locales"));
        assert_eq!(packs.len(), BUILT_IN.len());
    }
}
