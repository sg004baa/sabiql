use std::sync::Arc;

#[derive(Debug, Clone, thiserror::Error)]
pub enum ExternalEditorError {
    #[error("$EDITOR is not set")]
    NotConfigured,
    #[error("editor exited with {0}")]
    EditorFailed(String),
    #[error("{0}")]
    Io(#[source] Arc<std::io::Error>),
}

impl From<std::io::Error> for ExternalEditorError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(Arc::new(e))
    }
}

pub trait ExternalEditor: Send + Sync {
    /// Blocking. Writes `content` to a temp file named `*.{extension}`, runs
    /// `$EDITOR` on it with the terminal inherited, and returns the edited text.
    fn edit(&self, content: &str, extension: &str) -> Result<String, ExternalEditorError>;
}
