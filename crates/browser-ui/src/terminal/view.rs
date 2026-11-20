//! A terminal tab's screen: PTY in, alacritty grid + kitty images out (ported from kubenx's workloads.rs), drawn by
//! the engine's native screen view.

use super::agent::{self, Agent, AgentState};
use super::emu::Emulator;
use super::input::{key_bytes, mouse_bytes, reports_mouse, Keystroke, Modifiers, MouseAction, MouseButton};
use super::kitty::{Delete, Graphics, Piece, Show, Splitter};
use super::pty::{self, Pty};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Direction, Point, Side};
use alacritty_terminal::selection::SelectionType;
use alacritty_terminal::term::cell::{Flags, Hyperlink};
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::search::{RegexIter, RegexSearch};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::vte::ansi::{Color as AnsiColor, CursorShape, NamedColor, Rgb};
use browser_core::Settings;
use browser_engine_macos::{TermHost, TermImage, TermInput, TermPaint, TermSpan, TermSurface};
use futures::StreamExt;
use portable_pty::PtySize;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Cell size before the font is measured.
const CELL_GUESS: (f32, f32) = (7.2, 16.);
/// Room between the screen's edge and the grid.
const PAD: f32 = 6.;
/// Prefix of the hyperlink id that pins an image row to a grid cell, so images move with the text.
const ANCHOR: &str = "nxkitty:";
/// shortcut: placements are only forgotten past this count (oldest first), not when their rows leave history.
const MAX_PLACEMENTS: usize = 512;

/// sRGB with alpha.
type Rgba = [f64; 4];

#[derive(Clone, Copy, PartialEq, Eq)]
enum PointerGrab {
    /// A drag extends the text selection.
    Select,
    /// The program asked for mouse events and got the press.
    Report,
}

struct Pointer {
    cell: (f32, f32),
    scale: f32,
    /// The screen's size in points.
    size: (f32, f32),
    held: Option<(MouseButton, PointerGrab)>,
    last_cell: Option<(u16, u16)>,
    /// Wheel lines not yet applied, so slow trackpad swipes still scroll.
    wheel_remainder: f32,
}

/// What the view should do after a pointer event.
#[derive(Default, Clone, Copy)]
struct PointerEffect {
    redraw: bool,
    consumed: bool,
}

/// An image drawn over the grid; its rows are found through the anchor cells stamped at `col`.
struct Placement {
    key: u64,
    image_id: u32,
    image: Rc<TermImage>,
    /// Drawn size in logical pixels.
    size: (f32, f32),
}

pub struct TerminalView {
    pty: Option<Pty>,
    emu: Emulator,
    splitter: Splitter,
    graphics: Graphics,
    placements: Vec<Placement>,
    next_placement: u64,
    pointer: Pointer,
    osc_title: Option<String>,
    /// The foreground process group when the title was set (`None`: the shell), so a title dies with its program.
    title_owner: Option<libc::pid_t>,
    /// The coding agent in the foreground and its status, refreshed by the window each tick.
    agent: Option<AgentState>,
    font: String,
    font_size: f32,
    line_height: f32,
    /// The shell exited (or never started); the window closes the tab.
    pub exited: bool,
    /// A link Cmd-clicked in the output, for the window to open in a new tab.
    pub open_url: Option<String>,
    /// The native view showing it; shared so the engine can place it without the terminal borrowed.
    pub(super) surface: Option<Rc<TermSurface>>,
}

/// Reaches tab `.0`'s terminal from its screen view.
struct Link(String);

impl TermHost for Link {
    fn input(&self, input: TermInput) -> bool {
        super::with(&self.0, |view| view.on_input(input)).unwrap_or(false)
    }

    fn paint(&self) -> Option<TermPaint> {
        super::with(&self.0, |view| view.paint())
    }
}

impl TerminalView {
    pub fn new(tab_id: &str, cwd: &Path, settings: &Settings) -> Self {
        let (rows, cols) = (24, 80);
        let mut view = Self {
            pty: None,
            emu: Emulator::new(rows, cols, settings.terminal_scrollback),
            splitter: Splitter::default(),
            graphics: Graphics::default(),
            placements: Vec::new(),
            next_placement: 1,
            pointer: Pointer { cell: CELL_GUESS, scale: 2., size: (0., 0.), held: None, last_cell: None, wheel_remainder: 0. },
            osc_title: None,
            title_owner: None,
            agent: None,
            font: String::new(),
            font_size: 0.,
            line_height: 0.,
            exited: false,
            open_url: None,
            surface: TermSurface::new(Box::new(Link(tab_id.to_string()))).map(Rc::new),
        };
        view.apply_prefs(settings);
        match pty::spawn(&settings.terminal_shell, cwd, view.pty_size(rows, cols)) {
            Ok((pty, mut output)) => {
                view.pty = Some(pty);
                let id = tab_id.to_string();
                std::thread::spawn(move || {
                    while let Some(mut bytes) = futures::executor::block_on(output.next()) {
                        // Coalesce what already arrived so a flood of output costs one feed.
                        while let Ok(more) = output.try_recv() {
                            bytes.extend_from_slice(&more);
                        }
                        let id = id.clone();
                        // Waits for the main thread, so a flood waits on the screen as it waits on the shell.
                        dispatch2::DispatchQueue::main().exec_sync(move || super::feed(&id, bytes));
                    }
                    dispatch2::DispatchQueue::main().exec_async(move || {
                        super::with(&id, |view| view.exited = true);
                    });
                });
            }
            Err(error) => {
                view.emu.feed(format!("Could not start a shell: {error}\r\n").as_bytes());
                view.exited = true;
            }
        }
        view
    }

