#![cfg_attr(feature = "fail-on-warnings", deny(warnings))]
#![warn(clippy::all, clippy::pedantic, clippy::nursery, clippy::cargo)]
#![allow(clippy::multiple_crate_versions)]

//! Interactive bmodels terminal UI built on `bmux_tui`, `bmux_tui_components`, and
//! `bmux_tui_runtime`.
//!
//! Four tabs: **Inbox** (unacknowledged events), **Models** (canonical models, or per-source
//! listings with `v`), **Sources** (health and schedule), and **History** (recent events). A
//! details pane docks on the right when the terminal is wide enough.
//!
//! The UI talks to the rest of bmodels only through the [`Backend`] trait. Architecture:
//! [`App`] holds state and handles input without touching the terminal; `render` draws it
//! onto a bmux `PaintCx`; `program` adapts it to `bmux_tui_runtime`. Rendering is testable
//! headlessly with [`render_to_text`].

mod app;
pub mod backend;
mod program;
mod render;
pub mod view;
pub mod widgets;

pub use app::{App, Handled, MessageLevel, Modal, SyncState};
pub use backend::{
    Backend, BackendEvent, Command, Data, EventReceiver, EventSink, SourceRow, merge_info,
};
pub use program::{BmodelsProgram, Msg, TuiError, run};
pub use render::DOCK_WIDTH;
pub use view::{Row, Tab, ViewOptions};

/// Renders `app` into an off-screen buffer of `width` × `height` cells.
#[must_use]
pub fn render_to_buffer<B: Backend>(
    app: &mut App<B>,
    width: u16,
    height: u16,
) -> bmux_tui::buffer::Buffer {
    use bmux_tui::geometry::Rect;
    let mut buffer = bmux_tui::buffer::Buffer::empty(Rect::new(0, 0, width, height));
    let mut frame = bmux_tui::frame::Frame::new(&mut buffer);
    render::render(app, &mut bmux_tui::paint::PaintCx::new(&mut frame));
    buffer
}

/// Renders `app` to plain text lines (symbols only), for snapshot tests.
#[must_use]
pub fn render_to_text<B: Backend>(app: &mut App<B>, width: u16, height: u16) -> Vec<String> {
    let buffer = render_to_buffer(app, width, height);
    (0..height)
        .map(|y| {
            buffer
                .row_symbols(y)
                .unwrap_or_default()
                .trim_end()
                .to_owned()
        })
        .collect()
}

#[cfg(test)]
mod tests;
