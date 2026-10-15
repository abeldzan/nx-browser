//! Terminal emulation for terminal tabs (ported from kubenx).
//!
//! Wraps `alacritty_terminal`'s grid and parser without its PTY or event
//! loop: bytes come from the PTY reader, replies go back to the PTY. No gpui
//! types here so the behaviour is unit-testable.

use std::cell::RefCell;
use std::rc::Rc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::{viewport_to_point, Config, Osc52, Term, TermMode};
use alacritty_terminal::vte::ansi::{CursorShape, CursorStyle, Processor};

pub use alacritty_terminal::selection::SelectionRange;
#[cfg(test)]
use alacritty_terminal::term::TermMode as Mode;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TermSize {
    columns: usize,
    screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }

    fn screen_lines(&self) -> usize {
        self.screen_lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

/// Collects what the emulator wants the host to do. Drained after each feed.
#[derive(Clone, Default)]
pub struct Listener {
    events: Rc<RefCell<Vec<Event>>>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            // Only the events the host acts on are kept; the rest fire on
            // every grid change and would grow the queue between drains.
            Event::PtyWrite(_) | Event::Bell | Event::Title(_) | Event::ResetTitle => {
                self.events.borrow_mut().push(event);
            }
            _ => {}
        }
    }
}

/// Result of feeding output to the emulator.
#[derive(Default, Debug, PartialEq, Eq)]
pub struct Feed {
    /// Bytes the remote end asked for (cursor position, device attributes);
    /// they must reach its stdin or full-screen programs stall waiting.
    pub reply: Vec<u8>,
    pub bell: bool,
    /// Window title the program set (OSC 0/2): `Some(Some(t))` sets it,
    /// `Some(None)` resets it, `None` leaves it alone. The last one wins.
    pub title: Option<Option<String>>,
}

pub struct Emulator {
    term: Term<Listener>,
    processor: Processor,
    events: Rc<RefCell<Vec<Event>>>,
}

