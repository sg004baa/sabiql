use color_eyre::eyre::Result;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph};
use sabiql_app::ports::outbound::ConnectionStore;
use sabiql_domain::connection::{ConnectionProfile, DatabaseType, SslMode};
use sabiql_tui_kit::input::{InputEvent, Key, KeyCombo, Modifiers};
use sabiql_tui_kit::primitives::atoms::text_cursor_spans;
use sabiql_tui_kit::primitives::molecules::render_modal;
use sabiql_tui_kit::theme::DEFAULT_THEME;
use sabiql_tui_kit::tui::TuiRunner;

// This form intentionally lives in the shared shell. Reusing the RDB
// ConnectionSetup reducer would make the RDB state machine own Redis profiles.
pub async fn edit_redis_profile(
    store: &dyn ConnectionStore,
    existing: Option<ConnectionProfile>,
) -> Result<bool> {
    let mut form = RedisProfileForm::new(existing);
    let mut tui = TuiRunner::new()?;
    tui.enter()?;

    let edit_result: Result<bool> = async {
        loop {
            tui.terminal()
                .draw(|frame| render_redis_profile_form(frame, &form))?;

            let Some(event) = tui.next_event().await else {
                return Ok(false);
            };

            match event {
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Esc)
                        || combo == KeyCombo::ctrl(Key::Char('c')) =>
                {
                    return Ok(false);
                }
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Tab)
                        || combo == KeyCombo::plain(Key::Down) =>
                {
                    form.select_next();
                }
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::BackTab)
                        || combo == KeyCombo::plain(Key::Up) =>
                {
                    form.select_previous();
                }
                InputEvent::Key(combo)
                    if combo == KeyCombo::plain(Key::Enter)
                        || combo == KeyCombo::ctrl(Key::Char('s')) =>
                {
                    if form.save(store) {
                        return Ok(true);
                    }
                }
                InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Backspace) => {
                    form.current_input_mut().backspace();
                    form.error = None;
                }
                InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Left) => {
                    form.current_input_mut().move_left();
                }
                InputEvent::Key(combo) if combo == KeyCombo::plain(Key::Right) => {
                    form.current_input_mut().move_right();
                }
                InputEvent::Key(KeyCombo {
                    key: Key::Char(ch),
                    modifiers,
                }) if is_printable_input(ch, modifiers) => {
                    form.current_input_mut().insert(ch);
                    form.error = None;
                }
                InputEvent::Paste(text) => {
                    for ch in text
                        .chars()
                        .filter(|ch| !matches!(ch, '\n' | '\r') && !ch.is_control())
                    {
                        form.current_input_mut().insert(ch);
                    }
                    form.error = None;
                }
                InputEvent::Init | InputEvent::Resize(_, _) | InputEvent::Key(_) => {}
            }
        }
    }
    .await;

    let exit_result = tui.exit();
    let saved = edit_result?;
    exit_result?;
    Ok(saved)
}

fn is_printable_input(ch: char, modifiers: Modifiers) -> bool {
    !ch.is_control() && !modifiers.intersects(Modifiers::CTRL | Modifiers::ALT)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedisField {
    Name,
    Host,
    Port,
    Database,
    Password,
}

impl RedisField {
    const ALL: [Self; 5] = [
        Self::Name,
        Self::Host,
        Self::Port,
        Self::Database,
        Self::Password,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Host => "Host",
            Self::Port => "Port",
            Self::Database => "DB number",
            Self::Password => "Password (optional)",
        }
    }
}

#[derive(Debug, Clone, Default)]
struct FormInput {
    value: String,
    cursor: usize,
}

impl FormInput {
    fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        let cursor = value.chars().count();
        Self { value, cursor }
    }

    fn insert(&mut self, ch: char) {
        let byte_index = byte_index_at_char(&self.value, self.cursor);
        self.value.insert(byte_index, ch);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = byte_index_at_char(&self.value, self.cursor - 1);
        let end = byte_index_at_char(&self.value, self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    fn move_left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    fn move_right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }
}

