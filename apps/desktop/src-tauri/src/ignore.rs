//! Compiled ignore-rule evaluator, the Rust twin of the native client's
//! IgnoreRules.swift. Built once from IgnoreSettings (regexes compiled, file
//! patterns parsed), rebuilt by save_settings when the ignore object changes;
//! evaluate() then runs on every clipboard change without re-parsing.
//!
//! Rule semantics (kept in step with the Swift side):
//! - Applications and regexes act on text only; file patterns on file lists
//!   only; pasteboard types on every kind
//! - Regexes run exactly as written, standard search semantics: a match
//!   anywhere in the text counts, anchor with ^ $ for an exact match. A
//!   pattern that fails to compile degrades to a literal substring check —
//!   this also catches ICU-only syntax written on the native client
//!   (lookahead and friends), which the regex crate deliberately lacks
//! - File patterns: one per line, `#` comments and blank lines skipped, no
//!   `!` negation. A pattern containing `/` matches against the full path,
//!   otherwise against the file name; glob syntax (* ? [..]) with `*`
//!   crossing `/`, case-insensitive — all matching the fnmatch flags the
//!   Swift side uses. File rules **filter** rather than suppress: matched
//!   paths are dropped from the affected pipeline and the rest of the batch
//!   goes through (copying a.yaml + b.png with *.yaml ignored still syncs
//!   b.png); a batch matched in full ends up empty and is skipped whole.
//! - A rule kind whose both toggles are off contributes nothing; hits from
//!   several kinds OR their toggles together (the type rule covers Files
//!   too and suppresses the whole batch — the type marker is a property of
//!   the clipboard change, not of any one file)

use std::collections::HashSet;

use lanecho_core::clipboard::ClipboardContent;

use crate::settings::IgnoreSettings;

/// The verdict for one clipboard change
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IgnoreVerdict {
    /// Do not broadcast to peers (the LWW baseline still advances)
    pub suppress_sync: bool,
    /// Do not record into the local history
    pub suppress_record: bool,
    /// File-rule hits to drop from the broadcast (empty when the files-sync
    /// toggle is off or nothing matched); the rest of the batch still syncs
    pub broadcast_files_excluded: Vec<std::path::PathBuf>,
    /// File-rule hits to drop from the history recording (same filtering
    /// semantics on the record leg)
    pub record_files_excluded: Vec<std::path::PathBuf>,
}

impl IgnoreVerdict {
    /// OR-merge the toggles of one matched rule kind
    fn merge(&mut self, sync: bool, record: bool) {
        self.suppress_sync |= sync;
        self.suppress_record |= record;
    }
}

/// One parsed file pattern
struct FilePattern {
    /// The glob, as written
    glob: String,
    /// A pattern containing "/" matches the full path, otherwise the name
    against_full_path: bool,
}

/// The compiled evaluator
pub struct IgnoreRules {
    /// Identifiers of ignored applications (bundle id on macOS, lowercased
    /// exe name on Windows)
    app_ids: HashSet<String>,
    /// Ignored pasteboard types / clipboard format names
    types: HashSet<String>,
    /// Compiled regexes, exactly as the user wrote them (search semantics)
    regexes: Vec<regex::Regex>,
    /// Patterns that failed to compile, kept as literal substring checks
    literals: Vec<String>,
    /// Parsed file patterns
    file_patterns: Vec<FilePattern>,
    /// The toggle pairs (the rest of the settings is not retained)
    config: IgnoreSettings,
}

impl IgnoreRules {
    /// Compile from the settings
    pub fn new(settings: &IgnoreSettings) -> Self {
        let mut regexes = Vec::new();
        let mut literals = Vec::new();
        for pattern in settings.regexes.iter().filter(|p| !p.is_empty()) {
            match regex::Regex::new(pattern) {
                Ok(compiled) => regexes.push(compiled),
                Err(_) => literals.push(pattern.clone()),
            }
        }
        let file_patterns = settings
            .file_patterns
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .map(|line| FilePattern {
                glob: line.to_string(),
                against_full_path: line.contains('/'),
            })
            .collect();
        Self {
            app_ids: settings.apps.iter().map(|app| app.id.clone()).collect(),
            types: settings.types.iter().cloned().collect(),
            regexes,
            literals,
            file_patterns,
            config: settings.clone(),
        }
    }