    /// Font, cursor and scrollback from settings; cells are measured again and the grid refitted.
    pub fn apply_prefs(&mut self, settings: &Settings) {
        let font = settings.terminal_font.trim();
        self.font = if font.is_empty() { "Menlo".to_string() } else { font.to_string() };
        self.font_size = (settings.terminal_font_size as f32).clamp(6., 48.);
        self.line_height = (settings.terminal_line_height as f32).clamp(1., 3.);
        let cursor = match settings.terminal_cursor.as_str() {
            "beam" => CursorShape::Beam,
            "underline" => CursorShape::Underline,
            _ => CursorShape::Block,
        };
        self.emu.set_options(settings.terminal_scrollback.clamp(100, 1_000_000), cursor);
        let (width, height) = browser_engine_macos::cell_size(&self.font, f64::from(self.font_size), f64::from(self.line_height));
        self.pointer.cell = (width as f32, height as f32);
        self.fit();
        self.redraw();
    }

    /// What the tab should say: the title the program in front set (an agent's session name), else the agent
    /// or running program, else the folder.
    pub fn title(&self) -> String {
        let front = self.pty.as_ref().and_then(Pty::foreground_group);
        let program_title = self.osc_title.as_deref().filter(|_| self.title_owner == front);
        if let Some(AgentState { agent, .. }) = self.agent {
            return program_title.and_then(|title| agent::session_title(agent, title)).unwrap_or_else(|| agent.name().into());
        }
        let program = || self.pty.as_ref().and_then(Pty::foreground);
        program_title.map(str::to_string).or_else(program).unwrap_or_else(|| self.cwd().map_or_else(|| "Terminal".into(), |dir| short_path(&dir)))
    }

    /// The coding agent in the foreground and its status, as of the last `refresh_agent`.
    pub fn agent(&self) -> Option<AgentState> {
        self.agent
    }

    /// Re-reads which agent is in the foreground and what it is doing; true if that changed.
    #[must_use]
    pub fn refresh_agent(&mut self, seen: bool) -> bool {
        let names = self.pty.as_ref().map(Pty::foreground_names).unwrap_or_default();
        let state = names.iter().find_map(|name| Agent::from_process(name)).map(|agent| {
            let now = agent::status(agent, self.osc_title.as_deref(), &self.emu.screen_tail(15));
            let prev = self.agent.filter(|state| state.agent == agent).map(|state| state.status);
            AgentState { agent, status: agent::next(prev, now, seen) }
        });
        std::mem::replace(&mut self.agent, state) != state
    }

    pub fn cwd(&self) -> Option<PathBuf> {
        self.pty.as_ref().and_then(Pty::cwd)
    }

    pub(super) fn redraw(&self) {
        if let Some(surface) = &self.surface {
            surface.redraw();
        }
    }

    fn pty_size(&self, rows: u16, cols: u16) -> PtySize {
        let (w, h) = (self.pointer.cell.0 * self.pointer.scale, self.pointer.cell.1 * self.pointer.scale);
        // Pixel sizes let icat and yazi size images to the window.
        PtySize { rows, cols, pixel_width: (f32::from(cols) * w) as u16, pixel_height: (f32::from(rows) * h) as u16 }
    }

    fn send(&mut self, bytes: &[u8]) {
        if let Some(pty) = self.pty.as_mut().filter(|_| !self.exited) {
            pty.write(bytes);
        }
    }

    /// Runs output through the image filter and the emulator, answering whatever the program asked.
    pub(super) fn feed(&mut self, bytes: &[u8]) {
        for piece in self.splitter.split(bytes) {
            match piece {
                Piece::Text(text) => {
                    let feed = self.emu.feed(&text);
                    if let Some(title) = feed.title {
                        self.osc_title = title.as_deref().and_then(clean_title);
                        self.title_owner = self.pty.as_ref().and_then(Pty::foreground_group);
                    }
                    self.send(&feed.reply);
                }
                Piece::Graphics(command) => {
                    let outcome = self.graphics.handle(&command);
                    self.send(&outcome.reply);
                    match outcome.delete {
                        Some(Delete::All) => self.placements.clear(),
                        Some(Delete::Id(id)) => self.placements.retain(|placement| placement.image_id != id),
                        None => {}
                    }
                    if let Some(show) = outcome.show {
                        self.place(show);
                    }
                    self.graphics.retired.clear();
                }
            }
        }
        self.redraw();
    }

