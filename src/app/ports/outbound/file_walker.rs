use tokio::sync::mpsc;

use crate::model::connection::file_picker::WalkOptions;
use crate::update::action::Action;

/// Recursively scans the filesystem for SQLite database files on behalf of the
/// file picker.
///
/// Unlike most outbound ports (which return domain values), this one streams
/// results directly as `Action`s: the walk produces an unbounded sequence of
/// chunks over time, and the picker is the sole consumer, so feeding the
/// reducer channel directly avoids an extra translation layer. Implementations
/// spawn their own task and return immediately.
pub trait FileSystemWalker: Send + Sync {
    /// Resolve the walk root from `field` (and the environment), then stream
    /// `Action::FilePickerChunk` / `Action::FilePickerWalkDone` tagged with
    /// `generation`. Must not block the caller.
    fn spawn_walk(
        &self,
        field: String,
        options: WalkOptions,
        generation: u64,
        tx: mpsc::Sender<Action>,
    );
}
