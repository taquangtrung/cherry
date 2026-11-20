//! A GUI host for Cherry on egui: the editor in a window, painted cell by
//! cell in a monospace font, with the system clipboard and the mouse.
//!
//! Run it with `cargo run -p cherry-egui -- <file>` (or `make gui`). It
//! opens the file, or an empty buffer when the file does not exist yet.
//! `CHERRY_STYLE` in the environment picks the editing style (`vim`,
//! `emacs`, or `standard`; Cherry's own otherwise). A window passes every
//! chord on as typed, so Ctrl+Shift+Z and `C-SPC` reach the editor here
//! even where a terminal cannot send them.
//!
//! The host's whole job is here: turn egui's key, text, clipboard, and
//! pointer events into Cherry keys and mouse events, tick the editor when
//! a half-typed sequence times out, paint the surface the editor renders,
//! and act on the editor's requests (save, quit, notice).

use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cherry::keys::{Key, KeyCode, Mods, Timestamp};
use cherry::{
    ClickCounter, Clipboard, ClipboardSelection, Cursor, CursorShape, DocumentId, EditingStyle,
    Editor, EditorOptions, HostRequest, Mode, MouseEvent, MouseEventKind, Style, Surface, ViewId,
    render_prompt, render_view,
};
use eframe::egui::{
    self, Align2, Color32, Event, FontId, Modifiers, PointerButton, Pos2, Rect, Sense, Vec2,
    ViewportCommand,
};

// ========================================================================
// Constants
// ========================================================================

/// The environment variable that picks the editing style.
const STYLE_VARIABLE: &str = "CHERRY_STYLE";

/// The values of [`STYLE_VARIABLE`] that pick a style other than Cherry's.
const STYLES: [(&str, EditingStyle); 3] = [
    ("emacs", EditingStyle::Emacs),
    ("standard", EditingStyle::Standard),
    ("vim", EditingStyle::Vim),
];

/// The size of the text, in points.
const FONT_SIZE: f32 = 15.0;

/// The height of a row as a share of the font size.
const LINE_SPACING: f32 = 1.25;

/// The width of the bar cursor, in points.
const BAR_WIDTH: f32 = 2.0;

/// The longest two presses on one cell may be apart to count as one click.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

/// The pixels of wheel motion that scroll the view one notch.
const SCROLL_NOTCH: f32 = 40.0;

/// The colors the host paints with.
const BACKGROUND: Color32 = Color32::from_rgb(0x1e, 0x1f, 0x24);
const CURSOR_LINE: Color32 = Color32::from_rgb(0x2a, 0x2b, 0x31);
const FOREGROUND: Color32 = Color32::from_rgb(0xd4, 0xd4, 0xd4);
const FAINT: Color32 = Color32::from_rgb(0x6e, 0x70, 0x78);
const MATCH: Color32 = Color32::from_rgb(0x61, 0x4a, 0x1c);
const SELECTION: Color32 = Color32::from_rgb(0x26, 0x4f, 0x78);
const STATUS: Color32 = Color32::from_rgb(0x32, 0x34, 0x3c);

/// The characters a US keyboard types with Shift on the key of each of
/// these, so Ctrl+Shift+[ reaches the editor as `C-{`.
const SHIFTED: [(char, char); 21] = [
    ('1', '!'),
    ('2', '@'),
    ('3', '#'),
    ('4', '$'),
    ('5', '%'),
    ('6', '^'),
    ('7', '&'),
    ('8', '*'),
    ('9', '('),
    ('0', ')'),
    ('-', '_'),
    ('=', '+'),
    ('[', '{'),
    (']', '}'),
    ('\\', '|'),
    (';', ':'),
    ('\'', '"'),
    (',', '<'),
    ('.', '>'),
    ('/', '?'),
    ('`', '~'),
];

// ========================================================================
// Data Structures
// ========================================================================