    /// Pins an image at the cursor: each row it covers gets an anchor cell, then the cursor moves past it.
    fn place(&mut self, show: Show) {
        let (cell, scale) = (self.pointer.cell, self.pointer.scale);
        let natural = (show.size.0 as f32 / scale, show.size.1 as f32 / scale);
        let aspect = natural.1 / natural.0.max(1.);
        let size = match (show.cols, show.rows) {
            (0, 0) => natural,
            (c, 0) => (c as f32 * cell.0, c as f32 * cell.0 * aspect),
            (0, r) => (r as f32 * cell.1 / aspect.max(0.001), r as f32 * cell.1),
            (c, r) => (c as f32 * cell.0, r as f32 * cell.1),
        };
        // `c`/`r` come from program output: r=4000000000 would feed billions of newlines on the UI thread.
        let (lines, columns) = self.emu.size();
        let size = (size.0.min(f32::from(columns) * cell.0), size.1.min(f32::from(lines) * 4. * cell.1));
        let cols = ((size.0 / cell.0).ceil() as usize).max(1);
        let rows = ((size.1 / cell.1).ceil() as usize).max(1);
        let key = self.next_placement;
        self.next_placement += 1;
        let start = self.emu.term().grid().cursor.point;
        for row in 0..rows {
            // Each row is anchored before the cursor moves on, so a scroll at the bottom carries it along.
            if row > 0 {
                self.emu.feed(b"\n");
            }
            let line = self.emu.term().grid().cursor.point.line;
            let link = Hyperlink::new(Some(format!("{ANCHOR}{key}:{row}")), String::new());
            self.emu.term_mut().grid_mut()[Point::new(line, start.column)].set_hyperlink(Some(link));
        }
        if show.move_cursor {
            let right = (start.column.0 + cols).min(self.emu.term().columns().saturating_sub(1));
            self.emu.feed(format!("\x1b[{}G", right + 1).as_bytes());
        } else {
            self.emu.term_mut().grid_mut().cursor.point = start;
        }
        self.placements.push(Placement { key, image_id: show.image_id, image: show.image, size });
        if self.placements.len() > MAX_PLACEMENTS {
            self.placements.remove(0);
        }
    }

    /// Fits the grid to the screen's size.
    fn fit(&mut self) {
        let ((width, height), (cell_w, cell_h)) = (self.pointer.size, self.pointer.cell);
        if width <= 0. || height <= 0. {
            return;
        }
        let cols = ((width - 2. * PAD) / cell_w).floor().clamp(2., 500.) as u16;
        let rows = ((height - 2. * PAD) / cell_h).floor().clamp(2., 200.) as u16;
        self.resize(rows, cols);
    }

    fn resize(&mut self, rows: u16, cols: u16) {
        if self.emu.size() == (rows, cols) {
            return;
        }
        // Reflow moves the text under the selection.
        self.emu.clear_selection();
        self.emu.resize(rows, cols);
        let size = self.pty_size(rows, cols);
        if let Some(pty) = &self.pty {
            pty.resize(size);
        }
    }

    /// Handles what the screen view reports; false if it went unused.
    fn on_input(&mut self, input: TermInput) -> bool {
        let effect = match input {
            TermInput::Key(keystroke) => return self.on_key(&keystroke),
            TermInput::Down { button, x, y, clicks, modifiers } => self.mouse_down(button, (x, y), clicks, &modifiers),
            TermInput::Drag { x, y, modifiers, .. } => self.mouse_drag((x, y), &modifiers),
            TermInput::Up { button, x, y, modifiers } => self.mouse_up(button, (x, y), &modifiers),
            TermInput::Move { x, y, modifiers } => self.mouse_move((x, y), &modifiers),
            TermInput::Scroll { dy, precise, x, y, modifiers } => self.scroll_wheel(dy as f32, precise, (x, y), &modifiers),
            // Dropped files arrive as typed paths, like Terminal.app; CLI agents spot image paths and attach them.
            TermInput::Drop(paths) => {
                let bytes = paste_bytes(&dropped_paths(&paths), self.emu.bracketed_paste());
                self.write_input(&bytes);
                return true;
            }
            TermInput::Copy => {
                self.copy();
                return true;
            }
            TermInput::Paste => {
                self.paste();
                return true;
            }
            TermInput::Resize { width, height, scale } => {
                self.pointer.size = (width as f32, height as f32);
                self.pointer.scale = scale as f32;
                self.fit();
                PointerEffect { redraw: true, consumed: true }
            }
        };
        if effect.redraw {
            self.redraw();
        }
        effect.consumed
    }