fn byte_index_at_char(input: &str, char_index: usize) -> usize {
    input
        .char_indices()
        .nth(char_index)
        .map_or(input.len(), |(index, _)| index)
}

struct RedisProfileForm {
    existing: Option<ConnectionProfile>,
    name: FormInput,
    host: FormInput,
    port: FormInput,
    database: FormInput,
    password: FormInput,
    selected: usize,
    error: Option<String>,
}

impl RedisProfileForm {
    fn new(existing: Option<ConnectionProfile>) -> Self {
        let (name, host, port, database, password) = existing.as_ref().map_or_else(
            || {
                (
                    String::new(),
                    "localhost".to_string(),
                    DatabaseType::Redis.default_port().to_string(),
                    "0".to_string(),
                    String::new(),
                )
            },
            |profile| {
                (
                    profile.display_name().to_string(),
                    profile.host.clone(),
                    profile.port.to_string(),
                    profile.database.clone(),
                    profile.password.clone(),
                )
            },
        );
        Self {
            existing,
            name: FormInput::new(name),
            host: FormInput::new(host),
            port: FormInput::new(port),
            database: FormInput::new(database),
            password: FormInput::new(password),
            selected: 0,
            error: None,
        }
    }

    fn selected_field(&self) -> RedisField {
        RedisField::ALL[self.selected]
    }

    fn input(&self, field: RedisField) -> &FormInput {
        match field {
            RedisField::Name => &self.name,
            RedisField::Host => &self.host,
            RedisField::Port => &self.port,
            RedisField::Database => &self.database,
            RedisField::Password => &self.password,
        }
    }

    fn current_input_mut(&mut self) -> &mut FormInput {
        match self.selected_field() {
            RedisField::Name => &mut self.name,
            RedisField::Host => &mut self.host,
            RedisField::Port => &mut self.port,
            RedisField::Database => &mut self.database,
            RedisField::Password => &mut self.password,
        }
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1).min(RedisField::ALL.len() - 1);
    }

    fn select_previous(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn build_profile(&self) -> std::result::Result<ConnectionProfile, String> {
        if self.host.value.trim().is_empty() {
            return Err("Host is required".to_string());
        }
        let port = self
            .port
            .value
            .trim()
            .parse::<u16>()
            .map_err(|_| "Port must be between 1 and 65535".to_string())?;
        if port == 0 {
            return Err("Port must be between 1 and 65535".to_string());
        }
        let database = self
            .database
            .value
            .trim()
            .parse::<u8>()
            .map_err(|_| "DB number must be between 0 and 255".to_string())?
            .to_string();
        let username = self
            .existing
            .as_ref()
            .map_or("", |profile| profile.username.as_str());

        let result = if let Some(existing) = &self.existing {
            ConnectionProfile::with_id(
                existing.id.clone(),
                self.name.value.trim(),
                self.host.value.trim(),
                port,
                database,
                username,
                self.password.value.clone(),
                SslMode::Disable,
                DatabaseType::Redis,
            )
        } else {
            ConnectionProfile::new(
                self.name.value.trim(),
                self.host.value.trim(),
                port,
                database,
                username,
                self.password.value.clone(),
                SslMode::Disable,
                DatabaseType::Redis,
            )
        };
        result.map_err(|error| error.to_string())
    }

    fn save(&mut self, store: &dyn ConnectionStore) -> bool {
        let result = self
            .build_profile()
            .and_then(|profile| store.save(&profile).map_err(|error| error.to_string()));
        match result {
            Ok(()) => true,
            Err(error) => {
                self.error = Some(error);
                false
            }
        }
    }
}

