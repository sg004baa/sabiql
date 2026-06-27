use color_eyre::eyre::Result;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use sabiql_app::ports::outbound::ConnectionStore;
use sabiql_domain::connection::{ConnectionProfile, DatabaseType};
use sabiql_infra::adapters::TomlConnectionStore;
use sabiql_tui_kit::input::{InputEvent, Key, KeyCombo};
use sabiql_tui_kit::theme::DEFAULT_THEME;
use sabiql_tui_kit::tui::TuiRunner;

mod redis_profile_form;

const PROFILE_READ_ONLY_DEFAULT: bool = false;

pub async fn run() -> Result<()> {
    let store = TomlConnectionStore::new()?;

    loop {
        let profiles = store.load_all()?;
        match pick_profile(profiles).await? {
            PickerAction::Quit => return Ok(()),
            PickerAction::Launch(profile) => {
                if dispatch(profile).await? == SessionOutcome::Quit {
                    return Ok(());
                }
            }
            PickerAction::NewRedis => {
                redis_profile_form::edit_redis_profile(&store, None).await?;
            }
            PickerAction::EditRedis(profile) => {
                redis_profile_form::edit_redis_profile(&store, Some(profile)).await?;
            }
            PickerAction::RdbSetup => {
                if sabiql::run(None).await? == sabiql::RunOutcome::Quit {
                    return Ok(());
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionOutcome {
    Quit,
    SwitchConnection,
}

async fn dispatch(profile: ConnectionProfile) -> Result<SessionOutcome> {
    match launch_target(profile) {
        LaunchTarget::Rdb(profile) => Ok(match sabiql::run(Some(profile)).await? {
            sabiql::RunOutcome::Quit => SessionOutcome::Quit,
            sabiql::RunOutcome::SwitchConnection => SessionOutcome::SwitchConnection,
        }),
        LaunchTarget::Redis { dsn, read_only } => {
            Ok(match sabiql_redis::run(dsn, read_only).await? {
                sabiql_redis::RunOutcome::Quit => SessionOutcome::Quit,
                sabiql_redis::RunOutcome::SwitchConnection => SessionOutcome::SwitchConnection,
            })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LaunchTarget {
    Rdb(ConnectionProfile),
    Redis { dsn: String, read_only: bool },
}

fn launch_target(profile: ConnectionProfile) -> LaunchTarget {
    match profile.database_type {
        DatabaseType::Redis => LaunchTarget::Redis {
            dsn: redis_dsn(&profile),
            read_only: PROFILE_READ_ONLY_DEFAULT,
        },
        DatabaseType::PostgreSQL | DatabaseType::MySQL | DatabaseType::SQLite => {
            LaunchTarget::Rdb(profile)
        }
    }
}

fn redis_dsn(profile: &ConnectionProfile) -> String {
    let database = if profile.database.trim().is_empty() {
        "0"
    } else {
        profile.database.trim()
    };
    let username = profile.username.trim();
    let authentication = match (username.is_empty(), profile.password.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!(":{}@", urlencoding::encode(&profile.password)),
        (false, true) => format!("{}@", urlencoding::encode(username)),
        (false, false) => format!(
            "{}:{}@",
            urlencoding::encode(username),
            urlencoding::encode(&profile.password)
        ),
    };

    // TODO(issue #46 follow-up): Add rediss:// after RedisCliSubprocess supports TLS.
    format!(
        "redis://{authentication}{}:{}/{}",
        profile.host.trim(),
        profile.port,
        database
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PickerAction {
    Quit,
    Launch(ConnectionProfile),
    NewRedis,
    EditRedis(ConnectionProfile),
    RdbSetup,
}

async fn pick_profile(profiles: Vec<ConnectionProfile>) -> Result<PickerAction> {
    let mut picker = ProfilePicker::new(profiles);
    let mut tui = TuiRunner::new()?;
    tui.enter()?;

    let selection_result: Result<PickerAction> = async {
        loop {
            tui.terminal().draw(|frame| render_picker(frame, &picker))?;

            let Some(event) = tui.next_event().await else {
                return Ok(PickerAction::Quit);
            };

            match event {
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Down)
                        || combo == KeyCombo::plain(Key::Char('j')) =>
                {
                    picker.select_next();
                }
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Up)
                        || combo == KeyCombo::plain(Key::Char('k')) =>
                {
                    picker.select_previous();
                }
                InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Enter) => {
                    if let Some(profile) = picker.selected_profile() {
                        return Ok(PickerAction::Launch(profile.clone()));
                    }
                }
                InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('n')) => {
                    return Ok(PickerAction::NewRedis);
                }
                InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Char('e')) => {
                    if let Some(profile) = picker
                        .selected_profile()
                        .filter(|profile| profile.database_type == DatabaseType::Redis)
                    {
                        return Ok(PickerAction::EditRedis(profile.clone()));
                    }
                }
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Char('r')) && !picker.has_rdb_profile() =>
                {
                    return Ok(PickerAction::RdbSetup);
                }
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Esc)
                        || combo == KeyCombo::plain(Key::Char('q'))
                        || combo == KeyCombo::ctrl(Key::Char('c')) =>
                {
                    return Ok(PickerAction::Quit);
                }
                InputEvent::Init
                | InputEvent::Resize(_, _)
                | InputEvent::Paste(_)
                | InputEvent::Key(_) => {}
            }
        }
    }
    .await;

    let exit_result = tui.exit();
    let action = selection_result?;
    exit_result?;
    Ok(action)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickerRow {
    Header(DatabaseType),
    Profile(usize),
}

struct ProfilePicker {
    profiles: Vec<ConnectionProfile>,
    rows: Vec<PickerRow>,
    selected_row: usize,
}

impl ProfilePicker {
    fn new(mut profiles: Vec<ConnectionProfile>) -> Self {
        profiles.sort_by(|left, right| {
            database_type_rank(left.database_type)
                .cmp(&database_type_rank(right.database_type))
                .then_with(|| {
                    left.display_name()
                        .to_lowercase()
                        .cmp(&right.display_name().to_lowercase())
                })
        });

        let mut rows = Vec::with_capacity(profiles.len() + DatabaseType::ALL.len());
        let mut current_group = None;
        for (index, profile) in profiles.iter().enumerate() {
            if current_group != Some(profile.database_type) {
                current_group = Some(profile.database_type);
                rows.push(PickerRow::Header(profile.database_type));
            }
            rows.push(PickerRow::Profile(index));
        }
        let selected_row = rows
            .iter()
            .position(|row| matches!(row, PickerRow::Profile(_)))
            .unwrap_or_default();

        Self {
            profiles,
            rows,
            selected_row,
        }
    }

    fn select_next(&mut self) {
        if let Some(next) = self.rows[self.selected_row.saturating_add(1)..]
            .iter()
            .position(|row| matches!(row, PickerRow::Profile(_)))
        {
            self.selected_row += next + 1;
        }
    }

    fn select_previous(&mut self) {
        if let Some(previous) = self.rows[..self.selected_row]
            .iter()
            .rposition(|row| matches!(row, PickerRow::Profile(_)))
        {
            self.selected_row = previous;
        }
    }

    fn selected_profile(&self) -> Option<&ConnectionProfile> {
        match self.rows.get(self.selected_row) {
            Some(PickerRow::Profile(index)) => self.profiles.get(*index),
            Some(PickerRow::Header(_)) | None => None,
        }
    }

    fn has_rdb_profile(&self) -> bool {
        self.profiles
            .iter()
            .any(|profile| DatabaseType::RDB.contains(&profile.database_type))
    }
}

fn database_type_rank(database_type: DatabaseType) -> usize {
    DatabaseType::ALL
        .iter()
        .position(|candidate| *candidate == database_type)
        .unwrap_or(DatabaseType::ALL.len())
}

fn render_picker(frame: &mut Frame, picker: &ProfilePicker) {
    let [list_area, hint_area] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(frame.area());
    let theme = DEFAULT_THEME;

    let mut items = picker
        .rows
        .iter()
        .map(|row| match row {
            PickerRow::Header(database_type) => ListItem::new(Line::from(Span::styled(
                format!(" {database_type}"),
                Style::default()
                    .fg(theme.component.navigation.section_header)
                    .add_modifier(Modifier::BOLD),
            ))),
            PickerRow::Profile(index) => {
                let profile = &picker.profiles[*index];
                ListItem::new(Line::from(vec![
                    Span::styled(
                        profile.display_name().to_string(),
                        Style::default().fg(theme.semantic.text.secondary),
                    ),
                    Span::styled(
                        format!("  {}", profile_endpoint(profile)),
                        Style::default().fg(theme.semantic.text.muted),
                    ),
                ]))
            }
        })
        .collect::<Vec<_>>();
    if items.is_empty() {
        items.push(ListItem::new(" No saved connections"));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .title(" Select Connection ")
                .borders(Borders::ALL)
                .border_style(theme.modal_border_style()),
        )
        .highlight_symbol("> ")
        .highlight_style(theme.picker_selected_style());
    let selected = (!picker.rows.is_empty()).then_some(picker.selected_row);
    let mut state = ListState::default().with_selected(selected);
    frame.render_stateful_widget(list, list_area, &mut state);

    let rdb_hint = if picker.has_rdb_profile() {
        ""
    } else {
        " r New RDB "
    };
    let hint = Paragraph::new(format!(
        " ↑/k previous  ↓/j next  Enter select  n New Redis  e Edit Redis{rdb_hint} q/Esc quit "
    ))
    .style(Style::default().fg(theme.semantic.text.muted));
    frame.render_widget(hint, hint_area);
}

fn profile_endpoint(profile: &ConnectionProfile) -> String {
    match profile.database_type {
        DatabaseType::SQLite => profile.database.clone(),
        DatabaseType::PostgreSQL | DatabaseType::MySQL | DatabaseType::Redis => {
            format!("{}:{}/{}", profile.host, profile.port, profile.database)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sabiql_domain::connection::SslMode;

    fn profile(name: &str, database_type: DatabaseType) -> ConnectionProfile {
        ConnectionProfile::new(
            name,
            "localhost",
            database_type.default_port(),
            if database_type == DatabaseType::Redis {
                "2"
            } else {
                "app"
            },
            "user",
            "password",
            SslMode::Prefer,
            database_type,
        )
        .unwrap()
    }

    #[test]
    fn picker_groups_by_database_type_and_sorts_names() {
        let picker = ProfilePicker::new(vec![
            profile("redis", DatabaseType::Redis),
            profile("zeta", DatabaseType::PostgreSQL),
            profile("alpha", DatabaseType::PostgreSQL),
            profile("mysql", DatabaseType::MySQL),
        ]);

        assert_eq!(
            picker.rows,
            vec![
                PickerRow::Header(DatabaseType::PostgreSQL),
                PickerRow::Profile(0),
                PickerRow::Profile(1),
                PickerRow::Header(DatabaseType::MySQL),
                PickerRow::Profile(2),
                PickerRow::Header(DatabaseType::Redis),
                PickerRow::Profile(3),
            ]
        );
        assert_eq!(picker.profiles[0].display_name(), "alpha");
        assert_eq!(picker.profiles[1].display_name(), "zeta");
    }

    #[test]
    fn picker_navigation_skips_group_headers() {
        let mut picker = ProfilePicker::new(vec![
            profile("pg", DatabaseType::PostgreSQL),
            profile("redis", DatabaseType::Redis),
        ]);

        assert_eq!(
            picker
                .selected_profile()
                .map(ConnectionProfile::display_name),
            Some("pg")
        );
        picker.select_next();
        assert_eq!(
            picker
                .selected_profile()
                .map(ConnectionProfile::display_name),
            Some("redis")
        );
        picker.select_previous();
        assert_eq!(
            picker
                .selected_profile()
                .map(ConnectionProfile::display_name),
            Some("pg")
        );
    }

    #[test]
    fn redis_profile_builds_supported_dsn_and_dispatch_target() {
        let target = launch_target(profile("cache", DatabaseType::Redis));

        assert_eq!(
            target,
            LaunchTarget::Redis {
                dsn: "redis://user:password@localhost:6379/2".to_string(),
                read_only: false,
            }
        );
    }

    #[test]
    fn empty_redis_database_defaults_to_zero() {
        let mut redis = profile("cache", DatabaseType::Redis);
        redis.database.clear();
        redis.username.clear();
        redis.password.clear();

        assert_eq!(redis_dsn(&redis), "redis://localhost:6379/0");
    }

    #[test]
    fn rdb_profile_keeps_profile_for_rdb_dispatch() {
        let postgres = profile("primary", DatabaseType::PostgreSQL);

        assert_eq!(launch_target(postgres.clone()), LaunchTarget::Rdb(postgres));
    }
}
