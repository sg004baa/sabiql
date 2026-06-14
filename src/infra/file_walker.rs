use std::path::PathBuf;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use walkdir::WalkDir;

use crate::app::model::connection::file_picker::{
    WalkOptions, is_target_file, resolve_walk_root, should_skip_dir,
};
use crate::app::ports::outbound::FileSystemWalker;
use crate::app::update::action::Action;

/// Number of paths accumulated before flushing a chunk to the picker. Keeps the
/// action channel from being flooded one-message-per-file on large trees.
const CHUNK_SIZE: usize = 64;

/// Filesystem walker backed by the `walkdir` crate.
pub struct WalkdirFileWalker;

impl WalkdirFileWalker {
    pub fn new() -> Self {
        Self
    }
}

impl Default for WalkdirFileWalker {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystemWalker for WalkdirFileWalker {
    fn spawn_walk(
        &self,
        field: String,
        options: WalkOptions,
        generation: u64,
        tx: mpsc::Sender<Action>,
    ) {
        // Filesystem traversal is blocking; run it off the async runtime and
        // hand results back over the channel.
        tokio::task::spawn_blocking(move || {
            let root = resolve_walk_root(&field, dirs::home_dir().as_deref());
            let truncated = walk_and_stream(&root, &options, generation, &tx);
            tx.blocking_send(Action::FilePickerWalkDone {
                generation,
                truncated,
            })
            .ok();
        });
    }
}

/// Walk `root`, streaming matching paths in chunks. Returns whether the scan was
/// cut short by the result-count or time bound.
fn walk_and_stream(
    root: &std::path::Path,
    options: &WalkOptions,
    generation: u64,
    tx: &mpsc::Sender<Action>,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(options.timeout_secs);
    let mut batch: Vec<PathBuf> = Vec::with_capacity(CHUNK_SIZE);
    let mut total = 0usize;

    let walker = WalkDir::new(root)
        .max_depth(options.max_depth)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            // Prune denylisted directories (don't descend). Files always pass
            // this stage; extension filtering happens below.
            if e.file_type().is_dir() {
                e.file_name()
                    .to_str()
                    .is_none_or(|name| !should_skip_dir(name, options.denylist))
            } else {
                true
            }
        });

    for entry in walker.filter_map(Result::ok) {
        if Instant::now() >= deadline {
            flush(&mut batch, generation, tx);
            return true;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if !is_target_file(entry.path(), options.extensions) {
            continue;
        }

        batch.push(entry.path().to_path_buf());
        total += 1;

        if total >= options.max_results {
            flush(&mut batch, generation, tx);
            return true;
        }
        if batch.len() >= CHUNK_SIZE {
            // Receiver gone (picker closed) → stop early, not truncated.
            if !flush(&mut batch, generation, tx) {
                return false;
            }
        }
    }

    flush(&mut batch, generation, tx);
    false
}

/// Send the accumulated batch as one chunk. Returns false if the receiver is
/// gone (the caller should stop walking).
fn flush(batch: &mut Vec<PathBuf>, generation: u64, tx: &mpsc::Sender<Action>) -> bool {
    if batch.is_empty() {
        return true;
    }
    let paths = std::mem::take(batch);
    tx.blocking_send(Action::FilePickerChunk { generation, paths })
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn collect_paths(actions: &[Action]) -> Vec<PathBuf> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::FilePickerChunk { paths, .. } => Some(paths.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    fn done_truncated(actions: &[Action]) -> Option<bool> {
        actions.iter().find_map(|a| match a {
            Action::FilePickerWalkDone { truncated, .. } => Some(*truncated),
            _ => None,
        })
    }

    /// Drive the walker synchronously and gather all emitted actions.
    async fn run_walk(root: &std::path::Path, options: WalkOptions) -> Vec<Action> {
        let (tx, mut rx) = mpsc::channel(256);
        let walker = WalkdirFileWalker::new();
        // Pass the root directly as the "field" — resolve_walk_root returns it
        // unchanged because it exists as a directory.
        walker.spawn_walk(root.to_string_lossy().into_owned(), options, 7, tx);
        let mut actions = Vec::new();
        while let Some(a) = rx.recv().await {
            actions.push(a);
        }
        actions
    }

    #[tokio::test]
    async fn finds_database_files_and_skips_others() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("a.db"), b"").unwrap();
        fs::write(root.join("b.sqlite"), b"").unwrap();
        fs::write(root.join("notes.txt"), b"").unwrap();

        let actions = run_walk(root, WalkOptions::default()).await;
        let paths = collect_paths(&actions);

        let names: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"a.db".to_string()));
        assert!(names.contains(&"b.sqlite".to_string()));
        assert!(!names.contains(&"notes.txt".to_string()));
        assert_eq!(done_truncated(&actions), Some(false));
    }

    #[tokio::test]
    async fn denylisted_directories_are_not_descended() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("top.db"), b"").unwrap();
        let noisy = root.join("node_modules");
        fs::create_dir(&noisy).unwrap();
        fs::write(noisy.join("hidden.db"), b"").unwrap();
        let cache = root.join(".cache");
        fs::create_dir(&cache).unwrap();
        fs::write(cache.join("app.sqlite"), b"").unwrap();

        let actions = run_walk(root, WalkOptions::default()).await;
        let paths = collect_paths(&actions);

        let names: Vec<String> = paths
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"top.db".to_string()));
        assert!(
            !names.contains(&"hidden.db".to_string()),
            "node_modules pruned"
        );
        assert!(!names.contains(&"app.sqlite".to_string()), ".cache pruned");
    }

    #[tokio::test]
    async fn dotdir_not_on_denylist_is_walked() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let share = root.join(".local").join("share").join("app");
        fs::create_dir_all(&share).unwrap();
        fs::write(share.join("data.db"), b"").unwrap();

        let actions = run_walk(root, WalkOptions::default()).await;
        let paths = collect_paths(&actions);

        assert!(
            paths.iter().any(|p| p.ends_with("data.db")),
            "~/.local/share-style paths must be reachable"
        );
    }

    #[tokio::test]
    async fn depth_limit_excludes_files_below_it() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let deep = root.join("a").join("b").join("c");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("deep.db"), b"").unwrap();

        let opts = WalkOptions {
            max_depth: 2,
            ..WalkOptions::default()
        };
        let actions = run_walk(root, opts).await;
        let paths = collect_paths(&actions);

        assert!(!paths.iter().any(|p| p.ends_with("deep.db")));
    }

    #[tokio::test]
    async fn max_results_truncates() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for i in 0..10 {
            fs::write(root.join(format!("f{i}.db")), b"").unwrap();
        }

        let opts = WalkOptions {
            max_results: 3,
            ..WalkOptions::default()
        };
        let actions = run_walk(root, opts).await;
        let paths = collect_paths(&actions);

        assert_eq!(paths.len(), 3);
        assert_eq!(done_truncated(&actions), Some(true));
    }
}
