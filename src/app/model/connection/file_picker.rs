use std::path::{Path, PathBuf};

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};

use crate::model::shared::picker::PickerState;
use crate::update::action::CursorMove;

/// A path that survived fuzzy filtering, with the matched character indices
/// (into its display string) for highlight rendering.
pub struct FilteredPath<'a> {
    pub path: &'a Path,
    pub display: String,
    pub match_indices: Vec<u32>,
}

/// State for the SQLite file picker.
///
/// Walk results stream in via [`append_paths`](Self::append_paths) and
/// accumulate in `all_paths`; filtering re-runs nucleo over that cache on every
/// keystroke without re-walking. `walk_started` enforces the lazy trigger (walk
/// begins only on the first filter character), and `generation` discards stale
/// async chunks after the picker is reopened.
#[derive(Debug, Clone, Default)]
pub struct FilePickerState {
    picker: PickerState,
    all_paths: Vec<PathBuf>,
    walk_started: bool,
    generation: u64,
    scanning: bool,
    truncated: bool,
}

impl FilePickerState {
    pub fn picker(&self) -> &PickerState {
        &self.picker
    }

    pub fn picker_mut(&mut self) -> &mut PickerState {
        &mut self.picker
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn walk_started(&self) -> bool {
        self.walk_started
    }

    pub fn is_scanning(&self) -> bool {
        self.scanning
    }

    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    pub fn path_count(&self) -> usize {
        self.all_paths.len()
    }

    pub fn filter_text(&self) -> &str {
        self.picker.filter_input().content()
    }

    /// Reset for a fresh open: clear results/filter and bump the generation so
    /// any in-flight walk chunks from a previous session are ignored.
    pub fn open(&mut self) {
        self.all_paths.clear();
        self.picker.clear_filter_and_reset();
        self.walk_started = false;
        self.scanning = false;
        self.truncated = false;
        self.generation = self.generation.wrapping_add(1);
    }

    /// Bump the generation to invalidate in-flight chunks (on close/confirm).
    pub fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.scanning = false;
    }

    /// Mark the walk as started and scanning. Returns the generation that the
    /// walk Effect should be tagged with.
    pub fn begin_walk(&mut self) -> u64 {
        self.walk_started = true;
        self.scanning = true;
        self.generation
    }

    /// Append a chunk of discovered paths (called as walk results stream in).
    pub fn append_paths(&mut self, paths: &[PathBuf]) {
        self.all_paths.extend_from_slice(paths);
    }

    /// Mark the walk finished; `truncated` is true if a bound (count/time/depth)
    /// cut the scan short.
    pub fn finish_walk(&mut self, truncated: bool) {
        self.scanning = false;
        self.truncated = truncated;
    }

    pub fn insert_filter_char(&mut self, ch: char) {
        self.picker.insert_filter_char(ch);
    }

    pub fn insert_filter_str(&mut self, text: &str) {
        self.picker.insert_filter_str(text);
    }

    pub fn backspace_filter(&mut self) {
        self.picker.backspace_filter();
    }

    pub fn move_filter_cursor(&mut self, direction: CursorMove) {
        self.picker.move_filter_cursor(direction);
    }

    pub fn select_next(&mut self) {
        let count = self.filtered_count();
        if count > 0 {
            let next = (self.clamped_selected() + 1).min(count - 1);
            self.picker.set_selection(next);
        }
    }

    pub fn select_previous(&mut self) {
        let prev = self.clamped_selected().saturating_sub(1);
        self.picker.set_selection(prev);
    }

    pub fn clamped_selected(&self) -> usize {
        let count = self.filtered_count();
        if count == 0 {
            0
        } else {
            self.picker.selected().min(count - 1)
        }
    }

    pub fn filtered_count(&self) -> usize {
        self.filtered_paths().len()
    }

    /// The path currently highlighted, if any.
    pub fn selected_path(&self) -> Option<PathBuf> {
        let filtered = self.filtered_paths();
        filtered
            .get(self.clamped_selected())
            .map(|f| f.path.to_path_buf())
    }