    fn on_key(&mut self, keys: &Keystroke) -> bool {
        let m = &keys.modifiers;
        let copy = (m.platform && !m.shift && keys.key == "c") || (m.control && m.shift && keys.key == "c");
        let paste = (m.platform && !m.shift && keys.key == "v") || (m.control && m.shift && keys.key == "v");
        if copy {
            self.copy();
            return true;
        }
        if paste {
            self.paste();
            return true;
        }
        match key_bytes(keys, self.emu.mode()) {
            Some(bytes) => {
                self.write_input(&bytes);
                true
            }
            None => false,
        }
    }

    pub fn has_selection(&self) -> bool {
        self.emu.selection_range().is_some()
    }

    pub fn copy(&mut self) {
        if let Some(text) = self.emu.selected_text() {
            browser_engine_macos::set_clipboard_text(&text);
        }
    }

    pub fn paste(&mut self) {
        match browser_engine_macos::clipboard_text() {
            Ok(text) => {
                let bytes = paste_bytes(&text, self.emu.bracketed_paste());
                self.write_input(&bytes);
            }
            // An image (a screenshot): Ctrl+V makes agent CLIs read it from the clipboard and attach it.
            Err(true) => self.write_input(&[0x16]),
            Err(false) => {}
        }
    }

    /// Wipes the screen and scrollback, then sends Ctrl+L so the shell redraws its prompt at the top.
    pub fn clear(&mut self) {
        // A full-screen program (vim, htop) owns the screen and would just be redrawn over.
        if self.emu.mode().contains(TermMode::ALT_SCREEN) {
            return;
        }
        self.feed(b"\x1b[H\x1b[2J\x1b[3J");
        self.write_input(&[0x0c]);
    }

    fn write_input(&mut self, bytes: &[u8]) {
        self.emu.clear_selection();
        self.emu.scroll_to_bottom();
        self.send(bytes);
        self.redraw();
    }

    /// The viewport cell under a screen position and which half of it,
    /// clamped to the grid so a drag past an edge keeps extending.
    fn cell_at(&self, (x, y): (f64, f64)) -> (u16, u16, Side) {
        let (cell_w, cell_h) = self.pointer.cell;
        let (rows, cols) = self.emu.size();
        let x = (x as f32 - PAD) / cell_w;
        let y = (y as f32 - PAD) / cell_h;
        let col = x.floor().clamp(0., f32::from(cols - 1)) as u16;
        let row = y.floor().clamp(0., f32::from(rows - 1)) as u16;
        let side = if x >= f32::from(cols) || (x >= 0. && x.fract() >= 0.5) { Side::Right } else { Side::Left };
        (row, col, side)
    }

