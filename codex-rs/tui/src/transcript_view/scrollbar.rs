//! A transient scrollbar over the rightmost transcript column. Only row counts are retained for
//! offscreen stable entries; the bounded text-layout cache continues to own rendered content.

use super::*;
use crossterm::event::MouseButton;
use crossterm::event::MouseEvent;
use crossterm::event::MouseEventKind;
use ratatui::style::Style;
use std::collections::HashMap;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

const HIDE_DELAY: Duration = Duration::from_secs(1);

pub(super) struct MeasuredHeight {
    source: Weak<dyn HistoryCell>,
    first: bool,
    rows: usize,
}

#[derive(Default)]
pub(super) struct Scrollbar {
    pub(super) heights: HashMap<EntryKey, MeasuredHeight>,
    until: Option<Instant>,
    hovered: bool,
    pub(super) painted: bool,
    pub(super) drag: Option<u16>,
}

#[derive(Debug)]
struct Geometry {
    track: Rect,
    thumb_top: u16,
    thumb_height: u16,
    scroll_top: usize,
    max_scroll: usize,
    offsets: Vec<usize>,
}

impl Scrollbar {
    pub(super) fn reveal(&mut self) {
        self.until = Some(Instant::now() + HIDE_DELAY);
    }

    pub(super) fn visible(&self) -> bool {
        self.hovered
            || self.drag.is_some()
            || self.until.is_some_and(|until| until > Instant::now())
    }
}

impl TranscriptView {
    pub(crate) fn is_dragging_scrollbar(&self) -> bool {
        self.scrollbar.drag.is_some()
    }

    pub(crate) fn end_scrollbar_interaction(&mut self) {
        self.scrollbar.drag = None;
        self.scrollbar.hovered = false;
    }

    pub(crate) fn scrollbar_hide_delay(&self) -> Option<Duration> {
        if self.scrollbar.hovered || self.scrollbar.drag.is_some() {
            return None;
        }
        self.scrollbar.until?.checked_duration_since(Instant::now())
    }

    fn scrollbar_geometry(&mut self, cells: &[Arc<dyn HistoryCell>]) -> Option<Geometry> {
        if self.area.width < 2 || self.area.height == 0 {
            return None;
        }
        let snapshot = self.snapshot_cells();
        let cells = snapshot.as_deref().unwrap_or(cells);
        self.scrollbar
            .heights
            .retain(|_, height| height.source.strong_count() > 0);
        let mut offsets = Vec::with_capacity(cells.len() + 2);
        offsets.push(0usize);
        for index in 0..=cells.len() {
            let key = self.entry_key(cells, index);
            // Mutable, animated, and search-expanded entries must be measured from their current
            // revision. Session headers change visibility when older history finishes loading.
            let stable = cells.get(index).is_some_and(|cell| {
                cell.has_stable_transcript_height()
                    && cell.transcript_animation_tick().is_none()
                    && !cell.as_any().is::<crate::history_cell::SessionInfoCell>()
                    && !cell
                        .as_any()
                        .is::<crate::history_cell::FinalMessageSeparator>()
            }) && !self.search.is_active();
            let cached = self
                .scrollbar
                .heights
                .get(&key)
                .filter(|height| stable && height.first == (index == 0))
                .map(|height| height.rows);
            let rows = cached.unwrap_or_else(|| {
                self.layout(cells, index)
                    .map_or(0, |layout| layout.row_count())
            });
            if stable
                && cached.is_none()
                && let Some(cell) = cells.get(index)
            {
                self.scrollbar.heights.insert(
                    key,
                    MeasuredHeight {
                        source: Arc::downgrade(cell),
                        first: index == 0,
                        rows,
                    },
                );
            }
            offsets.push(offsets.last().copied().unwrap_or(0).saturating_add(rows));
        }
        let total = *offsets.last()?;
        let height = usize::from(self.area.height);
        if total <= height {
            return None;
        }
        let (bottom_index, bottom_row) = self.bottom_start(cells);
        let max_scroll = offsets
            .get(bottom_index)
            .copied()
            .unwrap_or(total)
            .saturating_add(bottom_row);
        if max_scroll == 0 {
            return None;
        }
        let (index, row) = self.start(cells);
        let scroll_top = offsets
            .get(index)
            .copied()
            .unwrap_or(total)
            .saturating_add(row)
            .min(max_scroll);
        let thumb_height = ((height as f64 * height as f64 / total as f64).round() as u16)
            .clamp(self.area.height.min(2), self.area.height);
        let travel = self.area.height - thumb_height;
        let thumb_top = self.area.y
            + (scroll_top as f64 / max_scroll as f64 * f64::from(travel)).round() as u16;
        Some(Geometry {
            track: Rect::new(self.area.right() - 1, self.area.y, 1, self.area.height),
            thumb_top,
            thumb_height,
            scroll_top,
            max_scroll,
            offsets,
        })
    }

