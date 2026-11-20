//! A terminal host for Cherry, built on crossterm: the smallest complete
//! embedding of the editor.
//!
//! Run it with `cargo run --example term -- <file>`. It opens the file, or
//! an empty buffer when the file does not exist yet, in the alternate
//! screen. `CHERRY_STYLE` in the environment picks the editing style:
//! `vim`, `emacs` (no modes; `C-x C-s` saves, `C-x C-c` quits), or
//! `standard` (no modes: Shift selects, Ctrl+C, X, V, Z, and Y copy, cut,
//! paste, undo, and redo, Ctrl+S saves, and Ctrl+Q quits); Cherry's own
//! style otherwise. `Z Z` (or `:wq`, `:x`) saves and quits, `Z Q` (or `:q!`)
//! quits without saving, `:q` quits unless there are unsaved changes,
//! `:w` saves, and `C-g`
//! (a command this host registers, see [`register_host_commands`]) shows
//! the file name and where the cursor is; `:trailing` marks trailing
//! blanks as warnings with signs (`:trailing!` clears them), as a language
//! server's diagnostics would be. Brackets and quotes type in pairs, new
//! lines keep their indent, and `gc` comments with the token of the file's
//! extension (block comments for CSS, HTML, and XML). Rust, Python, JSON, TOML, and Markdown files are
//! highlighted by the `syntax` module, with `af`/`if` and `ac`/`ic` selecting
//! functions and classes. The mouse works too: click to move, drag to select, double
//! and triple click for a word or a line, the wheel to scroll. The bottom
//! row shows the mode (Replace mode with an underline cursor, visual block
//! mode as `V-BLOCK`),
//! `[+]` when the text has unsaved changes, `[3/17]` for the search match the
//! cursor is at, the register keys are being
//! recorded into, the selected register, any half-typed keys, and the
//! editor's last notice; while a prompt is open (`/` and `?` search, `:` ex commands),
//! it shows the prompt instead, with the cursor in it. In Insert mode,
//! `C-x C-n`, `C-x C-p`, and `C-x C-l` open a completion menu painted
//! below the word (`C-n` and `C-p` pick, `C-g` cancels). `M-n` adds a
//! cursor at the next occurrence of the word under the cursor (`M-x`
//! skips it, `M-p` drops it), `M-j` and `M-k` add one on the line below
//! or above, and every key then works at each cursor.
//!
//! The host's whole job is here: turn crossterm key events into
//! [`cherry_engine::keys::Key`]s, feed them to the editor with the time, tick the
//! editor when a half-typed sequence times out, paint the surface the
//! editor renders, and act on the editor's requests (save, quit, notice).
//! The terminal is restored on exit and on panic.