impl Emulator {
    pub fn new(rows: u16, cols: u16, scrollback: usize) -> Self {
        let listener = Listener::default();
        let events = listener.events.clone();
        let term = Term::new(config(scrollback, CursorShape::Block), &size(rows, cols), listener);
        Self {
            term,
            processor: Processor::new(),
            events,
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Feed {
        self.processor.advance(&mut self.term, bytes);
        let mut feed = Feed::default();
        for event in self.events.borrow_mut().drain(..) {
            match event {
                Event::PtyWrite(text) => feed.reply.extend_from_slice(text.as_bytes()),
                Event::Bell => feed.bell = true,
                Event::Title(title) => feed.title = Some(Some(title)),
                Event::ResetTitle => feed.title = Some(None),
                _ => {}
            }
        }
        feed
    }

    /// Applies the user's scrollback length and default cursor to a running terminal.
    pub fn set_options(&mut self, scrollback: usize, cursor: CursorShape) {
        self.term.set_options(config(scrollback, cursor));
    }

    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    pub fn term_mut(&mut self) -> &mut Term<Listener> {
        &mut self.term
    }

    /// `(rows, cols)` of the visible grid.
    pub fn size(&self) -> (u16, u16) {
        (
            u16::try_from(self.term.screen_lines()).unwrap_or(u16::MAX),
            u16::try_from(self.term.columns()).unwrap_or(u16::MAX),
        )
    }

    pub fn resize(&mut self, rows: u16, cols: u16) {
        self.term.resize(size(rows, cols));
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    pub fn bracketed_paste(&self) -> bool {
        self.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Lines the view is scrolled back from the live bottom.
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Positive moves toward older output. Clamped to the history length.
    pub fn scroll_by(&mut self, lines: i32) {
        if lines != 0 {
            self.term.scroll_display(Scroll::Delta(lines));
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
        }
    }

    /// Grid point under a viewport cell, accounting for the scrolled view.
    pub fn point_at(&self, row: u16, col: u16) -> Point {
        viewport_to_point(
            self.display_offset(),
            Point::new(usize::from(row), Column(usize::from(col))),
        )
    }

    pub fn start_selection(&mut self, ty: SelectionType, row: u16, col: u16, side: Side) {
        let point = self.point_at(row, col);
        self.term.selection = Some(Selection::new(ty, point, side));
    }

    pub fn update_selection(&mut self, row: u16, col: u16, side: Side) {
        let point = self.point_at(row, col);
        if let Some(selection) = self.term.selection.as_mut() {
            selection.update(point, side);
        }
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn selection_range(&self) -> Option<SelectionRange> {
        self.term
            .selection
            .as_ref()
            .filter(|selection| !selection.is_empty())
            .and_then(|selection| selection.to_range(&self.term))
    }

    pub fn selected_text(&self) -> Option<String> {
        self.selection_range()?;
        self.term
            .selection_to_string()
            .filter(|text| !text.is_empty())
    }

    /// Visible rows as text, trailing blanks trimmed per row and trailing
    /// empty rows dropped.
    #[cfg(test)]
    pub fn visible_text(&self) -> String {
        use alacritty_terminal::index::Line;
        let offset = self.display_offset() as i32;
        let mut rows: Vec<String> = (0..self.term.screen_lines() as i32)
            .map(|row| {
                let line = Line(row - offset);
                let start = Point::new(line, Column(0));
                let end = Point::new(line, Column(self.term.columns() - 1));
                self.term
                    .bounds_to_string(start, end)
                    .trim_end()
                    .to_string()
            })
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows.join("\n")
    }
}

fn config(scrollback: usize, cursor: CursorShape) -> Config {
    Config {
        scrolling_history: scrollback,
        default_cursor_style: CursorStyle { shape: cursor, blinking: false },
        // A program must not write to the user's clipboard unprompted.
        osc52: Osc52::Disabled,
        ..Config::default()
    }
}

fn size(rows: u16, cols: u16) -> TermSize {
    TermSize {
        columns: usize::from(cols.max(1)),
        screen_lines: usize::from(rows.max(1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrollback_reaches_past_one_screen() {
        let mut emu = Emulator::new(3, 16, 100);
        for i in 0..12 {
            emu.feed(format!("L{i:02}\r\n").as_bytes());
        }
        let tail = emu.visible_text();
        assert!(tail.contains("L11"), "live tail was {tail:?}");
        let rows = emu.size().0 as i32;
        emu.scroll_by(rows + 2);
        assert!(emu.display_offset() > rows as usize);
        let scrolled = emu.visible_text();
        assert_ne!(scrolled, tail);
        assert!(!scrolled.contains("L11"), "scrolled was {scrolled:?}");
        // Over-scrolling stops at the oldest line instead of failing.
        emu.scroll_by(10_000);
        assert!(emu.visible_text().starts_with("L00"));
        emu.scroll_to_bottom();
        assert_eq!(emu.visible_text(), tail);
    }

    #[test]
    fn spaces_and_alternate_screen_survive() {
        let mut emu = Emulator::new(3, 12, 10);
        emu.feed(b"a  b\x1b[2;6HX");
        assert_eq!(emu.visible_text(), "a  b\n     X");
        emu.feed(b"\x1b[?1049h\x1b[2J\x1b[Htop");
        assert!(emu.mode().contains(Mode::ALT_SCREEN));
        assert_eq!(emu.visible_text(), "top");
        emu.feed(b"\x1b[?1049l");
        assert!(!emu.mode().contains(Mode::ALT_SCREEN));
        assert_eq!(emu.visible_text(), "a  b\n     X");
        emu.resize(5, 20);
        assert_eq!(emu.size(), (5, 20));
    }

    #[test]
    fn shrinking_keeps_the_prompt_and_growing_restores_history() {
        let mut emu = Emulator::new(4, 8, 10);
        emu.feed(b"AAA\r\nBBB\r\nCCC\r\nDDD");
        assert_eq!(emu.visible_text(), "AAA\nBBB\nCCC\nDDD");
        emu.resize(2, 8);
        assert_eq!(emu.visible_text(), "CCC\nDDD");
        emu.scroll_by(2);
        assert_eq!(emu.visible_text(), "AAA\nBBB");
        emu.scroll_to_bottom();
        emu.resize(4, 8);
        assert_eq!(emu.visible_text(), "AAA\nBBB\nCCC\nDDD");
    }

    #[test]
    fn narrowing_reflows_wrapped_lines() {
        let mut emu = Emulator::new(4, 10, 10);
        emu.feed(b"0123456789abcdef");
        assert_eq!(emu.visible_text(), "0123456789\nabcdef");
        emu.resize(4, 20);
        assert_eq!(emu.visible_text(), "0123456789abcdef");
    }

    #[test]
    fn device_queries_are_answered() {
        let mut emu = Emulator::new(5, 20, 10);
        let feed = emu.feed(b"ab\x1b[6n");
        // Cursor is on row 1, column 3 (1-based) after "ab".
        assert_eq!(feed.reply, b"\x1b[1;3R");
        assert!(!feed.bell);
        assert!(emu.feed(b"\x07").bell);
    }

    #[test]
    fn clipboard_writes_from_the_remote_are_ignored() {
        let mut emu = Emulator::new(5, 20, 10);
        let feed = emu.feed(b"\x1b]52;c;aGVsbG8=\x07");
        assert!(feed.reply.is_empty());
    }

    #[test]
    fn window_title_follows_osc_0_and_2() {
        let mut emu = Emulator::new(5, 20, 10);
        assert_eq!(emu.feed(b"plain output").title, None);
        assert_eq!(emu.feed(b"\x1b]0;vim\x07").title, Some(Some("vim".into())));
        assert_eq!(
            emu.feed(b"\x1b]2;htop\x1b\\").title,
            Some(Some("htop".into()))
        );
        // The last title in one chunk wins.
        assert_eq!(
            emu.feed(b"\x1b]0;a\x07\x1b]0;b\x07").title,
            Some(Some("b".into()))
        );
    }

    #[test]
    fn title_stack_restores_and_resets() {
        let mut emu = Emulator::new(5, 20, 10);
        emu.feed(b"\x1b]0;shell\x07");
        emu.feed(b"\x1b[22;0t\x1b]0;vim\x07");
        // Popping restores the title saved before the program started.
        assert_eq!(emu.feed(b"\x1b[23;0t").title, Some(Some("shell".into())));
        // An empty title is still a title event; the tab treats it as unset.
        assert_eq!(emu.feed(b"\x1b]0;\x07").title, Some(Some(String::new())));
    }

    #[test]
    fn mode_flags_follow_the_program() {
        let mut emu = Emulator::new(5, 20, 10);
        assert!(!emu.bracketed_paste());
        emu.feed(b"\x1b[?2004h\x1b[?1h\x1b[?1000h\x1b[?1006h");
        let mode = emu.mode();
        assert!(emu.bracketed_paste());
        assert!(mode.contains(Mode::APP_CURSOR));
        assert!(mode.contains(Mode::MOUSE_REPORT_CLICK));
        assert!(mode.contains(Mode::SGR_MOUSE));
    }

    #[test]
    fn selection_copies_across_rows_in_either_direction() {
        let mut emu = Emulator::new(4, 20, 10);
        emu.feed(b"alpha beta\r\n\r\ngamma delta");
        // From the left half of "beta" to the left half of column 5 two rows down.
        emu.start_selection(SelectionType::Simple, 0, 6, Side::Left);
        emu.update_selection(2, 5, Side::Left);
        assert_eq!(emu.selected_text().as_deref(), Some("beta\n\ngamma"));
        emu.start_selection(SelectionType::Simple, 2, 5, Side::Left);
        emu.update_selection(2, 0, Side::Left);
        assert_eq!(emu.selected_text().as_deref(), Some("gamma"));
        emu.clear_selection();
        assert!(emu.selected_text().is_none());
        // A click without a drag selects nothing.
        emu.start_selection(SelectionType::Simple, 0, 2, Side::Left);
        assert!(emu.selection_range().is_none());
    }

    #[test]
    fn double_and_triple_click_pick_word_and_line() {
        let mut emu = Emulator::new(3, 30, 10);
        emu.feed(b"kubectl get pods -n kube-system");
        emu.start_selection(SelectionType::Semantic, 0, 9, Side::Left);
        assert_eq!(emu.selected_text().as_deref(), Some("get"));
        emu.start_selection(SelectionType::Lines, 0, 9, Side::Left);
        assert_eq!(
            emu.selected_text().as_deref().map(str::trim_end),
            Some("kubectl get pods -n kube-system")
        );
    }

    #[test]
    fn selection_follows_its_text_into_history() {
        let mut emu = Emulator::new(3, 12, 50);
        emu.feed(b"keep me\r\n");
        emu.start_selection(SelectionType::Lines, 0, 0, Side::Left);
        for i in 0..6 {
            emu.feed(format!("x{i}\r\n").as_bytes());
        }
        emu.scroll_by(100);
        assert_eq!(
            emu.selected_text().as_deref().map(str::trim_end),
            Some("keep me")
        );
    }
}