    pub(super) fn render_scrollbar(&mut self, buf: &mut Buffer, cells: &[Arc<dyn HistoryCell>]) {
        self.scrollbar.painted = false;
        if !self.scrollbar.visible() {
            return;
        }
        let Some(geometry) = self.scrollbar_geometry(cells) else {
            self.scrollbar.drag = None;
            return;
        };
        self.scrollbar.painted = true;
        let active = self.scrollbar.hovered || self.scrollbar.drag.is_some();
        for y in geometry.track.y..geometry.track.bottom() {
            let thumb = y >= geometry.thumb_top && y < geometry.thumb_top + geometry.thumb_height;
            let symbol = if thumb {
                if active { "█" } else { "┃" }
            } else {
                "│"
            };
            if buf[(geometry.track.x - 1, y)].symbol().width() > 1 {
                buf[(geometry.track.x - 1, y)].set_symbol(" ");
            }
            let style = if thumb {
                Style::reset()
            } else {
                Style::reset().patch(crate::style::secondary_text_style())
            };
            buf[(geometry.track.x, y)]
                .set_symbol(symbol)
                .set_style(style);
        }
    }

    fn drag_scrollbar(
        &mut self,
        cells: &[Arc<dyn HistoryCell>],
        geometry: Geometry,
        y: u16,
        grab: u16,
    ) {
        let travel = geometry.track.height - geometry.thumb_height;
        let offset = y
            .saturating_sub(geometry.track.y)
            .saturating_sub(grab)
            .min(travel);
        let target = if travel == 0 {
            0
        } else {
            (f64::from(offset) / f64::from(travel) * geometry.max_scroll as f64).round() as usize
        };
        if target == geometry.max_scroll {
            self.jump_to_latest();
        } else if target != geometry.scroll_top {
            // Seek through measured heights instead of rendering every entry between the old
            // position and the pointer. Only the destination needs a full text layout.
            let current_cells = cells;
            let snapshot = self.snapshot_cells();
            let cells = snapshot.as_deref().unwrap_or(cells);
            let index = geometry
                .offsets
                .partition_point(|offset| *offset <= target)
                .saturating_sub(1);
            let row = target.saturating_sub(geometry.offsets[index]);
            self.cancel_beginning();
            if let Some(layout) = self.layout(cells, index) {
                let offset = layout.position_at(row, 0);
                self.position = Position::Reading(Anchor {
                    key: self.entry_key(cells, index),
                    index,
                    offset,
                    row_bias: layout.row_for_offset(offset) as isize - row as isize,
                });
                self.hold_live_reading(current_cells, layout);
            }
        }
        self.scrollbar.reveal();
    }

    pub(super) fn handle_scrollbar_mouse(
        &mut self,
        event: MouseEvent,
        cells: &[Arc<dyn HistoryCell>],
    ) -> Option<ViewAction> {
        if let Some(grab) = self.scrollbar.drag {
            match event.kind {
                MouseEventKind::Up(MouseButton::Left) => {
                    self.scrollbar.drag = None;
                    self.scrollbar.reveal();
                    self.scrollbar.hovered = self.area.width >= 2
                        && event.column == self.area.right() - 1
                        && event.row >= self.area.y
                        && event.row < self.area.bottom();
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if let Some(geometry) = self.scrollbar_geometry(cells) {
                        self.drag_scrollbar(cells, geometry, event.row, grab);
                    } else {
                        self.scrollbar.drag = None;
                    }
                }
                _ => return None,
            }
            return Some(ViewAction::Changed);
        }
        let inside = self.area.width >= 2
            && event.column == self.area.right() - 1
            && event.row >= self.area.y
            && event.row < self.area.bottom();
        if matches!(
            event.kind,
            MouseEventKind::Moved | MouseEventKind::Down(_) | MouseEventKind::Up(_)
        ) && self.scrollbar.hovered != inside
        {
            self.scrollbar.hovered = inside;
            self.scrollbar.reveal();
            if event.kind == MouseEventKind::Moved {
                return Some(ViewAction::Changed);
            }
        }
        if inside
            && event.kind == MouseEventKind::Down(MouseButton::Left)
            && event.modifiers.is_empty()
            && self.scrollbar_geometry(cells).is_some()
        {
            self.end_select_mode();
            self.end_selection(cells);
            let Some(geometry) = self.scrollbar_geometry(cells) else {
                return Some(ViewAction::Changed);
            };
            self.last_click = None;
            self.clear_activity_focus();
            self.scrollbar.hovered = true;
            let on_thumb = event.row >= geometry.thumb_top
                && event.row < geometry.thumb_top + geometry.thumb_height;
            let grab = if on_thumb {
                event.row - geometry.thumb_top
            } else {
                geometry.thumb_height / 2
            };
            if !on_thumb {
                self.drag_scrollbar(cells, geometry, event.row, grab);
            }
            self.scrollbar.drag = Some(grab);
            self.scrollbar.reveal();
            return Some(ViewAction::Changed);
        }
        None
    }
}

#[cfg(test)]
#[path = "scrollbar_tests.rs"]
mod tests;