    /// Paths matching the current filter, with highlight indices. Empty filter
    /// returns every path unfiltered.
    pub fn filtered_paths(&self) -> Vec<FilteredPath<'_>> {
        let filter = self.picker.filter_input().content();

        if filter.is_empty() {
            return self
                .all_paths
                .iter()
                .map(|p| FilteredPath {
                    path: p.as_path(),
                    display: p.to_string_lossy().into_owned(),
                    match_indices: Vec::new(),
                })
                .collect();
        }

        let mut matcher = Matcher::new(Config::DEFAULT);
        let pattern = Pattern::parse(filter, CaseMatching::Ignore, Normalization::Smart);

        self.all_paths
            .iter()
            .filter_map(|p| {
                let display = p.to_string_lossy().into_owned();
                let mut indices = Vec::new();
                let mut buf = Vec::new();
                let haystack = nucleo_matcher::Utf32Str::new(&display, &mut buf);
                let score = pattern.indices(haystack, &mut matcher, &mut indices);
                score.map(|_| FilteredPath {
                    path: p.as_path(),
                    display,
                    match_indices: indices,
                })
            })
            .collect()
    }
}

/// Directory names never descended into during a file-picker walk.
///
/// A denylist (not a blanket "skip hidden dirs" rule) is used deliberately:
/// users keep databases under `~/.local/share/...`, so dot-directories must
/// still be walked. Only known-huge / known-noisy trees are pruned.
pub const DEFAULT_DENYLIST: &[&str] = &[
    ".git",
    ".cache",
    ".cargo",
    ".rustup",
    ".npm",
    ".mozilla",
    ".thunderbird",
    "node_modules",
    "target",
    "vendor",
    "__pycache__",
    ".venv",
    "venv",
];

/// File extensions accepted as SQLite database candidates.
pub const DEFAULT_EXTENSIONS: &[&str] = &["db", "sqlite", "sqlite3", "db3"];

const DEFAULT_MAX_DEPTH: usize = 8;
const DEFAULT_MAX_RESULTS: usize = 5000;
const DEFAULT_TIMEOUT_SECS: u64 = 3;

/// Bounds and filters for a recursive file-picker walk.
///
/// `denylist` and `extensions` borrow `'static` slices because the defaults are
/// compile-time constants; the walk implementation only needs read access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkOptions {
    pub denylist: &'static [&'static str],
    pub extensions: &'static [&'static str],
    pub max_depth: usize,
    pub max_results: usize,
    pub timeout_secs: u64,
}

impl Default for WalkOptions {
    fn default() -> Self {
        Self {
            denylist: DEFAULT_DENYLIST,
            extensions: DEFAULT_EXTENSIONS,
            max_depth: DEFAULT_MAX_DEPTH,
            max_results: DEFAULT_MAX_RESULTS,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        }
    }
}

/// Whether a directory with this name should be skipped (not descended into).
#[must_use]
pub fn should_skip_dir(name: &str, denylist: &[&str]) -> bool {
    denylist.contains(&name)
}

/// Whether this path is an accepted database file based on its extension.
///
/// The extension match is case-insensitive so `Foo.DB` is accepted.
#[must_use]
pub fn is_target_file(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            extensions
                .iter()
                .any(|allowed| allowed.eq_ignore_ascii_case(ext))
        })
}

