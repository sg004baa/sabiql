use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use sabiql_app::model::connection::setup::ConnectionSetupState;
use sabiql_app::model::shared::db_capabilities::DbCapabilities;
use sabiql_app::model::shared::input_mode::InputMode;
use sabiql_app::ports::outbound::{
    ConnectionStore, ConnectionStoreError, DatabaseCapabilityProvider, DsnBuilder,
    PgServiceEntryReader, SettingsStore,
};
use sabiql_app::services::AppServices;
use sabiql_app::update::action::Action;
use sabiql_app::update::input::handle_event;
use sabiql_app::update::reducer::reduce;
use sabiql_infra::adapters::{
    ArboardClipboard, DispatchAdapter, FileConfigWriter, FileQueryHistoryStore, FsErLogWriter,
    NativeFolderOpener, PgServiceFileReader, TomlConnectionStore, TomlSettingsStore,
};
use sabiql_infra::config::project_root::{find_project_root, get_project_name};
use sabiql_infra::export::DotExporter;
use sabiql_infra::file_walker::WalkdirFileWalker;
use sabiql_ui::adapters::TuiAdapter;
use sabiql_ui::tui::TuiRunner;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Quit,
    SwitchConnection,
}

#[allow(
    clippy::print_stderr,
    reason = "CLI error output before TUI initialization"
)]
pub async fn run(
    preselected: Option<sabiql_app::domain::connection::ConnectionProfile>,
) -> Result<RunOutcome> {
    dotenvy::dotenv().ok();
    error::install_hooks()?;

    let project_root = find_project_root()?;
    let project_name = get_project_name(&project_root);

    let (action_tx, mut action_rx) = mpsc::channel::<Action>(256);

    let adapter = Arc::new(DispatchAdapter::new());
    let metadata_cache = TtlCache::new(300);
    let completion_engine = RefCell::new(CompletionEngine::new());
    let connection_store = TomlConnectionStore::new()?;
    let settings_store = TomlSettingsStore::new()?;
    let app_settings = settings_store.load().unwrap_or_default();
    let mut initial_profile = preselected;
    let mut initial_service = None;
    let mut service_file_path = None;
    let mut force_setup = false;
    if initial_profile.is_none() {
        match connection_store.load_all() {
            Ok(profiles) => {
                let mut profiles = rdb_profiles(profiles);
                profiles.sort_by(|left, right| {
                    left.display_name()
                        .to_lowercase()
                        .cmp(&right.display_name().to_lowercase())
                });
                initial_profile = profiles.into_iter().next();
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
            Err(_) => {
                force_setup = true;
            }
        }
    }
    let connection_store = Arc::new(connection_store);
    let settings_store = Arc::new(settings_store);

    if let Some(profile) = initial_profile.as_ref() {
        color_eyre::eyre::ensure!(
            sabiql_app::domain::connection::DatabaseType::RDB.contains(&profile.database_type),
            "Redis is dispatched to sabiql-redis, not handled in RDB path"
        );
        adapter.set_active_type(profile.database_type);
    }

    let db_capabilities: DbCapabilities = adapter.capabilities().into();
    let pg_service_entry_reader: Arc<dyn PgServiceEntryReader> =
        Arc::new(PgServiceFileReader::new());
    if initial_profile.is_none() && !force_setup {
        match pg_service_entry_reader.read_services() {
            Ok((services, path)) => {
                initial_service = services.into_iter().next();
                service_file_path = Some(path);
            }
            Err(_) => {}
        }
    }

    let adapter_for_callback = Arc::clone(&adapter);
    let effect_runner = EffectRunner::builder()
        .metadata_provider(Arc::clone(&adapter) as _)
        .query_executor(Arc::clone(&adapter) as _)
        .dsn_builder(Arc::clone(&adapter) as _)
        .er_exporter(Arc::new(DotExporter::new()))
        .config_writer(Arc::new(FileConfigWriter::new()))
        .er_log_writer(Arc::new(FsErLogWriter))
        .connection_store(Arc::clone(&connection_store) as _)
        .clipboard(Arc::new(ArboardClipboard))
        .folder_opener(Arc::new(NativeFolderOpener))
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
    state.ui.set_theme(app_settings.theme_id);

    if let Some(profile) = initial_profile {
        let dsn = adapter.build_dsn(&profile);
        state.session.active_connection_id = Some(profile.id.clone());
        state.session.active_connection_name = Some(profile.display_name().to_string());
        state.session.dsn = Some(dsn);
        state.connection_setup = ConnectionSetupState::from(&profile);
        state.modal.set_mode(InputMode::Normal);
    } else if let Some(service) = initial_service {
        state.session.active_connection_id = Some(service.connection_id());
        state.session.active_connection_name = Some(service.display_name().to_string());
        state.session.dsn = Some(service.to_string());
        state.runtime.service_file_path = service_file_path;
        state.modal.set_mode(InputMode::Normal);
    } else {
        state.connection_setup.is_first_run = true;
        state.modal.set_mode(InputMode::ConnectionSetup);
    }

    let mut tui = TuiRunner::new()?;
    tui.enter()?;

    let run_result: Result<RunOutcome> = async {
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

        Ok(if state.should_switch_connection {
            RunOutcome::SwitchConnection
        } else {
            RunOutcome::Quit
        })
    }
    .await;

    let exit_result = tui.exit();
    let outcome = run_result?;
    exit_result?;
    Ok(outcome)
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

fn rdb_profiles(
    profiles: Vec<sabiql_app::domain::connection::ConnectionProfile>,
) -> Vec<sabiql_app::domain::connection::ConnectionProfile> {
    profiles
        .into_iter()
        .filter(|profile| {
            sabiql_app::domain::connection::DatabaseType::RDB.contains(&profile.database_type)
        })
        .collect()
}

#[cfg(test)]
mod run_tests {
    use super::*;
    use sabiql_app::domain::connection::{ConnectionProfile, DatabaseType, SslMode};

    fn profile(database_type: DatabaseType) -> ConnectionProfile {
        ConnectionProfile::new(
            database_type.to_string(),
            "localhost",
            database_type.default_port(),
            "0",
            "",
            "",
            SslMode::Prefer,
            database_type,
        )
        .unwrap()
    }

    #[test]
    fn rdb_profile_filter_excludes_redis() {
        let profiles = rdb_profiles(vec![
            profile(DatabaseType::PostgreSQL),
            profile(DatabaseType::Redis),
        ]);

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].database_type, DatabaseType::PostgreSQL);
    }
}