/// The editor in a window.
struct Host {
    /// Turns quick presses on one cell into double and triple clicks.
    clicks: ClickCounter,
    /// The clipboard the editor reads and writes, shared with the host.
    clipboard: SharedClipboard,
    /// The document being edited.
    document: DocumentId,
    /// The editor.
    editor: Editor,
    /// Whether the primary button is held over the text.
    dragging: bool,
    /// The last notice the editor gave.
    notice: String,
    /// The file the document is saved to, if any.
    path: Option<PathBuf>,
    /// What the prompt line paints, when a prompt is open.
    prompt_surface: Surface,
    /// When the host started, the origin of every timestamp.
    started: Instant,
    /// What the view paints.
    surface: Surface,
    /// The view onto the document.
    view: ViewId,
}

/// A clipboard the editor and the host share: the editor writes the text
/// for the host to put on the system clipboard, and reads back what the
/// host last pasted or what it wrote itself.
#[derive(Clone, Default)]
struct SharedClipboard(Arc<Mutex<ClipboardText>>);

/// The text behind [`SharedClipboard`].
#[derive(Default)]
struct ClipboardText {
    /// The text the editor wrote, for the host to put on the system
    /// clipboard at the end of the frame.
    outgoing: Option<String>,
    /// The text the clipboard holds.
    text: Option<String>,
}

/// The size of a cell, in points.
#[derive(Clone, Copy)]
struct CellSize {
    /// The height of a row.
    height: f32,
    /// The width of a column.
    width: f32,
}

// ========================================================================
// Clipboard Implementations
// ========================================================================

impl Clipboard for SharedClipboard {
    fn read(&mut self, _selection: ClipboardSelection) -> Option<String> {
        self.0.lock().ok()?.text.clone()
    }

    fn write(&mut self, _selection: ClipboardSelection, text: &str) {
        if let Ok(mut held) = self.0.lock() {
            held.text = Some(text.to_string());
            held.outgoing = Some(text.to_string());
        }
    }
}

impl SharedClipboard {
    /// Holds `text`, pasted from the system clipboard, for the editor to
    /// read.
    fn hold(&self, text: String) {
        if let Ok(mut held) = self.0.lock() {
            held.text = Some(text);
        }
    }

    /// Takes the text the editor wrote since the last call.
    fn take_outgoing(&self) -> Option<String> {
        self.0.lock().ok()?.outgoing.take()
    }
}

// ========================================================================
// Host Implementations
// ========================================================================

impl Host {
    /// A host editing the file at `path`, or an empty buffer when there is
    /// no path or no file there yet.
    fn open(path: Option<PathBuf>) -> Self {
        let text = path
            .as_ref()
            .and_then(|path| fs::read_to_string(path).ok())
            .unwrap_or_default();
        let options = EditorOptions {
            auto_pairs: true,
            editing_style: editing_style_from_env(),
            line_numbers: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let clipboard = SharedClipboard::default();
        editor.set_clipboard(clipboard.clone());
        let document = editor.open_document(&text);
        editor.mark_saved(document);
        let view = editor.new_view(document).expect("the document is open");
        Self {
            clicks: ClickCounter::new(DOUBLE_CLICK),
            clipboard,
            document,
            dragging: false,
            editor,
            notice: String::new(),
            path,
            prompt_surface: Surface::new(0, 0),
            started: Instant::now(),
            surface: Surface::new(0, 0),
            view,
        }
    }

    /// The time now, from the host's clock.
    fn now(&self) -> Timestamp {
        Timestamp::from_duration(self.started.elapsed())
    }

    /// Feeds `key` to the editor.
    fn feed(&mut self, key: Key) {
        self.notice.clear();
        let now = self.now();
        self.editor.handle_key(self.view, key, now);
    }

    /// Feeds the keyboard events of the frame to the editor.
    fn take_keys(&mut self, events: &[Event]) {
        for event in events {
            match event {
                Event::Copy => self.feed(Key::new(KeyCode::Char('c'), Mods::CTRL)),
                Event::Cut => self.feed(Key::new(KeyCode::Char('x'), Mods::CTRL)),
                Event::Paste(text) => {
                    self.clipboard.hold(text.clone());
                    self.feed(Key::new(KeyCode::Char('v'), Mods::CTRL));
                }
                Event::Text(text) => {
                    for c in text.chars() {
                        self.feed(Key::new(KeyCode::Char(c), Mods::NONE));
                    }
                }
                Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(key) = map_key(*key, *modifiers) {
                        self.feed(key);
                    }
                }
                _ => {}
            }
        }
    }

