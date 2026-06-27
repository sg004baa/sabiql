use super::{ExecBinding, KeyBinding, ModeRow};
use super::{Key, KeyCombo};
use crate::update::action::{Action, ScrollAmount, ScrollDirection, ScrollTarget};

// =============================================================================
// Connection Setup
// =============================================================================

pub const CONNECTION_SETUP_KEYS: &[KeyBinding] = &[
    KeyBinding {
        key_short: "Tab/⇧Tab",
        key: "Tab/⇧Tab",
        desc_short: "Next/Prev",
        description: "Next/Previous field",
        action: Action::None,
        combos: &[],
    },
    KeyBinding {
        key_short: "Tab",
        key: "Tab",
        desc_short: "Next",
        description: "Next field",
        action: Action::None,
        combos: &[],
    },
    KeyBinding {
        key_short: "⇧Tab",
        key: "⇧Tab",
        desc_short: "Prev",
        description: "Previous field",
        action: Action::None,
        combos: &[],
    },
    KeyBinding {
        key_short: "^S",
        key: "Ctrl+S",
        desc_short: "Connect",
        description: "Save and connect",
        action: Action::ConnectionSetupSave,
        combos: &[KeyCombo::ctrl(Key::Char('s'))],
    },
    KeyBinding {
        key_short: "Esc",
        key: "Esc",
        desc_short: "Cancel",
        description: "Cancel",
        action: Action::ConnectionSetupCancel,
        combos: &[KeyCombo::plain(Key::Esc)],
    },
    KeyBinding {
        key_short: "Enter",
        key: "Enter",
        desc_short: "Toggle",
        description: "Toggle dropdown (SSL field)",
        action: Action::ConnectionSetupToggleDropdown,
        combos: &[KeyCombo::plain(Key::Enter)],
    },
    KeyBinding {
        key_short: "^N/^P/↑↓",
        key: "Ctrl+N / Ctrl+P / ↑ / ↓",
        desc_short: "Select",
        description: "Dropdown navigation",
        action: Action::None,
        combos: &[],
    },
    KeyBinding {
        key_short: "^F",
        key: "Ctrl+F",
        desc_short: "Files",
        description: "Open SQLite file picker",
        action: Action::OpenFilePicker,
        combos: &[KeyCombo::ctrl(Key::Char('f'))],
    },
];

// =============================================================================
// Connection Error
// =============================================================================

pub const CONNECTION_ERROR_ROWS: &[ModeRow] = &[
    ModeRow {
        key_short: "e",
        key: "e",
        desc_short: "Edit",
        description: "Edit connection settings",
        bindings: &[ExecBinding {
            action: Action::ReenterConnectionSetup,
            combos: &[KeyCombo::plain(Key::Char('e'))],
        }],
    },
    ModeRow {
        key_short: "s",
        key: "s",
        desc_short: "Switch",
        description: "Switch to another connection",
        bindings: &[ExecBinding {
            action: Action::RequestConnectionSwitch,
            combos: &[KeyCombo::plain(Key::Char('s'))],
        }],
    },
    ModeRow {
        key_short: "d",
        key: "d",
        desc_short: "Details",
        description: "Toggle error details",
        bindings: &[ExecBinding {
            action: Action::ToggleConnectionErrorDetails,
            combos: &[KeyCombo::plain(Key::Char('d'))],
        }],
    },
    ModeRow {
        key_short: "y",
        key: "y",
        desc_short: "Copy",
        description: "Copy error to clipboard",
        bindings: &[ExecBinding {
            action: Action::CopyConnectionError,
            combos: &[KeyCombo::plain(Key::Char('y'))],
        }],
    },
    ModeRow {
        key_short: "^N/^P/j/k/↑↓",
        key: "j / k / Ctrl+N / Ctrl+P / ↑ / ↓",
        desc_short: "Scroll",
        description: "Scroll error",
        bindings: &[
            ExecBinding {
                action: Action::Scroll {
                    target: ScrollTarget::ConnectionError,
                    direction: ScrollDirection::Down,
                    amount: ScrollAmount::Line,
                },
                combos: &[
                    KeyCombo::plain(Key::Char('j')),
                    KeyCombo::plain(Key::Down),
                    KeyCombo::ctrl(Key::Char('n')),
                ],
            },
            ExecBinding {
                action: Action::Scroll {
                    target: ScrollTarget::ConnectionError,
                    direction: ScrollDirection::Up,
                    amount: ScrollAmount::Line,
                },
                combos: &[
                    KeyCombo::plain(Key::Char('k')),
                    KeyCombo::plain(Key::Up),
                    KeyCombo::ctrl(Key::Char('p')),
                ],
            },
        ],
    },
    ModeRow {
        key_short: "Esc/q",
        key: "Esc / q",
        desc_short: "Close",
        description: "Close",
        bindings: &[ExecBinding {
            action: Action::CloseConnectionError,
            combos: &[KeyCombo::plain(Key::Esc), KeyCombo::plain(Key::Char('q'))],
        }],
    },
    ModeRow {
        key_short: "r",
        key: "r",
        desc_short: "Retry",
        description: "Retry service connection",
        bindings: &[ExecBinding {
            action: Action::RetryServiceConnection,
            combos: &[KeyCombo::plain(Key::Char('r'))],
        }],
    },
];
