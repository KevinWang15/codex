use super::*;
use crate::transcript_view::tests::cell;
use crate::transcript_view::tests::render;
use crossterm::event::KeyModifiers;

fn history() -> Vec<Arc<dyn HistoryCell>> {
    vec![cell(
        (0..100)
            .map(|i| format!("row {i:03}"))
            .collect::<Vec<_>>()
            .join("\n"),
    )]
}

fn mouse(
    view: &mut TranscriptView,
    cells: &[Arc<dyn HistoryCell>],
    kind: MouseEventKind,
    x: u16,
    y: u16,
) {
    view.handle_mouse(
        MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        },
        cells,
    );
}

#[test]
fn scrollbar_is_transient_and_does_not_rewrap_content() {
    let cells = history();
    let mut view = TranscriptView::default();
    let plain = render(&mut view, &cells, 20, 10);
    assert_ne!(plain[(19, 9)].symbol(), "┃");
    view.scroll(&cells, -1);
    view.scrollbar.reveal();
    let visible = render(&mut view, &cells, 20, 10);
    assert_eq!(visible[(19, 0)].symbol(), "│");
    assert_eq!(visible[(19, 9)].symbol(), "┃");
    view.scrollbar.until = Some(Instant::now() - HIDE_DELAY);
    let hidden = render(&mut view, &cells, 20, 10);
    assert_ne!(hidden[(19, 0)].symbol(), "│");
    assert_eq!(view.area.width, 20);
    assert!(view.scrollbar_hide_delay().is_none());
}

#[test]
fn dragging_preserves_grab_offset_and_clamps_outside_track() {
    let cells = history();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Down(MouseButton::Left),
        19,
        9,
    );
    assert_eq!(view.scrollbar.drag, Some(1));
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Drag(MouseButton::Left),
        5,
        5,
    );
    assert_eq!(view.start(&cells), (0, 45));
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Drag(MouseButton::Left),
        0,
        0,
    );
    assert_eq!(view.start(&cells), (0, 0));
    assert!(!view.is_following());
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Drag(MouseButton::Left),
        0,
        100,
    );
    assert_eq!(view.position, Position::Latest);
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Up(MouseButton::Left),
        0,
        100,
    );
    assert!(view.scrollbar.drag.is_none());
    assert!(view.selection.is_none());
    assert!(view.scrollbar_hide_delay().is_some());
}

#[test]
fn track_click_jumps_and_hover_reveals_hidden_scrollbar() {
    let cells = history();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    mouse(&mut view, &cells, MouseEventKind::Moved, 19, 5);
    let visible = render(&mut view, &cells, 20, 10);
    assert_eq!(visible[(19, 9)].symbol(), "█");
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Down(MouseButton::Left),
        19,
        5,
    );
    assert_eq!(view.start(&cells), (0, 45));
    assert!(view.selection.is_none());
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Up(MouseButton::Left),
        19,
        5,
    );
    mouse(&mut view, &cells, MouseEventKind::Moved, 5, 5);
    assert!(!view.scrollbar.hovered);
    assert!(view.scrollbar_hide_delay().is_some());
}

#[test]
fn fitting_and_tiny_viewports_do_not_capture_text_clicks() {
    for (width, height) in [(20, 10), (1, 1), (0, 0)] {
        let cells = vec![cell("one")];
        let mut view = TranscriptView::default();
        render(&mut view, &cells, width, height);
        assert!(view.scrollbar_geometry(&cells).is_none());
        mouse(
            &mut view,
            &cells,
            MouseEventKind::Down(MouseButton::Left),
            width.saturating_sub(1),
            0,
        );
        assert!(view.scrollbar.drag.is_none());
    }
}

#[test]
fn geometry_updates_after_resize_and_history_growth() {
    let mut cells = history();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    let initial = view.scrollbar_geometry(&cells).unwrap();
    assert_eq!(initial.max_scroll, 90);
    cells.push(cell("new output"));
    render(&mut view, &cells, 30, 20);
    let grown = view.scrollbar_geometry(&cells).unwrap();
    assert_eq!(grown.track.x, 29);
    assert_eq!(grown.track.height, 20);
    assert_eq!(grown.scroll_top, grown.max_scroll);
    assert!(grown.thumb_height >= initial.thumb_height);
}

