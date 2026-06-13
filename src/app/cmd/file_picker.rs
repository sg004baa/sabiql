use std::sync::Arc;

use tokio::sync::mpsc;

use crate::cmd::effect::Effect;
use crate::ports::outbound::FileSystemWalker;
use crate::update::action::Action;

/// Dispatch a `StartFilePickerWalk` effect to the walker, which streams results
/// back as actions.
pub fn run(effect: Effect, action_tx: &mpsc::Sender<Action>, walker: &Arc<dyn FileSystemWalker>) {
    match effect {
        Effect::StartFilePickerWalk {
            field,
            options,
            generation,
        } => {
            walker.spawn_walk(field, options, generation, action_tx.clone());
        }
        _ => unreachable!("file_picker::run called with non-file-picker effect"),
    }
}

/// A walker that finds nothing — used when no real walker is wired (e.g. in
/// tests or non-SQLite-focused builds). Reports an immediate, empty,
/// non-truncated result for the requested generation.
pub struct NoopFileSystemWalker;

impl FileSystemWalker for NoopFileSystemWalker {
    fn spawn_walk(
        &self,
        _field: String,
        _options: crate::model::connection::file_picker::WalkOptions,
        generation: u64,
        tx: mpsc::Sender<Action>,
    ) {
        tokio::spawn(async move {
            tx.send(Action::FilePickerWalkDone {
                generation,
                truncated: false,
            })
            .await
            .ok();
        });
    }
}