    /// The http(s) link under a cell: an OSC 8 hyperlink, else a URL in the text (following wrapped lines).
    fn url_at(&self, row: u16, col: u16) -> Option<String> {
        let term = self.emu.term();
        let point = self.emu.point_at(row, col);
        // Image anchors carry an empty uri, so they never pass.
        if let Some(link) = term.grid()[point].hyperlink().filter(|link| is_web_url(link.uri())) {
            return Some(link.uri().to_string());
        }
        let mut regex = RegexSearch::new(r#"https?://[^\s<>"'`]+"#).ok()?;
        let (start, end) = (term.line_search_left(point), term.line_search_right(point));
        let found = RegexIter::new(start, end, Direction::Right, term, &mut regex).find(|range| range.contains(&point))?;
        Some(trim_url(&term.bounds_to_string(*found.start(), *found.end())).to_string()).filter(|url| is_web_url(url))
    }

    /// Mouse reports go to the program unless Shift forces local selection.
    fn reports(&self, action: MouseAction, modifiers: &Modifiers) -> bool {
        !self.exited && !modifiers.shift && reports_mouse(self.emu.mode(), action)
    }

    fn mouse_down(&mut self, button: MouseButton, at: (f64, f64), clicks: isize, modifiers: &Modifiers) -> PointerEffect {
        let (row, col, side) = self.cell_at(at);
        // Before mouse reporting, so links open inside vim or tmux too.
        if button == MouseButton::Left && modifiers.platform {
            if let Some(url) = self.url_at(row, col) {
                self.open_url = Some(url);
                return PointerEffect { redraw: true, consumed: true };
            }
        }
        let press = MouseAction::Press(button);
        if self.reports(press, modifiers) {
            if let Some(bytes) = mouse_bytes(press, row, col, modifiers, self.emu.mode()) {
                self.send(&bytes);
            }
            self.pointer.held = Some((button, PointerGrab::Report));
            self.pointer.last_cell = Some((row, col));
            return PointerEffect { redraw: false, consumed: true };
        }
        if button != MouseButton::Left {
            return PointerEffect::default();
        }
        let kind = match clicks {
            0 | 1 => SelectionType::Simple,
            2 => SelectionType::Semantic,
            _ => SelectionType::Lines,
        };
        self.emu.start_selection(kind, row, col, side);
        self.pointer.held = Some((button, PointerGrab::Select));
        PointerEffect { redraw: true, consumed: true }
    }

    /// Hover motion only matters to programs that asked for it.
    fn mouse_move(&mut self, at: (f64, f64), modifiers: &Modifiers) -> PointerEffect {
        if self.pointer.held.is_none() && self.reports(MouseAction::Move, modifiers) {
            let (row, col, _) = self.cell_at(at);
            self.report_motion(MouseAction::Move, row, col, modifiers);
        }
        PointerEffect::default()
    }

    fn mouse_drag(&mut self, at: (f64, f64), modifiers: &Modifiers) -> PointerEffect {
        let Some((button, grab)) = self.pointer.held else {
            return PointerEffect::default();
        };
        let (row, col, side) = self.cell_at(at);
        match grab {
            PointerGrab::Select => {
                self.emu.update_selection(row, col, side);
                PointerEffect { redraw: true, consumed: true }
            }
            PointerGrab::Report => {
                self.report_motion(MouseAction::Drag(button), row, col, modifiers);
                PointerEffect { redraw: false, consumed: true }
            }
        }
    }

    /// Motion is reported once per cell, not once per pixel.
    fn report_motion(&mut self, action: MouseAction, row: u16, col: u16, modifiers: &Modifiers) {
        if self.pointer.last_cell == Some((row, col)) {
            return;
        }
        self.pointer.last_cell = Some((row, col));
        if let Some(bytes) = mouse_bytes(action, row, col, modifiers, self.emu.mode()) {
            self.send(&bytes);
        }
    }

    fn mouse_up(&mut self, button: MouseButton, at: (f64, f64), modifiers: &Modifiers) -> PointerEffect {
        let Some((button, grab)) = self.pointer.held.filter(|(held, _)| *held == button) else {
            return PointerEffect::default();
        };
        self.pointer.held = None;
        self.pointer.last_cell = None;
        let (row, col, side) = self.cell_at(at);
        match grab {
            PointerGrab::Select => {
                self.emu.update_selection(row, col, side);
                PointerEffect { redraw: true, consumed: true }
            }
            PointerGrab::Report => {
                if let Some(bytes) = mouse_bytes(MouseAction::Release(button), row, col, modifiers, self.emu.mode()) {
                    self.send(&bytes);
                }
                PointerEffect { redraw: false, consumed: true }
            }
        }
    }

    /// Wheel: report to a program that wants the mouse, send arrow keys on
    /// the alternate screen (less, vim), otherwise walk the scrollback.
    fn scroll_wheel(&mut self, dy: f32, precise: bool, at: (f64, f64), modifiers: &Modifiers) -> PointerEffect {
        let delta = if precise { dy / self.pointer.cell.1 } else { dy };
        self.pointer.wheel_remainder += delta;
        let whole = self.pointer.wheel_remainder.trunc();
        self.pointer.wheel_remainder -= whole;
        let lines = whole as i32;
        let consumed = PointerEffect { redraw: false, consumed: true };
        if lines == 0 {
            return consumed;
        }
        // Positive lines move toward older output.
        let action = if lines > 0 { MouseAction::WheelUp } else { MouseAction::WheelDown };
        let mode = self.emu.mode();
        let count = lines.unsigned_abs().min(20) as usize;
        if self.reports(action, modifiers) {
            let (row, col, _) = self.cell_at(at);
            if let Some(bytes) = mouse_bytes(action, row, col, modifiers, mode) {
                self.send(&bytes.repeat(count));
            }
            return consumed;
        }
        if mode.contains(TermMode::ALT_SCREEN) && mode.contains(TermMode::ALTERNATE_SCROLL) {
            let arrow: &[u8] = match (lines > 0, mode.contains(TermMode::APP_CURSOR)) {
                (true, true) => b"\x1bOA",
                (true, false) => b"\x1b[A",
                (false, true) => b"\x1bOB",
                (false, false) => b"\x1b[B",
            };
            self.send(&arrow.repeat(count));
            return consumed;
        }
        self.emu.scroll_by(lines);
        PointerEffect { redraw: true, consumed: true }
    }

    /// The screen as it should look now.
    fn paint(&mut self) -> TermPaint {
        let [fg, bg, accent] = super::colors();
        let TermFrame { spans, backgrounds, cursor, images } = terminal_frame(&self.emu, &self.placements, fg, bg, accent);
        let (cell_w, cell_h) = self.pointer.cell;
        let at = |col: f32, row: f32| (f64::from(PAD + col * cell_w), f64::from(PAD + row * cell_h));
        let mut rects = backgrounds
            .iter()
            .map(|run| {
                let (x, y) = at(f32::from(run.col), f32::from(run.row));
                (x, y, f64::from(f32::from(run.cols) * cell_w), f64::from(cell_h), run.color)
            })
            .collect::<Vec<_>>();
        if let Some(bar) = &cursor {
            let (x, y) = at(f32::from(bar.col), f32::from(bar.row));
            rects.push(if bar.underline {
                (x, y + f64::from(cell_h - 2.), f64::from(f32::from(bar.cols) * cell_w), 2., bar.color)
            } else {
                (x, y, 2., f64::from(cell_h), bar.color)
            });
        }
        let images = images
            .into_iter()
            .map(|image| {
                let (x, y) = at(f32::from(image.col), image.row);
                (x, y, f64::from(image.size.0), f64::from(image.size.1), image.image)
            })
            .collect();
        let spans = spans
            .into_iter()
            .map(|span| {
                let (x, y) = at(f32::from(span.col), f32::from(span.row));
                TermSpan { x, y, text: span.text, font: span.font, color: span.color, underline: span.underline, strike: span.strike }
            })
            .collect();
        TermPaint { font: self.font.clone(), size: f64::from(self.font_size), line_h: f64::from(cell_h), background: bg, rects, spans, images }
    }
}

/// `~/dev/app` for a tab label.
pub(super) fn short_path(dir: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match home.as_deref().and_then(|home| dir.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => dir.display().to_string(),
    }
}

/// Longest title kept; the tab ellipsizes visually anyway.
const TITLE_MAX: usize = 60;

/// A title a program asked for, made safe to show: control characters dropped, length bounded.
fn clean_title(title: &str) -> Option<String> {
    let title: String = title.chars().filter(|c| !c.is_control()).take(TITLE_MAX).collect();
    let title = title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Bytes for a newline that does not submit the line. Bracketed paste
/// inserts the newline into shells that enable it; otherwise ESC CR is
/// the sequence fish and agent CLIs bind as newline.
pub fn newline_bytes(bracketed_paste: bool) -> Vec<u8> {
    if bracketed_paste { paste_bytes("\n", true) } else { b"\x1b\r".to_vec() }
}

/// Wrap pasted text the way the line editor expects.
/// Shell-escaped paths, space separated with a trailing space so typing can continue.
fn dropped_paths(paths: &[std::path::PathBuf]) -> String {
    let mut out = String::new();
    for path in paths {
        for ch in path.to_string_lossy().chars() {
            if !(ch.is_alphanumeric() || "/._-+,@%:".contains(ch)) {
                out.push('\\');
            }
            out.push(ch);
        }
        out.push(' ');
    }
    out
}

fn paste_bytes(text: &str, bracketed_paste: bool) -> Vec<u8> {
    // An end marker inside the text would end the paste early and run what follows as typed commands.
    if bracketed_paste { format!("\x1b[200~{}\x1b[201~", text.replace("\x1b[201~", "")).into_bytes() } else { text.as_bytes().to_vec() }
}

struct TermBgRun {
    row: u16,
    col: u16,
    cols: u16,
    color: Rgba,
}

struct GlyphSpan {
    row: u16,
    col: u16,
    text: String,
    /// 0 regular, 1 bold, 2 italic, 3 bold italic.
    font: usize,
    color: Rgba,
    /// 0 none, 1 straight, 2 wavy.
    underline: u8,
    strike: bool,
}

/// A cursor drawn as a thin bar; a block cursor is a colour swap on its cell.
struct TermCursor {
    row: u16,
    col: u16,
    cols: u16,
    underline: bool,
    color: Rgba,
}

/// An image to draw; `row` may be negative when its top has scrolled above the viewport.
struct ImageDraw {
    row: f32,
    col: u16,
    size: (f32, f32),
    image: Rc<TermImage>,
}

struct TermFrame {
    spans: Vec<GlyphSpan>,
    backgrounds: Vec<TermBgRun>,
    cursor: Option<TermCursor>,
    images: Vec<ImageDraw>,
}

struct OpenSpan {
    col: u16,
    text: String,
    font_index: usize,
    color: Rgba,
    /// 0 none, 1 straight, 2 wavy.
    underline: u8,
    strike: bool,
}

fn flush_span(open: &mut Option<OpenSpan>, row: u16, spans: &mut Vec<GlyphSpan>) {
    let Some(open) = open.take().filter(|open| !open.text.is_empty()) else {
        return;
    };
    spans.push(GlyphSpan { row, col: open.col, text: open.text, font: open.font_index, color: open.color, underline: open.underline, strike: open.strike });
}

fn push_bg(runs: &mut Vec<TermBgRun>, row: u16, col: u16, cols: u16, color: Rgba, default_bg: Rgba) {
    if color == default_bg {
        return;
    }
    if let Some(last) = runs.last_mut() {
        if last.row == row && last.col + last.cols == col && last.color == color {
            last.cols += cols;
            return;
        }
    }
    runs.push(TermBgRun { row, col, cols, color });
}

/// Visible cells grouped into background runs and positioned text spans, plus the images anchored in them.
///
/// A shell row is a handful of spans, each placed on its column, instead of
/// one element per cell.
fn terminal_frame(emu: &Emulator, placements: &[Placement], fg_default: Rgba, bg_default: Rgba, accent: Rgba) -> TermFrame {
    let content = emu.term().renderable_content();
    let offset = content.display_offset as i32;
    let colors = content.colors;
    let selection = content.selection;
    let cursor = content.cursor;
    let cursor_cell = (offset == 0 && content.mode.contains(TermMode::SHOW_CURSOR) && cursor.shape != CursorShape::Hidden)
        .then_some((cursor.point.line.0 as u16, cursor.point.column.0 as u16));
    let mut spans = Vec::new();
    let mut backgrounds = Vec::new();
    let mut images: Vec<ImageDraw> = Vec::new();
    let mut seen = Vec::new();
    let mut bar = None;
    let mut open: Option<OpenSpan> = None;
    let mut open_row = 0u16;
    for indexed in content.display_iter {
        let Ok(row) = u16::try_from(indexed.point.line.0 + offset) else {
            continue;
        };
        let col = indexed.point.column.0 as u16;
        if row != open_row {
            flush_span(&mut open, open_row, &mut spans);
            open_row = row;
        }
        let cell = indexed.cell;
        // The first visible row of an image says where its top is, even when that is above the viewport.
        if let Some((key, image_row)) = cell.hyperlink().and_then(|link| anchor(link.id())) {
            if !seen.contains(&key) {
                seen.push(key);
                if let Some(placement) = placements.iter().find(|placement| placement.key == key) {
                    images.push(ImageDraw { row: f32::from(row) - image_row as f32, col, size: placement.size, image: placement.image.clone() });
                }
            }
        }
        let flags = cell.flags;
        if flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        let width: u16 = if flags.contains(Flags::WIDE_CHAR) { 2 } else { 1 };
        let mut fg = terminal_color(cell.fg, colors, fg_default, bg_default);
        let mut bg = terminal_color(cell.bg, colors, fg_default, bg_default);
        if flags.contains(Flags::DIM) {
            fg = dimmed(fg, bg);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cursor_cell == Some((row, col)) {
            match cursor.shape {
                CursorShape::Beam | CursorShape::Underline => {
                    bar = Some(TermCursor { row, col, cols: width, underline: cursor.shape == CursorShape::Underline, color: fg_default });
                }
                _ => std::mem::swap(&mut fg, &mut bg),
            }
        }
        if selection.is_some_and(|range| range.contains(indexed.point)) {
            bg = accent;
            fg = bg_default;
        }
        push_bg(&mut backgrounds, row, col, width, bg, bg_default);
        let underline = if flags.contains(Flags::UNDERCURL) {
            2
        } else if flags.intersects(Flags::ALL_UNDERLINES) {
            1
        } else {
            0
        };
        let strike = flags.contains(Flags::STRIKEOUT);
        let blank = flags.contains(Flags::HIDDEN) || ((cell.c == ' ' || cell.c == '\0') && underline == 0 && !strike);
        if blank {
            flush_span(&mut open, row, &mut spans);
            continue;
        }
        let font_index = match (flags.contains(Flags::BOLD), flags.contains(Flags::ITALIC)) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        };
        let continues = open
            .as_ref()
            .is_some_and(|span| span.font_index == font_index && span.color == fg && span.underline == underline && span.strike == strike);
        if !continues {
            flush_span(&mut open, row, &mut spans);
            open = Some(OpenSpan { col, text: String::new(), font_index, color: fg, underline, strike });
        }
        if let Some(span) = open.as_mut() {
            span.text.push(if cell.c == '\0' { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth() {
                span.text.extend(extra.iter());
            }
        }
        if width == 2 {
            flush_span(&mut open, row, &mut spans);
        }
    }
    flush_span(&mut open, open_row, &mut spans);
    TermFrame { spans, backgrounds, cursor: bar, images }
}

/// Terminal output is untrusted: only web links open, never `file:` or `javascript:`.
fn is_web_url(url: &str) -> bool {
    url.starts_with("https://") || url.starts_with("http://")
}

/// Drops sentence punctuation after a URL, and a closing bracket the URL didn't open: `(see https://a.b/c).`
fn trim_url(mut url: &str) -> &str {
    loop {
        let unopened = |close: char, open: char| url.ends_with(close) && url.matches(close).count() > url.matches(open).count();
        if url.ends_with(['.', ',', ';', ':', '!', '?', '\'', '"']) || unopened(')', '(') || unopened(']', '[') {
            url = &url[..url.len() - 1];
        } else {
            return url;
        }
    }
}

/// `(placement key, row within the image)` from an anchor cell's hyperlink id.
fn anchor(id: &str) -> Option<(u64, usize)> {
    let (key, row) = id.strip_prefix(ANCHOR)?.split_once(':')?;
    Some((key.parse().ok()?, row.parse().ok()?))
}

/// A colour faded toward the background, for SGR 2 and the dim palette.
fn dimmed(color: Rgba, bg: Rgba) -> Rgba {
    std::array::from_fn(|i| if i == 3 { 1. } else { color[i] * 0.6 + bg[i] * 0.4 })
}

fn rgb_color(rgb: Rgb) -> Rgba {
    hex((rgb.r as u32) << 16 | (rgb.g as u32) << 8 | rgb.b as u32)
}

/// `0xRRGGBB`, opaque.
fn hex(value: u32) -> Rgba {
    let channel = |shift: u32| f64::from((value >> shift) & 0xff) / 255.;
    [channel(16), channel(8), channel(0), 1.]
}

fn terminal_color(color: AnsiColor, colors: &Colors, fg_default: Rgba, bg_default: Rgba) -> Rgba {
    match color {
        AnsiColor::Spec(rgb) => rgb_color(rgb),
        AnsiColor::Indexed(index) => colors[usize::from(index)].map(rgb_color).unwrap_or_else(|| indexed_color(index)),
        AnsiColor::Named(named) => {
            // A program can repaint any slot with OSC 4/10/11.
            if let Some(rgb) = colors[named] {
                return rgb_color(rgb);
            }
            match named {
                NamedColor::Foreground | NamedColor::BrightForeground | NamedColor::Cursor => fg_default,
                NamedColor::DimForeground => dimmed(fg_default, bg_default),
                NamedColor::Background => bg_default,
                other => {
                    let slot = other as usize;
                    if slot < 16 {
                        indexed_color(slot as u8)
                    } else {
                        // Dim black..white follow the 8 base colours.
                        dimmed(indexed_color((slot - NamedColor::DimBlack as usize) as u8), bg_default)
                    }
                }
            }
        }
    }
}

/// The xterm 256-colour palette: 16 ANSI, a 6x6x6 cube, then a grey ramp.
fn indexed_color(index: u8) -> Rgba {
    const PALETTE: [u32; 16] = [
        0x111111, 0xcc5555, 0x55aa55, 0xaaaa55, 0x5555cc, 0xaa55aa, 0x55aaaa, 0xcccccc, 0x555555, 0xff7777, 0x77ff77, 0xffff77, 0x7777ff, 0xff77ff, 0x77ffff,
        0xffffff,
    ];
    let value = if index < 16 {
        PALETTE[index as usize]
    } else if index < 232 {
        let n = index as u32 - 16;
        let component = |v| if v == 0 { 0 } else { 55 + 40 * v };
        component(n / 36) << 16 | component(n / 6 % 6) << 8 | component(n % 6)
    } else {
        let v = 8 + (index as u32 - 232) * 10;
        v << 16 | v << 8 | v
    };
    hex(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_punctuation_after_urls() {
        assert_eq!(trim_url("https://a.b/c)."), "https://a.b/c");
        assert_eq!(trim_url("https://en.wikipedia.org/wiki/Rust_(language)"), "https://en.wikipedia.org/wiki/Rust_(language)");
        assert_eq!(trim_url("https://a.b/?q=1\","), "https://a.b/?q=1");
        assert!(!is_web_url("file:///etc/passwd") && !is_web_url("javascript:alert(1)"));
    }

    #[test]
    fn dropped_paths_are_shell_escaped() {
        let paths = [std::path::PathBuf::from("/tmp/Screen Shot (1).png"), std::path::PathBuf::from("/a/b.jpg")];
        assert_eq!(dropped_paths(&paths), "/tmp/Screen\\ Shot\\ \\(1\\).png /a/b.jpg ");
    }

    #[test]
    fn paste_cannot_end_bracketed_mode_early() {
        assert_eq!(paste_bytes("a\x1b[201~rm -rf ~\n", true), b"\x1b[200~arm -rf ~\n\x1b[201~");
        assert_eq!(paste_bytes("a", false), b"a");
    }

    #[test]
    fn anchors_scroll_with_text_after_history_is_full() {
        let mut emu = Emulator::new(3, 10, 5);
        emu.feed(b"top\r\n");
        let link = Hyperlink::new(Some(format!("{ANCHOR}1:0")), String::new());
        emu.term_mut().grid_mut()[Point::new(alacritty_terminal::index::Line(0), alacritty_terminal::index::Column(2))].set_hyperlink(Some(link));
        // More lines than screen + history: the anchor rotates out, as its image should.
        for i in 0..4 {
            emu.feed(format!("l{i}\r\n").as_bytes());
        }
        emu.scroll_by(10);
        let found = |emu: &Emulator| emu.term().renderable_content().display_iter.any(|cell| cell.hyperlink().is_some_and(|link| link.id() == "nxkitty:1:0"));
        assert!(found(&emu), "anchor kept while in history");
        for i in 0..10 {
            emu.feed(format!("m{i}\r\n").as_bytes());
        }
        emu.scroll_by(100);
        assert!(!found(&emu), "anchor dropped with its line");
    }

    #[test]
    fn anchor_ids_round_trip() {
        assert_eq!(anchor("nxkitty:12:3"), Some((12, 3)));
        assert_eq!(anchor("other:1:2"), None);
    }
}