    /// Evaluate one clipboard change
    ///
    /// - `pasteboard_types`: the type snapshot the watcher took at read time
    /// - `source_app_id`: the frontmost application's identifier; pass None
    ///   to exempt the application rule (restore writes — the content does
    ///   not come from whatever happens to be frontmost)
    pub fn evaluate(
        &self,
        content: &ClipboardContent,
        pasteboard_types: &[String],
        source_app_id: Option<&str>,
    ) -> IgnoreVerdict {
        let mut verdict = IgnoreVerdict::default();
        if !self.types.is_empty() && pasteboard_types.iter().any(|t| self.types.contains(t)) {
            verdict.merge(self.config.types_sync, self.config.types_record);
        }
        match content {
            ClipboardContent::Text(text) => {
                if source_app_id.is_some_and(|id| self.app_ids.contains(id)) {
                    verdict.merge(self.config.apps_sync, self.config.apps_record);
                }
                if self.matches_text(text) {
                    verdict.merge(self.config.regex_sync, self.config.regex_record);
                }
            }
            ClipboardContent::Files(paths) if !self.file_patterns.is_empty() => {
                // Filtering semantics: collect the hits and hand them to
                // whichever legs the toggles arm; the pipelines drop them
                // and keep the rest of the batch
                let hits: Vec<std::path::PathBuf> = paths
                    .iter()
                    .filter(|path| self.matches_file(path))
                    .cloned()
                    .collect();
                if !hits.is_empty() {
                    if self.config.files_sync {
                        verdict.broadcast_files_excluded = hits.clone();
                    }
                    if self.config.files_record {
                        verdict.record_files_excluded = hits;
                    }
                }
            }
            _ => {}
        }
        verdict
    }

    /// Whether anything can ever match by source application (lets the ingest
    /// skip the frontmost query when no rule needs it)
    pub fn wants_source_app(&self) -> bool {
        !self.app_ids.is_empty() && (self.config.apps_sync || self.config.apps_record)
    }

    /// Regex search or literal substring match
    fn matches_text(&self, text: &str) -> bool {
        self.literals.iter().any(|lit| text.contains(lit.as_str()))
            || self.regexes.iter().any(|re| re.is_match(text))
    }

    /// One path against the pattern list
    fn matches_file(&self, path: &std::path::Path) -> bool {
        let full = path.to_string_lossy();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        self.file_patterns.iter().any(|pattern| {
            let target = if pattern.against_full_path {
                full.as_ref()
            } else {
                name.as_ref()
            };
            glob_match(&pattern.glob, target)
        })
    }
}