use std::env;
use std::fs;
use std::io::{self, Stdout, Write};
use std::panic;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use cherry_engine::keys::{Key, KeyCode, Mods, Timestamp};
use cherry_engine::syntax::{Language, SyntaxHighlighter, highlight_name};
use cherry_engine::{
    Cell, ClickCounter, CommandContext, CommandError, Cursor, CursorShape, Decoration,
    DecorationKind, Document, DocumentId, EditingStyle, Editor, EditorOptions, ExContext,
    HostRequest, Mode, MouseEvent, MouseEventKind, ReadIntoRequest, Sign, Style, Surface, ViewId,
    block_comment_tokens_for_extension, comment_token_for_extension, render_prompt, render_view,
};
use crossterm::cursor::{Hide, MoveTo, SetCursorStyle, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyEvent, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::style::{
    Attribute, Color, Print, ResetColor, SetAttribute, SetBackgroundColor, SetForegroundColor,
    SetUnderlineColor,
};
use crossterm::terminal::{self, Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{execute, queue};

// ========================================================================
// Constants
// ========================================================================

/// The environment variable that picks the editing style.
const STYLE_VARIABLE: &str = "CHERRY_STYLE";

/// The values of [`STYLE_VARIABLE`] that pick an editing style other
/// than Cherry's.
const STYLES: [(&str, EditingStyle); 3] = [
    ("emacs", EditingStyle::Emacs),
    ("standard", EditingStyle::Standard),
    ("vim", EditingStyle::Vim),
];

/// The background of the line the cursor is on.
const CURSOR_LINE_BACKGROUND: Color = Color::AnsiValue(236);

/// The foreground of text laid into a line that is not its own.
const INLAY_FOREGROUND: Color = Color::DarkGrey;

/// The foreground of the line number gutter.
const LINE_NUMBER_FOREGROUND: Color = Color::DarkGrey;

/// The background of the bracket at the cursor and the one matching it.
const MATCHING_BRACKET_BACKGROUND: Color = Color::AnsiValue(239);

/// The background of text matching the last search.
const SEARCH_MATCH_BACKGROUND: Color = Color::AnsiValue(94);

/// The background of the cell an extra cursor is on.
const EXTRA_CURSOR_BACKGROUND: Color = Color::AnsiValue(250);

/// The keys this host binds to Cherry's commands for several cursors.
const CURSOR_KEYS: &[(&str, &str)] = &[
    ("M-j", "add-cursor-below"),
    ("M-k", "add-cursor-above"),
    ("M-n", "add-cursor-next-match"),
    ("M-p", "remove-cursor-last"),
    ("M-x", "skip-cursor-next-match"),
];

/// The background of selected text.
const SELECTION_BACKGROUND: Color = Color::AnsiValue(24);

/// The background of text a decoration highlights.
const HIGHLIGHT_BACKGROUND: Color = Color::AnsiValue(58);

/// The owner name of the decorations `:trailing` sets.
const TRAILING_OWNER: &str = "trailing";

/// The id of the host command that shows the file name and cursor place.
const SHOW_POSITION: &str = "show-position";

/// The longest gap between presses on one cell that still counts them as
/// one double or triple click.
const MULTI_CLICK_INTERVAL: Duration = Duration::from_millis(400);

/// The most matches the completion menu shows at once.
const MENU_ROWS: usize = 8;

/// The background of the completion menu.
const MENU_BACKGROUND: Color = Color::AnsiValue(238);

/// The rows the status line takes at the bottom of the terminal.
const STATUS_ROWS: u16 = 1;

// ========================================================================
// Data Structures
// ========================================================================

/// What the event loop does after acting on the editor's requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Flow {
    /// Keep running.
    Continue,
    /// The editor asked to quit.
    Quit,
}

/// The running host: the editor, its one view, and where to save.
struct Host {
    /// Counts clicks, since terminals report each press alone.
    clicks: ClickCounter,
    /// The editor.
    editor: Editor,
    /// The last notice the editor sent, shown on the status line.
    notice: String,
    /// The file the document is saved to.
    path: Option<PathBuf>,
    /// The prompt row's surface, reused between frames.
    prompt_surface: Surface,
    /// When the host started: the origin of the timestamps it passes the
    /// editor.
    started: Instant,
    /// The editor's surface, reused between frames.
    surface: Surface,
    /// The one view.
    view: ViewId,
}

/// Raw mode and the alternate screen, restored when dropped.
struct TerminalGuard;
// ========================================================================
// Host Implementations
// ========================================================================

impl Host {
    /// A host editing the file at `path`, or an empty buffer when there is
    /// no path or no file there yet.
    fn open(path: Option<PathBuf>) -> io::Result<Self> {
        let text = match &path {
            Some(path) if path.exists() => fs::read_to_string(path)?,
            Some(_) | None => String::new(),
        };
        let options = EditorOptions {
            auto_pairs: true,
            editing_style: editing_style_from_env(),
            line_numbers: true,
            sign_column: true,
            ..EditorOptions::default()
        };
        let mut editor = Editor::new(options);
        let name = path.as_ref().map_or_else(
            || "[No Name]".to_string(),
            |path| path.display().to_string(),
        );
        let document = editor.open_document(&text);
        if path.is_some() {
            editor.set_document_name(document, Some(&name));
        }
        register_host_commands(&mut editor, name).map_err(io::Error::other)?;
        let extension = path
            .as_ref()
            .and_then(|path| path.extension())
            .and_then(|extension| extension.to_str());
        editor.set_comment_token(document, extension.and_then(comment_token_for_extension));
        editor.set_block_comment_tokens(
            document,
            extension.and_then(block_comment_tokens_for_extension),
        );
        if let Some(language) = extension.and_then(Language::from_extension) {
            let text = editor
                .document(document)
                .expect("the document was just opened");
            let highlighter = SyntaxHighlighter::new(language, text).map_err(io::Error::other)?;
            editor.set_highlighter(document, Some(Box::new(highlighter)));
        }
        cherry_engine::syntax::register_text_objects(&mut editor).map_err(io::Error::other)?;
        let view = editor
            .new_view(document)
            .expect("the document was just opened");
        Ok(Self {
            clicks: ClickCounter::new(MULTI_CLICK_INTERVAL),
            editor,
            notice: String::new(),
            path,
            prompt_surface: Surface::new(0, 0),
            started: Instant::now(),
            surface: Surface::new(0, 0),
            view,
        })
    }

    /// The time now, on the clock of the timestamps passed to the editor.
    fn now(&self) -> Timestamp {
        Timestamp::from_duration(self.started.elapsed())
    }

    /// Acts on every request the editor queued, in order.
    fn act_on_requests(&mut self) -> Flow {
        for request in self.editor.take_requests() {
            match request {
                HostRequest::Notice(notice) => self.notice = notice.message,
                HostRequest::Quit(_) => return Flow::Quit,
                HostRequest::ReadInto(read) => self.read_into(read),
                HostRequest::Save(save) => self.save(save.document),
                // A request this host does not know yet is not acted on.
                _ => {}
            }
        }
        Flow::Continue
    }

    /// Paints the view and the status line to `out`.
    fn paint(&mut self, out: &mut Stdout) -> io::Result<()> {
        let cursor = render_view(&self.editor, self.view, &mut self.surface);
        queue!(out, Hide)?;
        queue_surface(out, &self.surface, 0)?;
        self.paint_completion(out)?;
        let prompt_cursor = render_prompt(&self.editor, self.view, &mut self.prompt_surface);
        let cursor = match prompt_cursor {
            Some(prompt_cursor) => {
                queue_surface(out, &self.prompt_surface, self.surface.rows())?;
                Some(Cursor {
                    row: prompt_cursor.row + self.surface.rows(),
                    ..prompt_cursor
                })
            }
            None => {
                self.paint_status(out)?;
                cursor
            }
        };
        if let Some(cursor) = cursor {
            queue_cursor(out, cursor)?;
        }
        out.flush()
    }

    /// Paints the view's completion menu, if one is open, below the word it
    /// completes (above it when the view has no room below), the match
    /// typed now in reverse.
    fn paint_completion(&self, out: &mut Stdout) -> io::Result<()> {
        let Some(completion) = self
            .editor
            .view(self.view)
            .and_then(|view| view.completion())
        else {
            return Ok(());
        };
        let Some((column, row)) = self.editor.screen_cell(self.view, completion.anchor()) else {
            return Ok(());
        };
        let lines: Vec<String> = completion
            .items()
            .iter()
            .take(MENU_ROWS)
            .map(|item| match &item.detail {
                Some(detail) => format!(" {}  {detail} ", item.label),
                None => format!(" {} ", item.label),
            })
            .collect();
        let shown = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        let top = if row + 1 + shown <= self.surface.rows() {
            row + 1
        } else {
            row.saturating_sub(shown)
        };
        let room = usize::from(self.surface.cols().saturating_sub(column));
        let width = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(0)
            .min(room);
        for (index, line) in lines.iter().enumerate() {
            let text: String = format!("{line:<width$}").chars().take(width).collect();
            let attribute = if completion.highlighted() == Some(index) {
                Attribute::Reverse
            } else {
                Attribute::NoReverse
            };
            let y = top + u16::try_from(index).unwrap_or(u16::MAX);
            queue!(
                out,
                MoveTo(column, y),
                SetBackgroundColor(MENU_BACKGROUND),
                SetAttribute(attribute),
                Print(text),
                SetAttribute(Attribute::Reset),
                ResetColor,
            )?;
        }
        Ok(())
    }

    /// Paints the status line: the mode, pending keys, and the notice.
    fn paint_status(&self, out: &mut Stdout) -> io::Result<()> {
        let Some(view) = self.editor.view(self.view) else {
            return Ok(());
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
        let count = view
            .count()
            .map(|count| count.to_string())
            .unwrap_or_default();
        let register = view
            .register()
            .map(|name| format!("\"{name}"))
            .unwrap_or_default();
        let pending: Vec<_> = view.pending_keys().iter().map(Key::to_string).collect();
        let recording = view
            .recording()
            .map(|name| format!("recording @{name} "))
            .unwrap_or_default();
        let modified = if self.editor.is_modified(view.document()) {
            "[+] "
        } else {
            ""
        };
        let matches = self
            .editor
            .search_count(self.view)
            .filter(|matches| matches.total > 0)
            .map(|matches| format!("[{}/{}] ", matches.current, matches.total))
            .unwrap_or_default();
        let status = format!(
            " {mode} {modified}{matches}{recording}{register}{count}{} {}",
            pending.join(" "),
            self.notice
        );
        let width = usize::from(self.surface.cols());
        let line: String = format!("{status:<width$}").chars().take(width).collect();
        queue!(
            out,
            MoveTo(0, self.surface.rows()),
            SetAttribute(Attribute::Reverse),
            Print(line),
            SetAttribute(Attribute::Reset),
        )
    }

    /// Fits the view to a terminal `cols` by `rows`, less the status line.
    fn resize(&mut self, cols: u16, rows: u16) {
        let text_rows = rows.saturating_sub(STATUS_ROWS);
        self.editor.resize_view(self.view, cols, text_rows);
    }

    /// Runs the event loop until the editor asks to quit.
    fn run(&mut self, out: &mut Stdout) -> io::Result<()> {
        let (cols, rows) = terminal::size()?;
        self.resize(cols, rows);
        loop {
            self.paint(out)?;
            let timeout = self
                .editor
                .deadline()
                .map(|deadline| deadline.saturating_duration_since(self.now()));
            if let Some(event) = next_event(timeout)? {
                self.take_event(event, out)?;
            }
            self.editor.tick(self.now());
            if self.act_on_requests() == Flow::Quit {
                return Ok(());
            }
        }
    }

    /// Reads the file `read` names (the edited file when it names none)
    /// into the view below the line it gives (`:r`).
    fn read_into(&mut self, read: ReadIntoRequest) {
        let Some(path) = read.path.map(PathBuf::from).or_else(|| self.path.clone()) else {
            self.notice = "No file name to read".to_string();
            return;
        };
        match fs::read_to_string(&path) {
            Ok(text) => self.editor.insert_lines(read.view, read.line, &text),
            Err(err) => self.notice = format!("Cannot read \"{}\": {err}", path.display()),
        }
    }

    /// Writes `document` to the file, with the line ending it came with, and
    /// tells the editor it is saved.
    fn save(&mut self, document: DocumentId) {
        let Some(path) = &self.path else {
            self.notice = "No file name to save to".to_string();
            return;
        };
        let Some(text) = self
            .editor
            .document(document)
            .map(Document::text_with_line_endings)
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

    /// Feeds a terminal event to the editor.
    fn take_event(&mut self, event: Event, out: &mut Stdout) -> io::Result<()> {
        match event {
            Event::Key(key_event) => {
                if let Some(key) = map_key(key_event) {
                    self.notice.clear();
                    let now = self.now();
                    self.editor.handle_key(self.view, key, now);
                }
            }
            Event::Resize(cols, rows) => {
                self.resize(cols, rows);
                queue!(out, Clear(ClearType::All))?;
            }
            Event::Mouse(mouse_event) => {
                if mouse_event.row >= self.surface.rows() {
                    return Ok(());
                }
                let now = self.now();
                if let Some(event) = map_mouse(mouse_event, &mut self.clicks, now) {
                    self.editor.handle_mouse(self.view, event);
                }
            }
            Event::FocusGained | Event::FocusLost | Event::Paste(_) => {}
        }
        Ok(())
    }
}

// ========================================================================
// TerminalGuard Implementations
// ========================================================================

impl TerminalGuard {
    /// Enters raw mode and the alternate screen, asks a terminal that can
    /// for unambiguous key codes (so Escape, `C-S-z`, and `C-/` arrive as
    /// typed), and makes a panic restore the terminal before its message
    /// prints.
    fn enter(out: &mut Stdout) -> io::Result<Self> {
        let default_hook = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            restore_terminal();
            default_hook(info);
        }));
        terminal::enable_raw_mode()?;
        execute!(
            out,
            EnterAlternateScreen,
            EnableMouseCapture,
            Clear(ClearType::All)
        )?;
        if terminal::supports_keyboard_enhancement().unwrap_or(false) {
            let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
            execute!(out, PushKeyboardEnhancementFlags(flags))?;
        }
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

// ========================================================================
// Functions
// ========================================================================

fn main() -> io::Result<()> {
    let path = env::args_os().nth(1).map(PathBuf::from);
    let mut host = Host::open(path)?;
    let mut out = io::stdout();
    let _guard = TerminalGuard::enter(&mut out)?;
    host.run(&mut out)
}

/// The editing style [`STYLE_VARIABLE`] names in [`STYLES`], else
/// Cherry's.
fn editing_style_from_env() -> EditingStyle {
    let named = env::var(STYLE_VARIABLE).unwrap_or_default();
    STYLES
        .iter()
        .find(|(name, _)| *name == named)
        .map_or(EditingStyle::Cherry, |(_, style)| *style)
}

/// The Cherry key for a crossterm key event, or `None` for a release or a
/// key Cherry has no name for (Caps Lock, media keys, a lone modifier).
fn map_key(event: KeyEvent) -> Option<Key> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let mut mods = map_mods(event.modifiers);
    let code = match event.code {
        event::KeyCode::BackTab => {
            mods |= Mods::SHIFT;
            KeyCode::Tab
        }
        event::KeyCode::Backspace => KeyCode::Backspace,
        event::KeyCode::Char(c) => KeyCode::Char(c),
        event::KeyCode::Delete => KeyCode::Delete,
        event::KeyCode::Down => KeyCode::Down,
        event::KeyCode::End => KeyCode::End,
        event::KeyCode::Enter => KeyCode::Enter,
        event::KeyCode::Esc => KeyCode::Esc,
        event::KeyCode::F(n) => KeyCode::F(n),
        event::KeyCode::Home => KeyCode::Home,
        event::KeyCode::Insert => KeyCode::Insert,
        event::KeyCode::Left => KeyCode::Left,
        event::KeyCode::PageDown => KeyCode::PageDown,
        event::KeyCode::PageUp => KeyCode::PageUp,
        event::KeyCode::Right => KeyCode::Right,
        event::KeyCode::Tab => KeyCode::Tab,
        event::KeyCode::Up => KeyCode::Up,
        event::KeyCode::CapsLock
        | event::KeyCode::KeypadBegin
        | event::KeyCode::Media(_)
        | event::KeyCode::Menu
        | event::KeyCode::Modifier(_)
        | event::KeyCode::Null
        | event::KeyCode::NumLock
        | event::KeyCode::Pause
        | event::KeyCode::PrintScreen
        | event::KeyCode::ScrollLock => return None,
    };
    Some(Key::new(code, mods))
}

/// The Cherry mouse event for a crossterm one, or `None` for one Cherry
/// does not take (another button, a bare move, a sideways scroll). A
/// press on the cell of the last one, soon enough after it, is a double
/// or triple click, counted by `clicks`.
fn map_mouse(
    event: event::MouseEvent,
    clicks: &mut ClickCounter,
    now: Timestamp,
) -> Option<MouseEvent> {
    let kind = match event.kind {
        event::MouseEventKind::Down(MouseButton::Left) => MouseEventKind::Press,
        event::MouseEventKind::Drag(MouseButton::Left) => MouseEventKind::Drag,
        event::MouseEventKind::ScrollDown => MouseEventKind::ScrollDown,
        event::MouseEventKind::ScrollUp => MouseEventKind::ScrollUp,
        event::MouseEventKind::Up(MouseButton::Left) => MouseEventKind::Release,
        event::MouseEventKind::Down(_)
        | event::MouseEventKind::Drag(_)
        | event::MouseEventKind::Moved
        | event::MouseEventKind::ScrollLeft
        | event::MouseEventKind::ScrollRight
        | event::MouseEventKind::Up(_) => return None,
    };
    let event = MouseEvent::new(kind, event.column, event.row).with_mods(map_mods(event.modifiers));
    Some(clicks.count(event, now))
}

/// The Cherry modifiers for crossterm's; Meta counts as Alt, and Hyper,
/// which Cherry has no name for, is dropped.
fn map_mods(modifiers: KeyModifiers) -> Mods {
    let mut mods = Mods::NONE;
    for (modifier, cherry) in [
        (KeyModifiers::ALT, Mods::ALT),
        (KeyModifiers::CONTROL, Mods::CTRL),
        (KeyModifiers::META, Mods::ALT),
        (KeyModifiers::SHIFT, Mods::SHIFT),
        (KeyModifiers::SUPER, Mods::SUPER),
    ] {
        if modifiers.contains(modifier) {
            mods |= cherry;
        }
    }
    mods
}

/// Registers the commands this host adds to the editor and binds their
/// keys: `C-g` in Normal mode shows the file `name` and where the cursor
/// is, as Vim's `C-g` does, and `:trailing` decorates trailing blanks. The closure keeps the name, host state the
/// editor knows nothing of.
fn register_host_commands(editor: &mut Editor, name: String) -> Result<(), CommandError> {
    let show_position = move |cx: &mut CommandContext<'_>| {
        let Some(view) = cx.editor.view(cx.view) else {
            return;
        };
        let Some(document) = cx.editor.document(view.document()) else {
            return;
        };
        let Some(position) = document.char_to_position(view.cursor()) else {
            return;
        };
        let message = format!(
            "\"{name}\" line {} of {}, column {}",
            position.line + 1,
            document.line_count(),
            position.column + 1
        );
        cx.editor.info(message);
    };
    editor.register_command(SHOW_POSITION, show_position)?;
    editor.bind(Mode::Normal, &["C-g"], SHOW_POSITION)?;
    for (keys, id) in CURSOR_KEYS {
        editor.bind(Mode::Normal, &[keys], id)?;
    }
    editor.register_ex_command("trailing", mark_trailing_blanks)
}