    /// Feeds the pointer events of the frame over the text in `rect` to the
    /// editor, as cells of `cell` size.
    fn take_pointer(&mut self, events: &[Event], rect: Rect, cell: CellSize, scroll: f32) {
        let cell_at = |pos: Pos2| {
            let column = ((pos.x - rect.left()) / cell.width).max(0.0) as u16;
            let row = ((pos.y - rect.top()) / cell.height).max(0.0) as u16;
            (column, row.min(self.surface.rows().saturating_sub(1)))
        };
        for event in events {
            let (kind, pos, modifiers) = match event {
                Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers,
                } => {
                    if *pressed && !rect.contains(*pos) {
                        continue;
                    }
                    self.dragging = *pressed;
                    let kind = match pressed {
                        true => MouseEventKind::Press,
                        false => MouseEventKind::Release,
                    };
                    (kind, *pos, *modifiers)
                }
                Event::PointerMoved(pos) if self.dragging => {
                    (MouseEventKind::Drag, *pos, Modifiers::NONE)
                }
                _ => continue,
            };
            let (column, row) = cell_at(pos);
            let mut mouse = MouseEvent::new(kind, column, row);
            if modifiers.shift {
                mouse = mouse.with_mods(Mods::SHIFT);
            }
            let now = self.now();
            let counted = self.clicks.count(mouse, now);
            self.editor.handle_mouse(self.view, counted);
        }
        let notches = (scroll / SCROLL_NOTCH).round() as i32;
        let kind = match notches > 0 {
            true => MouseEventKind::ScrollUp,
            false => MouseEventKind::ScrollDown,
        };
        for _ in 0..notches.unsigned_abs() {
            self.editor
                .handle_mouse(self.view, MouseEvent::new(kind, 0, 0));
        }
    }

    /// Acts on what the editor asked since the last frame.
    fn take_requests(&mut self, ctx: &egui::Context) {
        for request in self.editor.take_requests() {
            match request {
                HostRequest::Notice(notice) => self.notice = notice.message,
                HostRequest::Quit(_) => ctx.send_viewport_cmd(ViewportCommand::Close),
                HostRequest::Save(save) => self.save(save.document),
                _ => {}
            }
        }
        if let Some(text) = self.clipboard.take_outgoing() {
            ctx.copy_text(text);
        }
    }

    /// Writes `document` to the file, with the line ending it came with,
    /// and tells the editor it is saved.
    fn save(&mut self, document: DocumentId) {
        let Some(path) = &self.path else {
            self.notice = "No file name to save to".to_string();
            return;
        };
        let Some(text) = self
            .editor
            .document(document)
            .map(cherry::Document::text_with_line_endings)
        else {
            return;
        };
        self.notice = match fs::write(path, text) {
            Ok(()) => {
                self.editor.mark_saved(document);
                format!("\"{}\" written", path.display())
            }
            Err(err) => format!("Cannot write \"{}\": {err}", path.display()),
        };
    }

    /// The status line: the mode, whether the text has unsaved changes,
    /// the keys half-typed, and the last notice.
    fn status(&self) -> String {
        let Some(view) = self.editor.view(self.view) else {
            return String::new();
        };
        let mode = match view.mode() {
            Mode::CommandLine => "COMMAND",
            Mode::Insert => "INSERT",
            Mode::Normal => "NORMAL",
            Mode::OperatorPending => "OPERATOR",
            Mode::Replace => "REPLACE",
            Mode::Visual => "VISUAL",
            Mode::VisualBlock => "V-BLOCK",
            Mode::VisualLine => "V-LINE",
            _ => "",
        };
        let modified = match self.editor.is_modified(self.document) {
            true => "[+] ",
            false => "",
        };
        let pending: Vec<_> = view.pending_keys().iter().map(Key::to_string).collect();
        format!(" {mode} {modified}{} {}", pending.join(" "), self.notice)
    }
}