#[test]
fn dragging_across_long_history_reuses_measured_heights() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    #[derive(Debug)]
    struct CountingCell(Arc<AtomicUsize>);
    impl HistoryCell for CountingCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            self.0.fetch_add(1, Ordering::Relaxed);
            vec!["entry".into()]
        }

        fn raw_lines(&self) -> Vec<Line<'static>> {
            self.display_lines(80)
        }
    }
    let renders = Arc::new(AtomicUsize::new(0));
    let cells: Vec<Arc<dyn HistoryCell>> = (0..200)
        .map(|_| Arc::new(CountingCell(Arc::clone(&renders))) as Arc<dyn HistoryCell>)
        .collect();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    view.scrollbar_geometry(&cells).unwrap();
    let measured = renders.load(Ordering::Relaxed);
    for _ in 0..3 {
        view.scrollbar_geometry(&cells).unwrap();
    }
    assert_eq!(renders.load(Ordering::Relaxed), measured);
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Down(MouseButton::Left),
        19,
        0,
    );
    render(&mut view, &cells, 20, 10);
    assert!(renders.load(Ordering::Relaxed) - measured < 20);
}

#[test]
fn prepending_history_preserves_anchor_and_updates_scroll_range() {
    let mut cells = history();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    view.scroll(&cells, -40);
    let before = view.scrollbar_geometry(&cells).unwrap();
    let anchor = match view.position {
        Position::Reading(anchor) => anchor.key,
        _ => panic!("reading"),
    };
    cells.insert(0, cell("older\nolder\nolder"));
    render(&mut view, &cells, 20, 10);
    let after = view.scrollbar_geometry(&cells).unwrap();
    assert!(after.max_scroll > before.max_scroll);
    assert!(after.scroll_top > before.scroll_top);
    assert!(matches!(view.position, Position::Reading(current) if current.key == anchor));
}

#[test]
fn losing_focus_cancels_drag_and_hover() {
    let cells = history();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Down(MouseButton::Left),
        19,
        9,
    );
    assert!(view.is_dragging_scrollbar());
    view.end_scrollbar_interaction();
    assert!(!view.is_dragging_scrollbar());
    assert!(!view.scrollbar.hovered);
}

#[test]
fn scrollbar_clears_wide_glyph_that_straddles_its_column() {
    let cells = vec![cell(vec!["界".repeat(10); 30].join("\n"))];
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    view.scrollbar.reveal();
    let buffer = render(&mut view, &cells, 20, 10);
    assert_eq!(buffer[(18, 0)].symbol(), " ");
    assert_eq!(buffer[(19, 0)].symbol(), "│");
}

#[test]
fn mutable_history_heights_are_refreshed() {
    use std::sync::atomic::AtomicUsize;
    use std::sync::atomic::Ordering;

    #[derive(Debug)]
    struct MutableCell(Arc<AtomicUsize>);
    impl HistoryCell for MutableCell {
        fn display_lines(&self, _width: u16) -> Vec<Line<'static>> {
            vec!["mutable".into(); self.0.load(Ordering::Relaxed)]
        }
        fn raw_lines(&self) -> Vec<Line<'static>> {
            self.display_lines(80)
        }
        fn has_stable_transcript_height(&self) -> bool {
            false
        }
    }
    let rows = Arc::new(AtomicUsize::new(100));
    let cells: Vec<Arc<dyn HistoryCell>> = vec![Arc::new(MutableCell(Arc::clone(&rows)))];
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    assert_eq!(view.scrollbar_geometry(&cells).unwrap().max_scroll, 90);
    rows.store(200, Ordering::Relaxed);
    render(&mut view, &cells, 20, 10);
    assert_eq!(view.scrollbar_geometry(&cells).unwrap().max_scroll, 190);
}

#[test]
fn clicking_scrollbar_clears_text_selection_without_copying() {
    let cells = history();
    let mut view = TranscriptView::default();
    render(&mut view, &cells, 20, 10);
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Down(MouseButton::Left),
        0,
        0,
    );
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Drag(MouseButton::Left),
        5,
        0,
    );
    mouse(
        &mut view,
        &cells,
        MouseEventKind::Up(MouseButton::Left),
        5,
        0,
    );
    assert!(view.selected_text(&cells).is_some());
    let action = view.handle_mouse(
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 19,
            row: 5,
            modifiers: KeyModifiers::NONE,
        },
        &cells,
    );
    assert!(matches!(action, Some(ViewAction::Changed)));
    assert!(view.selection.is_none());
    assert!(view.is_dragging_scrollbar());
}