/// Waits for the next terminal event, but only for `timeout` when a key
/// sequence is pending, so the editor can be ticked; `None` on timeout.
fn next_event(timeout: Option<Duration>) -> io::Result<Option<Event>> {
    let Some(timeout) = timeout else {
        return event::read().map(Some);
    };
    if event::poll(timeout.max(Duration::from_millis(1)))? {
        event::read().map(Some)
    } else {
        Ok(None)
    }
}

/// Queues the cursor at its cell in its shape, and shows it.
fn queue_cursor(out: &mut Stdout, cursor: Cursor) -> io::Result<()> {
    let style = match cursor.shape {
        CursorShape::Bar => SetCursorStyle::SteadyBar,
        CursorShape::Block => SetCursorStyle::SteadyBlock,
        CursorShape::Underline => SetCursorStyle::SteadyUnderScore,
        _ => SetCursorStyle::DefaultUserShape,
    };
    queue!(out, MoveTo(cursor.column, cursor.row), style, Show)
}

/// Queues every cell of `surface`, its top row at terminal row `top`,
/// skipping the second cell of a wide character.
fn queue_surface(out: &mut Stdout, surface: &Surface, top: u16) -> io::Result<()> {
    for row in 0..surface.rows() {
        queue!(out, MoveTo(0, top + row))?;
        for column in 0..surface.cols() {
            let Some(cell) = surface.cell(column, row) else {
                continue;
            };
            if cell.symbol.is_empty() {
                continue;
            }
            queue_cell_style(out, cell)?;
            queue!(out, Print(&cell.symbol))?;
        }
    }
    queue!(out, ResetColor, SetAttribute(Attribute::Reset))
}