/// Case-insensitive glob match over the fnmatch subset the Swift side uses:
/// `*` (any run, `/` included — FNM_PATHNAME is not set there), `?` (any one
/// character), `[..]` character classes with `!`/`^` negation and `a-z`
/// ranges. No escaping — the rule editor documents the subset.
///
/// Iterative two-pointer algorithm with the classic star backtrack; linear in
/// practice, worst-case O(n·m), fine for path-sized inputs.
fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let (mut p, mut t) = (0usize, 0usize);
    // The position to resume from after the most recent `*` on a mismatch
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pattern.len() {
            match pattern[p] {
                '*' => {
                    // Record the star and first try matching it to nothing
                    star = Some((p, t));
                    p += 1;
                    continue;
                }
                '?' => {
                    p += 1;
                    t += 1;
                    continue;
                }
                '[' => {
                    if let Some((true, next_p)) = match_class(&pattern, p, text[t]) {
                        p = next_p;
                        t += 1;
                        continue;
                    }
                    // Unclosed class or no member matched: fall through to
                    // the star backtrack below
                }
                literal => {
                    if literal == text[t] {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        // Mismatch: grow the most recent star by one character, or fail
        match star {
            Some((star_p, star_t)) => {
                p = star_p + 1;
                t = star_t + 1;
                star = Some((star_p, star_t + 1));
            }
            None => return false,
        }
    }
    // Text consumed: only trailing stars may remain
    pattern[p..].iter().all(|&c| c == '*')
}

/// Match one character against the `[..]` class starting at `open` (the `[`);
/// returns (matched, index past the closing `]`), or None when the class
/// never closes
fn match_class(pattern: &[char], open: usize, ch: char) -> Option<(bool, usize)> {
    let mut i = open + 1;
    let negated = matches!(pattern.get(i), Some('!' | '^'));
    if negated {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < pattern.len() {
        let c = pattern[i];
        // A `]` in the leading position is a literal member; later it closes
        if c == ']' && !first {
            return Some((matched != negated, i + 1));
        }
        first = false;
        // Range a-z (a trailing `-` is a literal)
        if pattern.get(i + 1) == Some(&'-') && pattern.get(i + 2).is_some_and(|&e| e != ']') {
            let end = pattern[i + 2];
            if c <= ch && ch <= end {
                matched = true;
            }
            i += 3;
        } else {
            if c == ch {
                matched = true;
            }
            i += 1;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::IgnoredApp;

    /// Rules over a settings value patched by the caller
    fn rules(patch: impl FnOnce(&mut IgnoreSettings)) -> IgnoreRules {
        let mut settings = IgnoreSettings::default();
        patch(&mut settings);
        IgnoreRules::new(&settings)
    }

    fn text(s: &str) -> ClipboardContent {
        ClipboardContent::Text(s.to_string())
    }

    fn files(paths: &[&str]) -> ClipboardContent {
        ClipboardContent::Files(paths.iter().map(std::path::PathBuf::from).collect())
    }

    /// Application rule: id hit on text only; None exempts (restore writes)
    #[test]
    fn app_rule_matches_text_by_id() {
        let rules = rules(|s| {
            s.apps = vec![IgnoredApp {
                id: "com.google.Chrome".into(),
                name: "Chrome".into(),
            }];
            s.apps_record = true;
        });
        let hit = rules.evaluate(&text("hello"), &[], Some("com.google.Chrome"));
        assert!(hit.suppress_sync && hit.suppress_record);
        assert_eq!(
            rules.evaluate(&text("hello"), &[], Some("com.apple.Safari")),
            IgnoreVerdict::default()
        );
        assert_eq!(
            rules.evaluate(&text("hello"), &[], None),
            IgnoreVerdict::default(),
            "None source (a restore write) must exempt the app rule"
        );
        let image = ClipboardContent::Image {
            width: 1,
            height: 1,
            rgba: vec![0, 0, 0, 255],
        };
        assert_eq!(
            rules.evaluate(&image, &[], Some("com.google.Chrome")),
            IgnoreVerdict::default(),
            "The app rule acts on text only"
        );
    }

    /// Type rule: any overlap with the snapshot counts, whatever the kind
    #[test]
    fn type_rule_matches_snapshot_overlap() {
        let rules = rules(|_| {});
        let hit = rules.evaluate(
            &text("secret"),
            &["public.utf8-plain-text".into(), "net.antelle.keeweb".into()],
            None,
        );
        assert!(hit.suppress_sync && !hit.suppress_record);
        assert_eq!(
            rules.evaluate(&text("plain"), &["public.utf8-plain-text".into()], None),
            IgnoreVerdict::default()
        );
    }

    /// Regex rule: search semantics as written, user anchors bind, an
    /// uncompilable pattern degrades to a literal substring check
    #[test]
    fn regex_rule_matches_by_search() {
        let rules = rules(|s| {
            s.regexes = vec!["[0-9]{6}".into(), "^secret$".into(), "([invalid".into()];
        });
        assert!(rules.evaluate(&text("834721"), &[], None).suppress_sync);
        assert!(
            rules
                .evaluate(&text("验证码 834721"), &[], None)
                .suppress_sync,
            "Search semantics: a match anywhere counts"
        );
        assert!(rules.evaluate(&text("secret"), &[], None).suppress_sync);
        assert_eq!(
            rules.evaluate(&text("my secret!"), &[], None),
            IgnoreVerdict::default(),
            "User-written anchors bind as written"
        );
        assert!(
            rules
                .evaluate(&text("see ([invalid here"), &[], None)
                .suppress_sync,
            "An uncompilable pattern degrades to a literal substring check"
        );
    }

    /// File rule: name vs full-path globs, comments, filtering semantics
    /// (hits are excluded, the rest of the batch goes through),
    /// case-insensitive
    #[test]
    fn file_rule_filters_matched_paths() {
        let filtering = rules(|s| {
            s.file_patterns = "# keys never leave\n*.key\n\n/Users/*/secrets/*".into();
            s.files_record = true;
        });
        let partial = filtering.evaluate(&files(&["/tmp/a.yaml", "/tmp/b.KEY"]), &[], None);
        assert_eq!(
            partial.broadcast_files_excluded,
            vec![std::path::PathBuf::from("/tmp/b.KEY")],
            "Only the hits are excluded (case-insensitive); the rest still syncs"
        );
        assert_eq!(
            partial.record_files_excluded,
            vec![std::path::PathBuf::from("/tmp/b.KEY")],
            "The record toggle arms the record leg with the same hits"
        );
        assert!(
            !partial.suppress_sync && !partial.suppress_record,
            "File rules filter; they never suppress the whole event"
        );
        assert!(
            !filtering
                .evaluate(&files(&["/Users/zero/secrets/token.txt"]), &[], None)
                .broadcast_files_excluded
                .is_empty(),
            "A pattern containing / matches the full path"
        );
        assert_eq!(
            filtering.evaluate(&files(&["/tmp/notes.txt"]), &[], None),
            IgnoreVerdict::default()
        );
        assert_eq!(
            filtering.evaluate(&text("*.key"), &[], None),
            IgnoreVerdict::default(),
            "The file rule leaves text alone"
        );

        // The sync toggle off leaves the broadcast leg unarmed
        let record_only = rules(|s| {
            s.file_patterns = "*.key".into();
            s.files_sync = false;
            s.files_record = true;
        });
        let verdict = record_only.evaluate(&files(&["/tmp/b.key"]), &[], None);
        assert!(verdict.broadcast_files_excluded.is_empty());
        assert_eq!(verdict.record_files_excluded.len(), 1);
    }

    /// Toggle merging across kinds; a kind with both toggles off is inert
    #[test]
    fn verdict_merges_across_rule_kinds() {
        let merged = rules(|s| {
            s.apps = vec![IgnoredApp {
                id: "com.example.app".into(),
                name: "Example".into(),
            }];
            s.apps_sync = false;
            s.apps_record = true;
            s.regexes = vec!["^token$".into()];
        });
        let both = merged.evaluate(&text("token"), &[], Some("com.example.app"));
        assert!(both.suppress_sync && both.suppress_record);

        let disabled = rules(|s| {
            s.types = vec!["custom.type".into()];
            s.types_sync = false;
            s.types_record = false;
        });
        assert_eq!(
            disabled.evaluate(&text("x"), &["custom.type".into()], None),
            IgnoreVerdict::default(),
            "Both toggles off means the rule kind is inert"
        );
    }

    /// wants_source_app: only with a non-empty list and a live toggle
    #[test]
    fn wants_source_app_reflects_configuration() {
        assert!(!rules(|_| {}).wants_source_app());
        assert!(
            rules(|s| s.apps = vec![IgnoredApp {
                id: "a.b".into(),
                name: "X".into()
            }])
            .wants_source_app()
        );
        assert!(
            !rules(|s| {
                s.apps = vec![IgnoredApp {
                    id: "a.b".into(),
                    name: "X".into(),
                }];
                s.apps_sync = false;
            })
            .wants_source_app()
        );
    }

    /// The glob engine itself: star spans, ?, classes with ranges and
    /// negation, star backtracking, trailing stars
    #[test]
    fn glob_engine_covers_the_fnmatch_subset() {
        assert!(glob_match("*.key", "server.key"));
        assert!(glob_match("*.KEY", "server.key"), "Case-insensitive");
        assert!(!glob_match("*.key", "server.keys"));
        assert!(glob_match("id_rsa*", "id_rsa.pub"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(
            glob_match("*/secrets/*", "/Users/zero/secrets/x"),
            "* crosses /"
        );
        assert!(glob_match("report-[0-9].txt", "report-7.txt"));
        assert!(!glob_match("report-[0-9].txt", "report-x.txt"));
        assert!(glob_match("[!a]bc", "xbc"));
        assert!(!glob_match("[!a]bc", "abc"));
        assert!(glob_match("a*b*c", "a-x-b-y-c"), "Star backtracking");
        assert!(glob_match("abc*", "abc"), "Trailing star matches empty");
        assert!(glob_match("*", "anything"));
        assert!(
            !glob_match("[unclosed", "u"),
            "An unclosed class never matches"
        );
    }
}