/// Resolve the directory to start walking from, given the current `File:`
/// field contents and the home directory.
///
/// Rationale: sabiql is often launched from `$HOME`, where a full recursive
/// walk is the worst case. If the user has already typed part of a path, its
/// parent directory drastically narrows the scan; otherwise fall back to home.
#[must_use]
pub fn resolve_walk_root(database_field: &str, home: Option<&Path>) -> PathBuf {
    let trimmed = database_field.trim();

    let expanded = if trimmed == "~" || trimmed.is_empty() {
        None
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home.map(|h| h.join(rest))
    } else {
        Some(PathBuf::from(trimmed))
    };

    if let Some(path) = expanded {
        // Walk from the deepest existing ancestor of what the user typed.
        let candidate = if path.is_dir() {
            Some(path.clone())
        } else {
            path.parent().map(Path::to_path_buf)
        };
        if let Some(dir) = candidate
            && dir.is_dir()
        {
            return dir;
        }
    }

    home.map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    mod file_picker_state {
        use super::*;

        fn paths(items: &[&str]) -> Vec<PathBuf> {
            items.iter().map(PathBuf::from).collect()
        }

        #[test]
        fn open_clears_results_and_bumps_generation() {
            let mut state = FilePickerState::default();
            state.append_paths(&paths(&["/a.db"]));
            let gen_before = state.generation();

            state.open();

            assert_eq!(state.path_count(), 0);
            assert!(!state.walk_started());
            assert!(!state.is_scanning());
            assert_eq!(state.generation(), gen_before + 1);
        }

        #[test]
        fn begin_walk_sets_started_and_returns_generation() {
            let mut state = FilePickerState::default();
            state.open();
            let g = state.begin_walk();

            assert!(state.walk_started());
            assert!(state.is_scanning());
            assert_eq!(g, state.generation());
        }

        #[test]
        fn append_paths_accumulates() {
            let mut state = FilePickerState::default();
            state.append_paths(&paths(&["/a.db", "/b.db"]));
            state.append_paths(&paths(&["/c.db"]));
            assert_eq!(state.path_count(), 3);
        }

        #[test]
        fn finish_walk_clears_scanning_and_sets_truncated() {
            let mut state = FilePickerState::default();
            state.begin_walk();
            state.finish_walk(true);
            assert!(!state.is_scanning());
            assert!(state.is_truncated());
        }

        #[test]
        fn empty_filter_returns_all_paths() {
            let mut state = FilePickerState::default();
            state.append_paths(&paths(&["/x/a.db", "/y/b.sqlite"]));
            assert_eq!(state.filtered_paths().len(), 2);
        }

        #[test]
        fn fuzzy_filter_matches_substring_case_insensitive() {
            let mut state = FilePickerState::default();
            state.append_paths(&paths(&[
                "/home/me/app.db",
                "/home/me/notes.sqlite",
                "/var/data/APP_cache.db",
            ]));
            state.insert_filter_str("app");

            let filtered = state.filtered_paths();

            assert_eq!(filtered.len(), 2);
            assert!(
                filtered
                    .iter()
                    .all(|f| f.display.to_lowercase().contains("app"))
            );
        }

        #[test]
        fn no_match_returns_empty() {
            let mut state = FilePickerState::default();
            state.append_paths(&paths(&["/a.db"]));
            state.insert_filter_str("zzz_nomatch");
            assert!(state.filtered_paths().is_empty());
        }

        #[test]
        fn selected_path_tracks_filtered_list() {
            let mut state = FilePickerState::default();
            state.picker_mut().pane_height = 10;
            state.append_paths(&paths(&["/a.db", "/b.db", "/c.db"]));

            state.select_next();
            assert_eq!(state.selected_path(), Some(PathBuf::from("/b.db")));
        }

        #[test]
        fn select_next_clamps_to_last() {
            let mut state = FilePickerState::default();
            state.picker_mut().pane_height = 10;
            state.append_paths(&paths(&["/a.db", "/b.db"]));

            state.select_next();
            state.select_next();
            state.select_next();

            assert_eq!(state.clamped_selected(), 1);
        }

        #[test]
        fn invalidate_bumps_generation_and_stops_scanning() {
            let mut state = FilePickerState::default();
            state.begin_walk();
            let g = state.generation();

            state.invalidate();

            assert_eq!(state.generation(), g + 1);
            assert!(!state.is_scanning());
        }

        #[test]
        fn selected_path_none_when_empty() {
            let state = FilePickerState::default();
            assert_eq!(state.selected_path(), None);
        }
    }

    mod walk_options {
        use super::*;

        #[test]
        fn default_bounds_match_expected_values() {
            let opts = WalkOptions::default();
            assert_eq!(opts.max_depth, 8);
            assert_eq!(opts.max_results, 5000);
            assert_eq!(opts.timeout_secs, 3);
        }

        #[test]
        fn default_extensions_cover_sqlite_variants() {
            let opts = WalkOptions::default();
            for ext in ["db", "sqlite", "sqlite3", "db3"] {
                assert!(opts.extensions.contains(&ext), "missing extension: {ext}");
            }
        }
    }

    mod should_skip_dir {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case("node_modules", true)]
        #[case("target", true)]
        #[case(".git", true)]
        #[case(".cache", true)]
        #[case(".cargo", true)]
        #[case(".rustup", true)]
        fn denylisted_dirs_are_skipped(#[case] name: &str, #[case] expected: bool) {
            assert_eq!(should_skip_dir(name, DEFAULT_DENYLIST), expected);
        }

        #[test]
        fn dotdir_not_on_denylist_is_walked() {
            // The core of the user requirement: ~/.local/share must be reachable.
            assert!(!should_skip_dir(".local", DEFAULT_DENYLIST));
            assert!(!should_skip_dir(".config", DEFAULT_DENYLIST));
        }

        #[test]
        fn ordinary_dir_is_walked() {
            assert!(!should_skip_dir("projects", DEFAULT_DENYLIST));
            assert!(!should_skip_dir("src", DEFAULT_DENYLIST));
        }
    }

    mod is_target_file {
        use super::*;
        use rstest::rstest;

        #[rstest]
        #[case("foo.db", true)]
        #[case("foo.sqlite", true)]
        #[case("foo.sqlite3", true)]
        #[case("foo.db3", true)]
        #[case("foo.DB", true)]
        #[case("foo.SQLite", true)]
        #[case("foo.txt", false)]
        #[case("foo", false)]
        #[case("foo.dbx", false)]
        fn extension_allowlist(#[case] name: &str, #[case] expected: bool) {
            assert_eq!(
                is_target_file(Path::new(name), DEFAULT_EXTENSIONS),
                expected
            );
        }
    }

    mod resolve_walk_root {
        use super::*;

        #[test]
        fn empty_field_falls_back_to_home() {
            let home = PathBuf::from("/home/me");
            assert_eq!(resolve_walk_root("", Some(&home)), home);
        }

        #[test]
        fn bare_tilde_falls_back_to_home() {
            let home = PathBuf::from("/home/me");
            assert_eq!(resolve_walk_root("~", Some(&home)), home);
        }

        #[test]
        fn existing_typed_directory_is_used_as_root() {
            let tmp = std::env::temp_dir();
            let root = resolve_walk_root(tmp.to_str().unwrap(), None);
            assert_eq!(root, tmp);
        }

        #[test]
        fn parent_of_typed_file_is_used_when_it_exists() {
            let tmp = std::env::temp_dir();
            let typed = tmp.join("nonexistent_file_xyz.db");
            let root = resolve_walk_root(typed.to_str().unwrap(), None);
            assert_eq!(root, tmp);
        }

        #[test]
        fn tilde_path_expands_against_home() {
            // ~/<tmpdir-relative> won't exist, so it falls back to home,
            // but a tilde pointing at an existing dir resolves to it.
            let home = std::env::temp_dir();
            let root = resolve_walk_root("~/", Some(&home));
            assert_eq!(root, home);
        }

        #[test]
        fn nonexistent_path_with_no_existing_parent_falls_back_to_home() {
            let home = PathBuf::from("/home/me");
            let root = resolve_walk_root("/no/such/deep/path/x.db", Some(&home));
            assert_eq!(root, home);
        }

        #[test]
        fn no_home_and_no_valid_path_falls_back_to_cwd_marker() {
            let root = resolve_walk_root("", None);
            assert_eq!(root, PathBuf::from("."));
        }
    }
}