/// Queues the colors of `cell`'s style, and its underline, if any.
fn queue_cell_style(out: &mut Stdout, cell: &Cell) -> io::Result<()> {
    let (foreground, background) = match cell.style {
        Style::CursorLine => (Color::Reset, CURSOR_LINE_BACKGROUND),
        Style::ExtraCursor => (Color::Black, EXTRA_CURSOR_BACKGROUND),
        Style::Highlight => (Color::Reset, HIGHLIGHT_BACKGROUND),
        Style::Inlay => (INLAY_FOREGROUND, Color::Reset),
        Style::LineNumber => (LINE_NUMBER_FOREGROUND, Color::Reset),
        Style::MatchingBracket => (Color::Reset, MATCHING_BRACKET_BACKGROUND),
        Style::SearchMatch => (Color::Reset, SEARCH_MATCH_BACKGROUND),
        Style::Selection => (Color::Reset, SELECTION_BACKGROUND),
        Style::Sign(kind) => (kind_color(kind), Color::Reset),
        Style::Text => (Color::Reset, Color::Reset),
        _ => (Color::Reset, Color::Reset),
    };
    let foreground = cell
        .highlight
        .and_then(highlight_name)
        .map_or(foreground, highlight_color);
    let underline = match cell.underline {
        Some(kind) => {
            queue!(out, SetUnderlineColor(kind_color(kind)))?;
            Attribute::Undercurled
        }
        None => Attribute::NoUnderline,
    };
    queue!(
        out,
        SetAttribute(underline),
        SetForegroundColor(foreground),
        SetBackgroundColor(background)
    )
}

