use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use color_eyre::eyre::Result;
use tokio::sync::mpsc;
use tokio::time::sleep_until;

mod error;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "tests/render_snapshots/mod.rs"]
mod render_snapshots;

use sabiql_app::cmd::cache::TtlCache;
use sabiql_app::cmd::completion_engine::CompletionEngine;
use sabiql_app::cmd::effect::Effect;
use sabiql_app::cmd::render_schedule::next_animation_deadline;
use sabiql_app::cmd::runner::EffectRunner;
use sabiql_app::model::app_state::AppState;
use sabiql_app::model::shared::db_capabilities::DbCapabilities;
use sabiql_app::model::shared::input_mode::InputMode;
use sabiql_app::ports::outbound::{
    ConnectionStore, ConnectionStoreError, DatabaseCapabilityProvider, PgServiceEntryReader,
    ServiceFileError, SettingsStore,
};
use sabiql_app::services::AppServices;
use sabiql_app::update::action::Action;
use sabiql_app::update::input::handle_event;
use sabiql_app::update::reducer::reduce;
use sabiql_infra::adapters::{
    ArboardClipboard, DispatchAdapter, FileConfigWriter, FileQueryHistoryStore, FsErLogWriter,
    NativeFolderOpener, PgServiceFileReader, SystemExternalEditor, TomlConnectionStore,
    TomlSettingsStore,
};
use sabiql_infra::config::project_root::{find_project_root, get_project_name};
use sabiql_infra::export::DotExporter;
use sabiql_infra::file_walker::WalkdirFileWalker;
use sabiql_ui::adapters::TuiAdapter;
use sabiql_ui::tui::TuiRunner;

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    #[cfg(feature = "self-update")]
    /// Update sabiql to the latest compatible version
    Update,
    #[cfg(not(feature = "self-update"))]
    /// Self-update is disabled in this build
    #[command(hide = true)]
    Update,
}

#[allow(
    clippy::print_stderr,
    reason = "CLI error output before TUI initialization"
)]
fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    error::install_hooks()?;

    let args = Args::parse();
    if matches!(args.command, Some(Command::Update)) {
        // Runs before the tokio runtime starts: self_update uses a blocking HTTP
        // client that must not be dropped inside an async context.
        #[cfg(feature = "self-update")]
        {
            return run_update();
        }
        #[cfg(not(feature = "self-update"))]
        {
            eprintln!("{}", self_update_disabled_message());
            std::process::exit(1);
        }
    }

    run_tui()
}

