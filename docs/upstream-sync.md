# Upstream sync: v3.0.1

The fork incorporates `riii111/sabiql` through `52781c60e2f3d5a73ca0c7a2ba57559ace7f983e` (2026-10-03), including the PostgreSQL, MySQL and SQLite engine changes, inspection UI, write safety, terminal lifecycle fixes, and updated dependencies. The full upstream commit is recorded in the merge ancestry.

## Retained fork behavior

- The `crates/` workspace and `sabiql-tui-kit` remain shared by `sabiql` and `sabiql-redis`; both binaries remain in the release configuration.
- Redis connection, browsing, value editing, read-only mode, and ElastiCache support remain available.
- SQLite connection setup opens the streaming file picker with `Ctrl+P` on the file path field. Results from closed or superseded scans are ignored.
- `Ctrl+E` opens `$EDITOR` from SQL editing and JSON editing. The terminal resumes on success, editor failure, or worker panic. Edited SQL is loaded without executing it; invalid JSON is rejected.
- `Space` marks result rows and `S` opens the SELECT / INSERT / UPDATE / DELETE generation menu. Generated SQL is loaded into the SQL editor for review. UPDATE, DELETE and SELECT use the original stable row identities, including hidden MySQL primary keys; absent identities and stale results are rejected.
- Primary-key cells can be edited through the SQL preview and confirmation flow. A primary-key update targets the original key and carries a Medium risk explanation.
- `Tab` / `Shift+Tab` and `>` / `<` cycle panes; `/` opens the table picker and `Ctrl+K` opens settings. Inspector tabs use `[` / `]` to keep pane cycling available.
- Result columns retain the 50-cell width cap. Newlines, tabs and other control characters render as visible escapes, and marked rows retain their highlight.
- The fork's Homebrew/release configuration and self-update repository remain `sg004baa/sabiql`.

The remaining engine behavior, feature policies and keymap presets follow upstream v3.0.1.