impl eframe::App for Host {
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        let font = FontId::monospace(FONT_SIZE);
        let cell = CellSize {
            height: FONT_SIZE * LINE_SPACING,
            width: ctx.fonts_mut(|fonts| fonts.glyph_width(&font, 'M')),
        };
        let (events, scroll) =
            ctx.input(|input| (input.events.clone(), input.smooth_scroll_delta.y));
        self.take_keys(&events);
        egui::CentralPanel::default()
            .frame(egui::Frame::NONE.fill(BACKGROUND))
            .show(root, |ui| {
                let rect = ui.max_rect();
                ui.allocate_rect(rect, Sense::click_and_drag());
                let cols = (rect.width() / cell.width).floor().max(1.0) as u16;
                let rows = (rect.height() / cell.height).floor().max(2.0) as u16 - 1;
                self.editor.resize_view(self.view, cols, rows);
                self.take_pointer(&events, rect, cell, scroll);
                let cursor = render_view(&self.editor, self.view, &mut self.surface);
                let painter = ui.painter();
                paint_surface(painter, &self.surface, rect.min, cell, &font);
                let status_top = rect.min + Vec2::new(0.0, f32::from(rows) * cell.height);
                let prompt_cursor =
                    render_prompt(&self.editor, self.view, &mut self.prompt_surface);
                let cursor = match prompt_cursor {
                    Some(prompt_cursor) => {
                        paint_surface(painter, &self.prompt_surface, status_top, cell, &font);
                        Some(Cursor {
                            row: prompt_cursor.row + rows,
                            ..prompt_cursor
                        })
                    }
                    None => {
                        let status =
                            Rect::from_min_size(status_top, Vec2::new(rect.width(), cell.height));
                        painter.rect_filled(status, 0.0, STATUS);
                        painter.text(
                            status.min,
                            Align2::LEFT_TOP,
                            self.status(),
                            font.clone(),
                            FOREGROUND,
                        );
                        cursor
                    }
                };
                if let Some(cursor) = cursor {
                    paint_cursor(painter, cursor, rect.min, cell);
                }
            });
        self.take_requests(&ctx);
        let now = self.now();
        if let Some(deadline) = self.editor.deadline() {
            if self.editor.tick(now) {
                ctx.request_repaint();
            }
            ctx.request_repaint_after(deadline.saturating_duration_since(now));
        }
    }
}

// ========================================================================
// Functions
// ========================================================================

fn main() -> eframe::Result {
    let path = env::args_os().nth(1).map(PathBuf::from);
    let title = path.as_ref().map_or_else(
        || "Cherry".to_string(),
        |path| format!("{} - Cherry", path.display()),
    );
    eframe::run_native(
        &title,
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::new(Host::open(path)))),
    )
}

/// The editing style [`STYLE_VARIABLE`] names in [`STYLES`], else Cherry's.
fn editing_style_from_env() -> EditingStyle {
    let named = env::var(STYLE_VARIABLE).unwrap_or_default();
    STYLES
        .iter()
        .find(|(name, _)| *name == named)
        .map_or(EditingStyle::Cherry, |(_, style)| *style)
}