#[tokio::main]
#[allow(
    clippy::print_stderr,
    reason = "CLI error output before TUI initialization"
)]
async fn run_tui() -> Result<()> {
    let project_root = find_project_root()?;
    let project_name = get_project_name(&project_root);

    let (action_tx, mut action_rx) = mpsc::channel::<Action>(256);

    let adapter = Arc::new(DispatchAdapter::new());
    let metadata_cache = TtlCache::new(300);
    let completion_engine = RefCell::new(CompletionEngine::new());
    let connection_store = TomlConnectionStore::new()?;
    let settings_store = TomlSettingsStore::new()?;
    let all_profiles = connection_store.load_all();
    let connection_store = Arc::new(connection_store);
    let settings_store = Arc::new(settings_store);

    let db_capabilities: DbCapabilities = adapter.capabilities().into();
    let pg_service_entry_reader: Arc<dyn PgServiceEntryReader> =
        Arc::new(PgServiceFileReader::new());

    let adapter_for_callback = Arc::clone(&adapter);
    let effect_runner = EffectRunner::builder()
        .metadata_provider(Arc::clone(&adapter) as _)
        .query_executor(Arc::clone(&adapter) as _)
        .dsn_builder(Arc::clone(&adapter) as _)
        .er_exporter(Arc::new(DotExporter::new()))
        .config_writer(Arc::new(FileConfigWriter::new()))
        .er_log_writer(Arc::new(FsErLogWriter))
        .connection_store(Arc::clone(&connection_store) as _)
        .pg_service_entry_reader(Arc::clone(&pg_service_entry_reader))
        .clipboard(Arc::new(ArboardClipboard))
        .folder_opener(Arc::new(NativeFolderOpener))
        .external_editor(Arc::new(SystemExternalEditor))
        .file_system_walker(Arc::new(WalkdirFileWalker::new()))
        .query_history_store(Arc::new(FileQueryHistoryStore::new()))
        .settings_store(Arc::clone(&settings_store) as _)
        .metadata_cache(metadata_cache.clone())
        .action_tx(action_tx.clone())
        .on_database_type_change(Box::new(move |db_type| {
            adapter_for_callback.set_active_type(db_type);
        }))
        .build();

    let services = AppServices {
        ddl_generator: Arc::clone(&adapter) as _,
        sql_dialect: Arc::clone(&adapter) as _,
        db_capabilities,
    };

    let mut state = AppState::new(project_name);

    match all_profiles {
        Ok(profiles) if profiles.is_empty() => {
            load_service_entries(&mut state, Some(&*pg_service_entry_reader));
            if state.service_entries().is_empty() {
                state.connection_setup.is_first_run = true;
                state.modal.set_mode(InputMode::ConnectionSetup);
            } else {
                state.modal.set_mode(InputMode::ConnectionSelector);
                state.ui.set_connection_list_selection(Some(0));
            }
        }
        Ok(mut profiles) => {
            profiles.sort_by(|a, b| {
                a.display_name()
                    .to_lowercase()
                    .cmp(&b.display_name().to_lowercase())
            });
            state.set_connections(profiles);
            load_service_entries(&mut state, Some(&*pg_service_entry_reader));

            state.modal.set_mode(InputMode::ConnectionSelector);
            state.ui.set_connection_list_selection(Some(0));
        }
        Err(ConnectionStoreError::VersionMismatch { found, expected }) => {
            eprintln!(
                "Error: Configuration file version mismatch (found v{}, expected v{}).\n\
                 Please delete {} and reconfigure.",
                found,
                expected,
                connection_store.storage_path().display()
            );
            std::process::exit(1);
        }
        Err(err) => {
            eprintln!(
                "Error: Failed to load connection profiles from {}: {err}\n\
                 The file may be corrupted. Back it up, then fix or remove it and restart.",
                connection_store.storage_path().display()
            );
            std::process::exit(1);
        }
    }

    let app_settings = match settings_store.load() {
        Ok(settings) => settings,
        Err(err) => {
            eprintln!(
                "Error: Failed to load settings from {}: {err}\n\
                 The file may be corrupted. Back it up, then fix or remove it and restart.",
                connection_store.storage_path().display()
            );
            std::process::exit(1);
        }
    };
    state.ui.set_theme(app_settings.theme_id);

    let mut tui = TuiRunner::new()?;
    tui.enter()?;

    let initial_size = tui.terminal().size()?;
    state.ui.terminal_width = initial_size.width;
    state.ui.terminal_height = initial_size.height;

    if state.session.dsn.is_some() && state.input_mode() == InputMode::Normal {
        process_action(
            Action::TryConnect,
            &mut state,
            &mut tui,
            &effect_runner,
            &completion_engine,
            &services,
        )
        .await?;
    }

    let cache_cleanup_interval = Duration::from_secs(150);
    let mut last_cache_cleanup = Instant::now();

    loop {
        let now = Instant::now();
        let deadline = next_animation_deadline(&state, now);

        tokio::select! {
            Some(event) = tui.next_event() => {
                let action = handle_event(event, &state, &services);
                if !action.is_none() {
                    drain_and_process_terminal_events(action, &mut state, &mut tui, &effect_runner, &completion_engine, &services).await?;
                }
            }
            Some(action) = action_rx.recv() => {
                process_action(action, &mut state, &mut tui, &effect_runner, &completion_engine, &services).await?;
            }
            // Animation deadline reached (spinner, cursor blink, message timeout)
            () = async {
                match deadline {
                    Some(d) => sleep_until(d.into()).await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                process_action(Action::Render, &mut state, &mut tui, &effect_runner, &completion_engine, &services).await?;
            }
        }

        if let Some(debounce_until) = state.sql_modal.completion_debounce()
            && Instant::now() >= debounce_until
        {
            state.sql_modal.consume_completion_debounce();
            process_action(
                Action::CompletionTrigger,
                &mut state,
                &mut tui,
                &effect_runner,
                &completion_engine,
                &services,
            )
            .await?;
        }

        if last_cache_cleanup.elapsed() >= cache_cleanup_interval {
            metadata_cache.cleanup_expired().await;
            last_cache_cleanup = Instant::now();
        }

        if state.should_quit {
            break;
        }
    }

    tui.exit()?;
    Ok(())
}

async fn process_action(
    action: Action,
    state: &mut AppState,
    tui: &mut TuiRunner,
    effect_runner: &EffectRunner,
    completion_engine: &RefCell<CompletionEngine>,
    services: &AppServices,
) -> Result<()> {
    let now = Instant::now();
    let is_animation_tick = matches!(action, Action::Render);
    if is_animation_tick {
        state.clear_expired_timers(now);
    }
    let mut effects = reduce(state, action, now, services);
    if state.render_dirty {
        if !is_animation_tick {
            state.clear_expired_timers(now);
        }
        effects.push(Effect::Render);
    }
    flush_effects(
        effects,
        state,
        tui,
        effect_runner,
        completion_engine,
        services,
    )
    .await
}

#[allow(
    clippy::print_stderr,
    reason = "last-resort fallback when effect dispatch exceeds recursion limit"
)]
async fn flush_effects(
    effects: Vec<Effect>,
    state: &mut AppState,
    tui: &mut TuiRunner,
    effect_runner: &EffectRunner,
    completion_engine: &RefCell<CompletionEngine>,
    services: &AppServices,
) -> Result<()> {
    let mut tui_adapter = TuiAdapter::new(tui);
    let mut pending = effect_runner
        .run(
            effects,
            &mut tui_adapter,
            state,
            completion_engine,
            services,
        )
        .await?;
    state.clear_dirty();

    const MAX_DEPTH: usize = 16;
    let mut depth = 0;
    while !pending.is_empty() && depth < MAX_DEPTH {
        depth += 1;
        let mut next = Vec::new();
        for action in pending {
            let now = Instant::now();
            let mut effects = reduce(state, action, now, services);
            if state.render_dirty {
                state.clear_expired_timers(now);
                effects.push(Effect::Render);
            }
            let mut tui_adapter = TuiAdapter::new(tui);
            next.extend(
                effect_runner
                    .run(
                        effects,
                        &mut tui_adapter,
                        state,
                        completion_engine,
                        services,
                    )
                    .await?,
            );
            state.clear_dirty();
        }
        pending = next;
    }
    if depth >= MAX_DEPTH && !pending.is_empty() {
        eprintln!(
            "DispatchActions recursion depth exceeded ({MAX_DEPTH}), \
             falling back to channel for {} remaining actions",
            pending.len()
        );
        for action in pending {
            if let Err(e) = effect_runner.action_tx().try_send(action) {
                eprintln!("DispatchActions fallback: channel full, dropping action: {e}");
            }
        }
    }
    Ok(())
}