/// The color of text highlighted as `name` (`"keyword"`, `"string.escape"`),
/// by the first part of the name.
fn highlight_color(name: &str) -> Color {
    let family = name.split('.').next().unwrap_or(name);
    match family {
        "attribute" | "label" => Color::Magenta,
        "boolean" | "constant" | "number" => Color::DarkYellow,
        "comment" => Color::DarkGrey,
        "constructor" | "type" => Color::Yellow,
        "escape" | "operator" | "punctuation" => Color::Cyan,
        "function" => Color::Blue,
        "keyword" => Color::Magenta,
        "property" | "variable" => Color::Reset,
        "string" | "text" => Color::Green,
        _ => Color::Reset,
    }
}

/// The color a decoration of `kind` is drawn in: its underline, its sign.
fn kind_color(kind: DecorationKind) -> Color {
    match kind {
        DecorationKind::Error => Color::Red,
        DecorationKind::Hint => Color::Cyan,
        DecorationKind::Info => Color::Blue,
        DecorationKind::Warning => Color::Yellow,
        _ => Color::Reset,
    }
}

/// `:trailing`: marks the trailing blanks of every line as warnings, with
/// a `W` sign by each line that has some; `:trailing!` clears them. A
/// small stand-in for the diagnostics a language server would send.
fn mark_trailing_blanks(ex: &mut ExContext<'_>) {
    let Some(document_id) = ex.editor.view(ex.view).map(|view| view.document()) else {
        return;
    };
    if ex.bang {
        return ex.editor.clear_decorations(document_id, TRAILING_OWNER);
    }
    let Some(document) = ex.editor.document(document_id) else {
        return;
    };
    let (decorations, signs) = trailing_blanks(document);
    let found = decorations.len();
    ex.editor
        .set_decorations(document_id, TRAILING_OWNER, decorations);
    ex.editor.set_signs(document_id, TRAILING_OWNER, signs);
    ex.editor
        .info(format!("{found} lines with trailing blanks"));
}