fn render_redis_profile_form(frame: &mut Frame, form: &RedisProfileForm) {
    let theme = DEFAULT_THEME;
    let title = if form.existing.is_some() {
        " Edit Redis Connection "
    } else {
        " New Redis Connection "
    };
    let (_, inner) = render_modal(
        frame,
        Constraint::Percentage(65),
        Constraint::Length(20),
        title,
        " Tab/↑↓ Field  Enter/^S Save  Esc Cancel ",
        &theme,
    );
    let [fields_area, error_area] =
        Layout::vertical([Constraint::Length(15), Constraint::Min(1)]).areas(inner);
    let field_areas = Layout::vertical([Constraint::Length(3); 5]).split(fields_area);

    for (index, field) in RedisField::ALL.iter().copied().enumerate() {
        render_field(frame, field_areas[index], form, field, &theme);
    }

    if let Some(error) = &form.error {
        frame.render_widget(
            Paragraph::new(error.as_str()).style(Style::default().fg(theme.semantic.status.error)),
            error_area,
        );
    }
}

fn render_field(
    frame: &mut Frame,
    area: Rect,
    form: &RedisProfileForm,
    field: RedisField,
    theme: &sabiql_tui_kit::theme::ThemePalette,
) {
    let focused = form.selected_field() == field;
    let input = form.input(field);
    let display = if field == RedisField::Password {
        "•".repeat(input.value.chars().count())
    } else {
        input.value.clone()
    };
    let line = if focused {
        Line::from(text_cursor_spans(
            &display,
            input.cursor,
            0,
            area.width.saturating_sub(2) as usize,
            theme,
        ))
    } else {
        Line::from(display)
    };
    let block = Block::default()
        .title(format!(" {} ", field.label()))
        .borders(Borders::ALL)
        .border_style(theme.modal_input_border_style(focused, false));
    frame.render_widget(Paragraph::new(line).block(block), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use sabiql_domain::connection::ConnectionId;
    use sabiql_infra::adapters::TomlConnectionStore;
    use tempfile::TempDir;

    #[test]
    fn new_form_builds_minimal_redis_profile() {
        let mut form = RedisProfileForm::new(None);
        form.name = FormInput::new("Local cache");
        form.password = FormInput::new("secret");

        let profile = form.build_profile().unwrap();

        assert_eq!(profile.database_type, DatabaseType::Redis);
        assert_eq!(profile.host, "localhost");
        assert_eq!(profile.port, 6379);
        assert_eq!(profile.database, "0");
        assert_eq!(profile.password, "secret");
        assert!(profile.username.is_empty());
        assert_eq!(profile.ssl_mode, SslMode::Disable);
    }

    #[test]
    fn edit_form_preserves_id_and_hidden_username() {
        let id = ConnectionId::from_string("redis-profile");
        let profile = ConnectionProfile::with_id(
            id.clone(),
            "Cache",
            "cache.internal",
            6380,
            "2",
            "default",
            "old",
            SslMode::Disable,
            DatabaseType::Redis,
        )
        .unwrap();
        let mut form = RedisProfileForm::new(Some(profile));
        form.password = FormInput::new("new");

        let edited = form.build_profile().unwrap();

        assert_eq!(edited.id, id);
        assert_eq!(edited.username, "default");
        assert_eq!(edited.password, "new");
    }

    #[test]
    fn validation_rejects_invalid_port_and_database() {
        let mut form = RedisProfileForm::new(None);
        form.name = FormInput::new("Cache");
        form.port = FormInput::new("0");
        assert_eq!(
            form.build_profile().unwrap_err(),
            "Port must be between 1 and 65535"
        );

        form.port = FormInput::new("6379");
        form.database = FormInput::new("256");
        assert_eq!(
            form.build_profile().unwrap_err(),
            "DB number must be between 0 and 255"
        );
    }

    #[test]
    fn saved_profile_round_trips_through_shared_store() {
        let temp_dir = TempDir::new().unwrap();
        let store = TomlConnectionStore::with_config_dir(temp_dir.path().to_path_buf());
        let mut form = RedisProfileForm::new(None);
        form.name = FormInput::new("Local cache");

        assert!(form.save(&store));

        let profiles = store.load_all().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].database_type, DatabaseType::Redis);
        assert_eq!(profiles[0].display_name(), "Local cache");
    }
}
