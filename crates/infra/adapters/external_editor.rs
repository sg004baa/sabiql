use std::process::Command;

use crate::app::ports::outbound::external_editor::{ExternalEditor, ExternalEditorError};

pub struct SystemExternalEditor;

impl ExternalEditor for SystemExternalEditor {
    fn edit(&self, content: &str, extension: &str) -> Result<String, ExternalEditorError> {
        let editor = std::env::var_os("EDITOR").ok_or(ExternalEditorError::NotConfigured)?;
        let editor = editor.to_string_lossy();
        // `EDITOR="code -w"`: first token is the program, the rest are leading arguments.
        // An empty or whitespace-only value yields no program at all.
        let mut tokens = editor.split_ascii_whitespace();
        let program = tokens.next().ok_or(ExternalEditorError::NotConfigured)?;

        // A directory, not a `NamedTempFile`: editors such as vim replace the file by
        // rename, which would orphan a held file handle. `TempDir` removes it all on drop.
        let dir = tempfile::Builder::new().prefix("sabiql-").tempdir()?;
        let path = dir.path().join(format!("buffer.{extension}"));
        // A text file's final newline is a terminator, not buffer content: editors such as
        // vim add one when it is missing. Writing it here and stripping it on the way back
        // makes the round-trip an identity instead of growing a blank line per invocation.
        std::fs::write(&path, format!("{content}\n"))?;

        // Stdio is inherited (the default for `status()`), so the editor owns the terminal.
        let status = Command::new(program).args(tokens).arg(&path).status()?;
        if !status.success() {
            return Err(ExternalEditorError::EditorFailed(status.to_string()));
        }

        let mut edited = std::fs::read_to_string(&path)?;
        if edited.ends_with('\n') {
            edited.truncate(edited.len() - 1);
            if edited.ends_with('\r') {
                edited.truncate(edited.len() - 1);
            }
        }
        Ok(edited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::sync::Mutex;

    /// Guards env-var–mutating tests so they don't race each other.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Sets `$EDITOR` for the duration of a test and restores the previous value on drop.
    struct EditorEnv(Option<OsString>);

    impl EditorEnv {
        fn set(value: Option<&str>) -> Self {
            let previous = std::env::var_os("EDITOR");
            write_editor(value.map(OsString::from));
            Self(previous)
        }
    }

    impl Drop for EditorEnv {
        fn drop(&mut self) {
            write_editor(self.0.take());
        }
    }

    fn write_editor(value: Option<OsString>) {
        // SAFETY: test-only, serialized by ENV_LOCK
        unsafe {
            match value {
                Some(v) => std::env::set_var("EDITOR", v),
                None => std::env::remove_var("EDITOR"),
            }
        }
    }

    #[test]
    fn missing_editor_env_is_not_configured() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = EditorEnv::set(None);

        let error = SystemExternalEditor.edit("SELECT 1", "sql").unwrap_err();
        assert!(matches!(error, ExternalEditorError::NotConfigured));
    }

    #[test]
    fn no_op_editor_preserves_buffer_contents() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = EditorEnv::set(Some("true"));

        let content = "SELECT 1;\n-- unchanged\n";
        let edited = SystemExternalEditor.edit(content, "sql").unwrap();
        assert_eq!(edited, content);
    }

    /// Writes an executable script and returns it plus the dir keeping it alive.
    fn editor_script(body: &str) -> (tempfile::TempDir, String) {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("editor.sh");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let command = path.to_str().unwrap().to_string();
        (dir, command)
    }

    #[test]
    fn editor_terminating_the_file_does_not_grow_the_buffer() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // vim and friends append a final newline to a file that lacks one, and leave a
        // properly terminated file alone.
        let (_dir, command) =
            editor_script(r#"if [ -n "$(tail -c1 "$1")" ]; then printf '\n' >> "$1"; fi"#);
        let _env = EditorEnv::set(Some(&command));

        let edited = SystemExternalEditor.edit("SELECT 1;", "sql").unwrap();
        assert_eq!(edited, "SELECT 1;");
    }

    #[test]
    fn trailing_blank_line_written_by_the_editor_survives() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The user added one empty line, so the file gains a newline beyond the terminator.
        let (_dir, command) = editor_script(r#"printf 'SELECT 1;\n\n' > "$1""#);
        let _env = EditorEnv::set(Some(&command));

        let edited = SystemExternalEditor.edit("SELECT 1;", "sql").unwrap();
        assert_eq!(edited, "SELECT 1;\n");
    }

    #[test]
    fn failing_editor_discards_edits() {
        let _guard = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _env = EditorEnv::set(Some("false"));

        let error = SystemExternalEditor.edit("{}", "json").unwrap_err();
        assert!(matches!(error, ExternalEditorError::EditorFailed(_)));
    }
}
