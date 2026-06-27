use std::time::Instant;

use crate::cmd::effect::Effect;
use crate::model::app_state::AppState;
use crate::model::shared::input_mode::InputMode;
use crate::services::AppServices;
use crate::update::action::Action;

pub fn reduce(
    state: &mut AppState,
    action: &Action,
    _now: Instant,
    _services: &AppServices,
) -> Option<Vec<Effect>> {
    match action {
        Action::TryConnect => {
            if state.session.connection_state().is_not_connected()
                && state.modal.active_mode() == InputMode::Normal
            {
                if let Some(dsn) = state.session.dsn.clone() {
                    state.session.begin_connecting(&dsn);
                    Some(vec![Effect::FetchMetadata { dsn }])
                } else {
                    Some(vec![])
                }
            } else {
                Some(vec![])
            }
        }

        _ => None,
    }
}