const MAX_DRAIN: usize = 32;

async fn drain_and_process_terminal_events(
    first_action: Action,
    state: &mut AppState,
    tui: &mut TuiRunner,
    effect_runner: &EffectRunner,
    completion_engine: &RefCell<CompletionEngine>,
    services: &AppServices,
) -> Result<()> {
    if !first_action.is_scroll() {
        return process_action(
            first_action,
            state,
            tui,
            effect_runner,
            completion_engine,
            services,
        )
        .await;
    }

    let now = Instant::now();
    let mut effects = reduce(state, first_action, now, services);
    if !effects.is_empty() {
        if state.render_dirty {
            state.clear_expired_timers(now);
            effects.push(Effect::Render);
        }
        return flush_effects(
            effects,
            state,
            tui,
            effect_runner,
            completion_engine,
            services,
        )
        .await;
    }

    let mut drained = 0;
    while drained < MAX_DRAIN {
        let Some(event) = tui.try_next_event() else {
            break;
        };
        drained += 1;
        let action = handle_event(event, state, services);
        if action.is_none() {
            continue;
        }

        if action.is_scroll() {
            let now = Instant::now();
            let mut effects = reduce(state, action, now, services);
            if !effects.is_empty() {
                if state.render_dirty {
                    state.clear_expired_timers(now);
                    effects.push(Effect::Render);
                }
                flush_effects(
                    effects,
                    state,
                    tui,
                    effect_runner,
                    completion_engine,
                    services,
                )
                .await?;
                break;
            }
        } else {
            if state.render_dirty {
                state.clear_dirty();
                process_action(
                    Action::Render,
                    state,
                    tui,
                    effect_runner,
                    completion_engine,
                    services,
                )
                .await?;
            }
            process_action(
                action,
                state,
                tui,
                effect_runner,
                completion_engine,
                services,
            )
            .await?;
            if state.should_quit {
                return Ok(());
            }
        }
    }

    if state.render_dirty {
        state.clear_dirty();
        process_action(
            Action::Render,
            state,
            tui,
            effect_runner,
            completion_engine,
            services,
        )
        .await?;
    }

    Ok(())
}