/// The Cherry key for an egui key pressed with `modifiers`, or `None` for
/// a key that types text, which arrives as a text event instead.
fn map_key(key: egui::Key, modifiers: Modifiers) -> Option<Key> {
    use egui::Key as K;
    let mut mods = Mods::NONE;
    if modifiers.ctrl || modifiers.mac_cmd {
        mods |= Mods::CTRL;
    }
    if modifiers.alt {
        mods |= Mods::ALT;
    }
    let shift = match modifiers.shift {
        true => Mods::SHIFT,
        false => Mods::NONE,
    };
    let code = match key {
        K::ArrowDown => KeyCode::Down,
        K::ArrowLeft => KeyCode::Left,
        K::ArrowRight => KeyCode::Right,
        K::ArrowUp => KeyCode::Up,
        K::Backspace => KeyCode::Backspace,
        K::Delete => KeyCode::Delete,
        K::End => KeyCode::End,
        K::Enter => KeyCode::Enter,
        K::Escape => KeyCode::Esc,
        K::Home => KeyCode::Home,
        K::Insert => KeyCode::Insert,
        K::PageDown => KeyCode::PageDown,
        K::PageUp => KeyCode::PageUp,
        K::Tab => KeyCode::Tab,
        _ => {
            if !mods.intersects(Mods::CTRL | Mods::ALT) {
                return None;
            }
            let symbol = key.symbol_or_name().chars().next()?;
            let lower = symbol.to_ascii_lowercase();
            let shifted = SHIFTED.iter().find(|(plain, _)| *plain == lower);
            match (modifiers.shift, shifted) {
                (true, Some((_, shifted))) => return Some(Key::new(KeyCode::Char(*shifted), mods)),
                (_, _) if key == K::Space => KeyCode::Char(' '),
                (_, _) => KeyCode::Char(lower),
            }
        }
    };
    Some(Key::new(code, mods | shift))
}

/// Paints every cell of `surface` with its top left corner at `origin`.
fn paint_surface(
    painter: &egui::Painter,
    surface: &Surface,
    origin: Pos2,
    cell: CellSize,
    font: &FontId,
) {
    for row in 0..surface.rows() {
        for column in 0..surface.cols() {
            let Some(shown) = surface.cell(column, row) else {
                continue;
            };
            let at =
                origin + Vec2::new(f32::from(column) * cell.width, f32::from(row) * cell.height);
            let (foreground, background) = colors(shown.style);
            if let Some(background) = background {
                painter.rect_filled(
                    Rect::from_min_size(at, Vec2::new(cell.width, cell.height)),
                    0.0,
                    background,
                );
            }
            if !shown.symbol.is_empty() && shown.symbol != " " {
                painter.text(
                    at,
                    Align2::LEFT_TOP,
                    &shown.symbol,
                    font.clone(),
                    foreground,
                );
            }
        }
    }
}

/// The text color and the background, if any, of a cell's `style`.
fn colors(style: Style) -> (Color32, Option<Color32>) {
    match style {
        Style::CursorLine => (FOREGROUND, Some(CURSOR_LINE)),
        Style::Folded | Style::Inlay | Style::LineNumber => (FAINT, None),
        Style::SearchMatch => (FOREGROUND, Some(MATCH)),
        Style::Selection => (FOREGROUND, Some(SELECTION)),
        _ => (FOREGROUND, None),
    }
}

/// Paints `cursor` in the shape it asks for.
fn paint_cursor(painter: &egui::Painter, cursor: Cursor, origin: Pos2, cell: CellSize) {
    let at = origin
        + Vec2::new(
            f32::from(cursor.column) * cell.width,
            f32::from(cursor.row) * cell.height,
        );
    let size = match cursor.shape {
        CursorShape::Bar => Vec2::new(BAR_WIDTH, cell.height),
        CursorShape::Underline => Vec2::new(cell.width, BAR_WIDTH),
        _ => Vec2::new(cell.width, cell.height),
    };
    let top = match cursor.shape {
        CursorShape::Underline => at + Vec2::new(0.0, cell.height - BAR_WIDTH),
        _ => at,
    };
    painter.rect_filled(
        Rect::from_min_size(top, size),
        0.0,
        FOREGROUND.gamma_multiply(0.6),
    );
}
