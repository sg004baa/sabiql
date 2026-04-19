# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What is sabiql

A fast, driver-less TUI for browsing, querying, and editing PostgreSQL databases. It wraps the `psql` CLI (no Rust database drivers). MySQL support is being added using the same CLI-subprocess pattern with `mysql`.

## Build & Test Commands

```bash
cargo build                            # Dev build
cargo build --release                  # Release build
cargo test                             # Run all unit tests
cargo test --all-features              # Run all tests including render snapshots
cargo test <test_name>                 # Run a single test by name
cargo test --test render_snapshots --features test-support  # Snapshot tests only

cargo clippy --all-targets --all-features -- -D warnings    # Lint (CI runs this)
cargo clippy --all-targets --no-default-features -- -D warnings
cargo fmt --all -- --check             # Format check

./scripts/lint_all.sh                  # Custom lints (test naming conventions)
```

Integration tests against a real PostgreSQL instance are `#[ignore]`-gated and require `SABIQL_TEST_DSN` (default: `postgres://postgres:postgres@localhost:5432/sabiql_test`).

Feature flags: `self-update` (GitHub release updater), `test-support` (enables `AppServices::stub()` and render snapshot tests).

## Architecture

Hexagonal architecture (ports & adapters) with four layers:

```
main.rs            Wiring: creates adapters, injects into EffectRunner
src/domain/        Pure domain models (Table, Column, QueryResult, ConnectionProfile, etc.)
src/app/           Application logic (ports, state, reducers, effects)
src/infra/         Infrastructure adapters (PostgresAdapter, MySqlAdapter, file I/O)
src/ui/            Ratatui-based TUI rendering
```

### Data flow: Action → Reducer → Effect → EffectRunner

1. **Event** (keyboard/terminal) → `ui::event::handlers::handle_event()` → **Action**
2. **Action** → `app::update::reducer::reduce()` (pure, no I/O) → mutates **AppState** + returns **Vec\<Effect\>**
3. **Effect** → `app::cmd::runner::EffectRunner::run()` (async, I/O) → may dispatch new **Actions** back via channel

The reducer is strictly pure: no `Instant::now()`, no I/O, no async. Time is passed as a parameter.

### Port traits (src/app/ports/)

All database operations go through trait-based ports, enabling mock-based testing:

| Trait | Purpose |
|---|---|
| `MetadataProvider` | Fetch schemas, tables, columns, indexes, FKs |
| `QueryExecutor` | Execute preview/adhoc/write queries, CSV export |
| `SqlDialect` | Build dialect-specific UPDATE/DELETE SQL |
| `DdlGenerator` | Generate CREATE TABLE DDL |
| `DsnBuilder` | Build connection strings from `ConnectionProfile` |

### PostgreSQL adapter (src/infra/adapters/postgres/)

Implements all port traits by spawning `psql` as a subprocess:
- **Metadata**: queries `pg_catalog` system tables, returns JSON via `json_agg(row_to_json(...))`
- **Data queries**: `psql --csv` output parsed with the `csv` crate
- **Writes**: parses `psql` command tags (`UPDATE 3`, `DELETE 1`)
- **Read-only mode**: `PGOPTIONS=-c default_transaction_read_only=on`
- **Identifier quoting**: double quotes via `infra::utils::quote_ident()`

### Key state types

- `AppState` (src/app/model/app_state.rs): root application state
- `BrowseSession` (src/app/model/browse/session.rs): connection lifecycle, selected table, metadata
- `ConnectionSetupState` (src/app/model/connection/setup.rs): connection form fields
- `AppServices` (src/app/services.rs): holds `Arc<dyn DdlGenerator>` + `Arc<dyn SqlDialect>` for sync access in reducers

### Testing patterns

- **Reducers**: test directly with `AppState::new()` + `AppServices::stub()` (requires `test-support` feature)
- **Effects**: use `mockall` mocks for port traits (`MockMetadataProvider`, `MockQueryExecutor`, etc.)
- **Render snapshots**: `insta` crate, under `tests/render_snapshots/` (require `test-support` feature)
- **Test naming lint**: CI enforces test name conventions via `scripts/lint_test_names.sh`

## Clippy Configuration

Strict: `clippy::all = deny`, `clippy::pedantic = warn`, `clippy::nursery = warn`. Restriction lints `dbg_macro`, `todo`, `print_stdout`, `print_stderr`, `allow_attributes_without_reason` are denied. See `Cargo.toml [lints.clippy]` for suppressed pedantic/nursery lints.

## Adding a new database backend

1. Create adapter module under `src/infra/adapters/<db>/` mirroring the postgres structure
2. Implement all 5 port traits (`MetadataProvider`, `QueryExecutor`, `SqlDialect`, `DdlGenerator`, `DsnBuilder`)
3. Add `DatabaseType` variant in `src/domain/connection/database_type.rs`
4. Wire into adapter selection in `main.rs`
