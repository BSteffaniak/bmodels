//! Generic widgets `bmux_tui_components` does not (yet) provide.
//!
//! Domain-neutral, in the style of bmux components, and shared with bclean's TUI so they can
//! be upstreamed together:
//!
//! * [`FixedRowsState`] / [`paint_fixed_rows`]: a virtual list of uniform-height rows; only
//!   visible rows are painted
//! * [`paint_component`]: paints a bmux `Component` into a terminal-space rectangle
//! * [`inline_details`]: label/value rows with the value beside its label (bmux's
//!   `LabeledDetailsComponent` always stacks them, which halves what fits in a side pane)

use std::ops::Range;

use bmux_tui::component::{Component, Constraints, LayoutCx};
use bmux_tui::geometry::Rect;
use bmux_tui::paint::{LocalRect, PaintCx};
use bmux_tui::style::Style;
use bmux_tui::text::{Line, Span, TextWrap, TextWrapGeometry, wrap_text};
use bmux_tui::text_width::display_width;
use bmux_tui_components::scrollbar::{Scrollbar, ScrollbarState};

/// Lays out and paints `component` into `area` (terminal coordinates of `cx`'s root).
pub fn paint_component(component: &impl Component, area: Rect, cx: &mut PaintCx<'_, '_>) {
    if area.is_empty() {
        return;
    }
    let layout = component.layout(Constraints::tight(area.size()), &mut LayoutCx::new());
    cx.with_child(
        i32::from(area.x),
        i64::from(area.y),
        LocalRect::new(0, 0, area.width, area.height),
        |cx| component.paint(&layout, cx),
    );
}

/// Rows of `label  value` pairs: labels padded to the widest label, values word-wrapped to
/// `width` with continuation lines aligned under the value column.
#[must_use]
pub fn inline_details(
    items: &[(String, String)],
    width: u16,
    label: Style,
    value: Style,
) -> Vec<Line> {
    let label_width = items
        .iter()
        .map(|(l, _)| display_width(l))
        .max()
        .unwrap_or(0)
        + 2;
    let value_width = usize::from(width).saturating_sub(label_width).max(8);
    let mut rows = Vec::new();
    for (name, text) in items {
        let chunks = wrap_text(text, TextWrapGeometry::uniform(value_width), TextWrap::Word);
        for (i, chunk) in chunks.into_iter().enumerate() {
            let head = if i == 0 {
                format!("{name:<label_width$}")
            } else {
                " ".repeat(label_width)
            };
            rows.push(Line::from_spans(vec![
                Span::styled(head, label),
                Span::styled(chunk.trim_end().to_owned(), value),
            ]));
        }
    }
    rows
}

/// Selection and scroll state of a uniform-height virtual list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FixedRowsState {
    /// Selected row index.
    pub selected: usize,
    /// First visible row index.
    pub offset: usize,
}

impl FixedRowsState {
    /// Clamps selection and offset to `len` rows and a `viewport` of rows, scrolling the
    /// minimum amount to keep the selection visible.
    pub const fn clamp(&mut self, len: usize, viewport: usize) {
        if len == 0 {
            self.selected = 0;
            self.offset = 0;
            return;
        }
        if self.selected >= len {
            self.selected = len - 1;
        }
        let viewport = if viewport == 0 { 1 } else { viewport };
        if self.selected < self.offset {
            self.offset = self.selected;
        } else if self.selected >= self.offset + viewport {
            self.offset = self.selected + 1 - viewport;
        }
        let max_offset = len.saturating_sub(viewport);
        if self.offset > max_offset {
            self.offset = max_offset;
        }
    }

    /// Moves the selection by `delta` rows (saturating).
    pub const fn move_by(&mut self, delta: isize, len: usize, viewport: usize) {
        self.selected = self.selected.saturating_add_signed(delta);
        self.clamp(len, viewport);
    }

    /// Moves one page down (`forward`) or up.
    pub const fn page(&mut self, forward: bool, len: usize, viewport: usize) {
        let step = if viewport > 1 { viewport - 1 } else { 1 };
        #[allow(clippy::cast_possible_wrap)] // viewport heights are tiny
        let step = step as isize;
        self.move_by(if forward { step } else { -step }, len, viewport);
    }

    /// Scrolls without moving the selection out of view (mouse wheel).
    pub fn scroll_by(&mut self, delta: isize, len: usize, viewport: usize) {
        let max_offset = len.saturating_sub(viewport.max(1));
        self.offset = self.offset.saturating_add_signed(delta).min(max_offset);
        let last = self.offset + viewport.max(1) - 1;
        self.selected = self
            .selected
            .clamp(self.offset, last.min(len.saturating_sub(1)));
    }

    /// Indices of the rows visible in a `viewport`.
    #[must_use]
    pub fn visible(&self, len: usize, viewport: usize) -> Range<usize> {
        let start = self.offset.min(len);
        start..(start + viewport).min(len)
    }
}

/// Paints the visible window of a uniform-height list into `area`.
///
/// A scrollbar fills the rightmost column when the content overflows.
/// `paint_row(index, row_area, selected, cx)` is called once per visible row.
pub fn paint_fixed_rows(
    cx: &mut PaintCx<'_, '_>,
    area: Rect,
    len: usize,
    state: FixedRowsState,
    mut paint_row: impl FnMut(usize, Rect, bool, &mut PaintCx<'_, '_>),
) {
    if area.is_empty() {
        return;
    }
    let viewport = usize::from(area.height);
    let overflow = len > viewport;
    let rows_width = if overflow {
        area.width.saturating_sub(1)
    } else {
        area.width
    };
    for (row, index) in state.visible(len, viewport).enumerate() {
        let y = area
            .y
            .saturating_add(u16::try_from(row).unwrap_or(u16::MAX));
        paint_row(
            index,
            Rect::new(area.x, y, rows_width, 1),
            index == state.selected,
            cx,
        );
    }
    if overflow {
        let bar_area = Rect::new(area.right().saturating_sub(1), area.y, 1, area.height);
        let content = u16::try_from(len).unwrap_or(u16::MAX);
        let offset = u16::try_from(state.offset).unwrap_or(u16::MAX);
        let scroll_state = ScrollbarState::new(content, area.height).offset(offset);
        Scrollbar::new().paint(bar_area, &scroll_state, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_details_align_and_wrap() {
        let items = vec![
            ("Key".to_owned(), "openai/gpt-6".to_owned()),
            (
                "Capabilities".to_owned(),
                "tools reasoning vision".to_owned(),
            ),
        ];
        let rows: Vec<String> = inline_details(&items, 26, Style::new(), Style::new())
            .iter()
            .map(Line::plain_text)
            .collect();
        assert_eq!(
            rows,
            [
                "Key           openai/gpt-6",
                "Capabilities  tools",
                "              reasoning",
                "              vision"
            ]
        );
    }

    #[test]
    fn clamp_keeps_selection_visible() {
        let mut s = FixedRowsState::default();
        s.move_by(15, 20, 5);
        assert_eq!((s.selected, s.offset), (15, 11));
        s.move_by(-100, 20, 5);
        assert_eq!((s.selected, s.offset), (0, 0));
        s.page(true, 20, 5);
        assert_eq!(s.selected, 4);
        s.clamp(3, 5);
        assert_eq!((s.selected, s.offset), (2, 0));
        s.scroll_by(10, 3, 5);
        assert_eq!(s.offset, 0);
    }
}