fn load_service_entries(state: &mut AppState, reader: Option<&dyn PgServiceEntryReader>) {
    let Some(reader) = reader else {
        return;
    };

    match reader.read_services() {
        Ok((services, path)) if !services.is_empty() => {
            state.set_service_entries(services);
            state.runtime.service_file_path = Some(path);
        }
        Ok(_) | Err(ServiceFileError::NotFound(_)) => {}
        Err(e) => {
            state.messages.set_error(e.to_string());
        }
    }
}

#[cfg(feature = "self-update")]
#[allow(clippy::print_stdout, reason = "CLI subcommand output, TUI not active")]
fn run_update() -> Result<()> {
    use color_eyre::eyre::eyre;

    const REPO_OWNER: &str = "riii111";
    const REPO_NAME: &str = "sabiql";

    let current = env!("CARGO_PKG_VERSION");
    println!("Current version: v{current}");
    println!("Checking for updates...");

    let releases = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("sabiql")
        .current_version(current)
        .build()?
        .get_latest_releases()?;

    if !releases.is_update_available()? {
        println!("Already up to date (v{current}).");
        return Ok(());
    }

    // Mirror the crate's selection: newest semver-compatible release, else newest.
    let releases = releases.into_vec();
    let release = releases
        .iter()
        .find(|release| {
            self_update::version::bump_is_compatible(current, &release.version).unwrap_or(false)
        })
        .or_else(|| releases.first())
        .ok_or_else(|| eyre!("No newer release found"))?;

    let archive_name = release_archive_name();
    let expected_sha256 = fetch_expected_sha256(release, &archive_name)?;

    // Pin the update to the exact release the checksum was fetched for, so a release
    // published mid-update cannot be installed against the wrong digest.
    let release_tag = format!("v{}", release.version);
    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("sabiql")
        .show_download_progress(true)
        .current_version(current)
        .release_tag(release_tag)
        .asset_matcher(move |assets: &[self_update::ReleaseAsset]| {
            assets
                .iter()
                .find(|asset| asset.name == archive_name)
                .cloned()
        })
        .verify_checksum(self_update::Checksum::Sha256(expected_sha256))
        .build()?
        .update()?;

    if status.is_updated() {
        println!(
            "Updated successfully: v{} -> v{}",
            current,
            status.version()
        );
    } else {
        println!("Already up to date (v{current}).");
    }

    Ok(())
}

/// cargo-dist release archive name for the running platform, e.g.
/// `sabiql-x86_64-unknown-linux-gnu.tar.gz`.
#[cfg(feature = "self-update")]
fn release_archive_name() -> String {
    let target = self_update::get_target();
    let extension = if cfg!(target_os = "windows") {
        "zip"
    } else {
        "tar.gz"
    };
    format!("sabiql-{target}.{extension}")
}

/// Download the cargo-dist `<archive>.sha256` release asset and return the expected
/// hex digest. Fails if the asset is missing or its content is not a SHA-256 digest.
#[cfg(feature = "self-update")]
fn fetch_expected_sha256(
    release: &self_update::update::Release,
    archive_name: &str,
) -> Result<String> {
    use color_eyre::eyre::eyre;

    let checksum_name = format!("{archive_name}.sha256");
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == checksum_name)
        .ok_or_else(|| {
            eyre!(
                "Checksum asset `{checksum_name}` not found in release v{}; refusing to update without verification",
                release.version
            )
        })?;

    let mut raw = Vec::new();
    let mut download = self_update::Download::from_url(&asset.download_url);
    download.header("Accept", "application/octet-stream")?;
    download.download_to(&mut raw)?;

    let content = String::from_utf8(raw)
        .map_err(|_| eyre!("Checksum asset `{checksum_name}` is not valid UTF-8"))?;
    parse_sha256_digest(&content, &checksum_name)
}

/// Parse a cargo-dist checksum asset: `sha256sum`-style content, `<hex digest>  <file name>`.
#[cfg(feature = "self-update")]
fn parse_sha256_digest(content: &str, checksum_name: &str) -> Result<String> {
    use color_eyre::eyre::eyre;

    let digest = content
        .split_whitespace()
        .next()
        .ok_or_else(|| eyre!("Checksum asset `{checksum_name}` is empty"))?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(eyre!(
            "Checksum asset `{checksum_name}` does not contain a SHA-256 digest: `{digest}`"
        ));
    }
    Ok(digest.to_lowercase())
}

#[cfg(not(feature = "self-update"))]
fn self_update_disabled_message() -> String {
    format!(
        "Self-update is not available in this build (v{}).\n\
         If installed via Homebrew: brew upgrade sabiql\n\
         If installed via cargo:    cargo install sabiql",
        env!("CARGO_PKG_VERSION")
    )
}
