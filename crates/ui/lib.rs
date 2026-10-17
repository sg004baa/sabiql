#![cfg_attr(
    test,
    allow(
        clippy::disallowed_methods,
        reason = "tests construct fixtures with real clock readings; purity is enforced on production code via the lib target"
    )
)]
#![cfg_attr(
    test,
    allow(
        unreachable_pub,
        reason = "test support visibility is excluded from production API measurement"
    )
)]

pub mod adapters;
pub mod features;
pub mod shell;

pub use sabiql_app as app;
pub use sabiql_domain as domain;

pub mod filter_input;
pub mod sql_highlight;
pub use sabiql_tui_kit::{event, primitives, theme, tui};

#[cfg(test)]
mod event_binding_tests;