/// The trailing blanks of each line of `document` as warnings, and a sign
/// by each line that has them.
fn trailing_blanks(document: &Document) -> (Vec<Decoration>, Vec<Sign>) {
    let mut decorations = Vec::new();
    let mut signs = Vec::new();
    let mut start = 0;
    for line in 0..document.len_lines() {
        let text = document.line_text(line).unwrap_or_default();
        let len = text.chars().count();
        let kept = text.trim_end().chars().count();
        if kept < len {
            let range = start + kept..start + len;
            decorations.push(Decoration::new(range, DecorationKind::Warning));
            signs.push(Sign::new(line, "W", DecorationKind::Warning));
        }
        start += len + 1;
    }
    (decorations, signs)
}

/// Leaves the alternate screen and raw mode and restores the cursor;
/// errors are ignored, as there is nothing left to tell them to.
fn restore_terminal() {
    let mut out = io::stdout();
    let _ = execute!(
        out,
        ResetColor,
        SetCursorStyle::DefaultUserShape,
        Show,
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        LeaveAlternateScreen
    );
    let _ = terminal::disable_raw_mode();
}

// ========================================================================
// Tests
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use cherry_engine::keys::parse_key;
    use crossterm::event::{KeyEventState, ModifierKeyCode};

    fn press(code: event::KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            kind: KeyEventKind::Press,
            modifiers,
            state: KeyEventState::NONE,
        }
    }

    fn key(token: &str) -> Option<Key> {
        Some(parse_key(token).unwrap())
    }

    #[test]
    fn test_shifted_letters_map_to_one_key_however_the_terminal_reports_them() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(map_key(press(event::KeyCode::Char('G'), shift)), key("G"));
        assert_eq!(map_key(press(event::KeyCode::Char('G'), none)), key("G"));
        assert_eq!(map_key(press(event::KeyCode::Char('g'), shift)), key("G"));
        let ctrl_shift = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
        assert_eq!(
            map_key(press(event::KeyCode::Char('F'), ctrl_shift)),
            key("C-S-f")
        );
        assert_eq!(map_key(press(event::KeyCode::Char('?'), shift)), key("?"));
    }

    #[test]
    fn test_control_alt_and_meta_chords_map_to_cherry_modifiers() {
        let ctrl = KeyModifiers::CONTROL;
        assert_eq!(map_key(press(event::KeyCode::Char('r'), ctrl)), key("C-r"));
        let alt = KeyModifiers::ALT;
        assert_eq!(map_key(press(event::KeyCode::Char('x'), alt)), key("M-x"));
        let meta = KeyModifiers::META;
        assert_eq!(map_key(press(event::KeyCode::Char('x'), meta)), key("M-x"));
        let hyper = KeyModifiers::HYPER;
        assert_eq!(map_key(press(event::KeyCode::Esc, hyper)), key("<esc>"));
    }

    #[test]
    fn test_back_tab_is_shift_tab_and_named_keys_map_by_name() {
        let none = KeyModifiers::NONE;
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            map_key(press(event::KeyCode::BackTab, shift)),
            key("S-<tab>")
        );
        assert_eq!(
            map_key(press(event::KeyCode::BackTab, none)),
            key("S-<tab>")
        );
        assert_eq!(map_key(press(event::KeyCode::Enter, none)), key("<cr>"));
        assert_eq!(map_key(press(event::KeyCode::Backspace, none)), key("<bs>"));
        assert_eq!(map_key(press(event::KeyCode::F(5), none)), key("<f5>"));
    }

    #[test]
    fn test_c_g_shows_the_file_name_and_the_cursor_line_and_column() {
        let mut editor = Editor::new(EditorOptions::default());
        register_host_commands(&mut editor, "notes.txt".to_string()).unwrap();
        let document = editor.open_document("one\ntwo\nthree");
        let view = editor.new_view(document).unwrap();
        for token in ["j", "l", "C-g"] {
            editor.handle_key(view, parse_key(token).unwrap(), Timestamp::default());
        }
        let notices: Vec<_> = editor
            .take_requests()
            .into_iter()
            .filter_map(|request| match request {
                HostRequest::Notice(notice) => Some(notice.message),
                _ => None,
            })
            .collect();
        assert_eq!(notices, ["\"notes.txt\" line 2 of 3, column 2"]);
    }

    #[test]
    fn test_releases_and_keys_cherry_cannot_name_are_dropped() {
        let none = KeyModifiers::NONE;
        let release = KeyEvent {
            kind: KeyEventKind::Release,
            ..press(event::KeyCode::Char('a'), none)
        };
        assert_eq!(map_key(release), None);
        let repeat = KeyEvent {
            kind: KeyEventKind::Repeat,
            ..press(event::KeyCode::Char('a'), none)
        };
        assert_eq!(map_key(repeat), key("a"));
        assert_eq!(map_key(press(event::KeyCode::CapsLock, none)), None);
        let lone_shift = event::KeyCode::Modifier(ModifierKeyCode::LeftShift);
        assert_eq!(map_key(press(lone_shift, KeyModifiers::SHIFT)), None);
    }

    #[test]
    fn test_trailing_blanks_become_warnings_with_a_sign_per_line() {
        let document = Document::new("ok\nab  \n\t\nend");
        let (decorations, signs) = trailing_blanks(&document);
        let ranges: Vec<_> = decorations.iter().map(|d| d.range.clone()).collect();
        assert_eq!(ranges, [5..7, 8..9]);
        let lines: Vec<_> = signs.iter().map(|sign| sign.line).collect();
        assert_eq!(lines, [1, 2]);
    }

    #[test]
    fn test_left_presses_are_counted_and_other_buttons_dropped() {
        let press = |button| event::MouseEvent {
            column: 3,
            kind: event::MouseEventKind::Down(button),
            modifiers: KeyModifiers::NONE,
            row: 2,
        };
        let mut clicks = ClickCounter::new(MULTI_CLICK_INTERVAL);
        let now = Timestamp::default();
        let mut kind = |button| map_mouse(press(button), &mut clicks, now).map(|event| event.kind);
        assert_eq!(kind(MouseButton::Left), Some(MouseEventKind::Press));
        assert_eq!(kind(MouseButton::Right), None);
        assert_eq!(kind(MouseButton::Left), Some(MouseEventKind::DoubleClick));
    }
}
