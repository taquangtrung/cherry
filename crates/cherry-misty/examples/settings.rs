//! A settings page over a small app: Ctrl+, opens it (it opens at start).
//! The page is misty's `settings_page`, a tree of the page kit's elements
//! held in a `ViewHost`: Tab and Shift+Tab move between its controls, Enter
//! or Space presses one, the arrows step a number or a dropdown, and Esc
//! closes the page. The scope tabs switch between User and Project; a
//! change made in Project marks its row, and its Reset button drops the
//! Project value. The Keybindings card rebinds the demo's commands: Enter
//! on a command's keys, then press the new key. Changes apply at once (the
//! theme, the counter step) and are printed to standard output, where a
//! real app would save them. With the page closed, F5 counts and Ctrl+Q
//! quits, resolved by Cherry's keymap from the registry's keys.
//!
//! ```sh
//! cargo run -p cherry-misty --example settings
//! ```

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use cherry_misty::clock;
use cherry_misty::engine::keys::{Keymap, Resolution, Resolver};
use cherry_misty::keymap;
use cherry_misty::keys::to_cherry;
use misty::view::{HostedView, SettingChange, SettingEdit, SettingScope, ViewHost, settings_page};
use misty::{
    AppContext, Application, Command, CommandRegistry, Element, Error, FocusId, Frame, ImeFocus,
    Input, KeyCode, KeyScope, NumberRange, Point, Rect, Rgb, Setting, SettingValue,
    SettingsSection, Span, Strings, StyledText, Theme, ThemeToken, WindowId, WindowOptions,
};

// ========================================================================
// Constants
// ========================================================================

/// The window title.
const TITLE: &str = "misty settings";

/// How long a half-typed key sequence waits for its next key.
const KEY_TIMEOUT: Duration = Duration::from_secs(1);

/// The id of the command that opens the settings page.
const OPEN_SETTINGS: &str = "settings.open";

/// The id of the command that counts.
const INCREMENT: &str = "counter.increment";

/// The id of the command that quits.
const QUIT: &str = "app.quit";

/// Every command of the demo, each written once.
const COMMANDS: &[Command] = &[
    Command::new(OPEN_SETTINGS, "Open Settings", "App").with_keys(&["C-,"]),
    Command::new(INCREMENT, "Increment", "Counter").with_keys(&["<f5>"]),
    Command::new(QUIT, "Quit", "App").with_keys(&["C-q"]),
];

/// The id of the theme setting.
const THEME: &str = "appearance.theme";

/// The id of the counter step setting.
const STEP: &str = "counter.step";

/// The scope tabs' names.
const SCOPES: [&str; 2] = ["User", "Project"];

/// The blank cells around the text, in columns and rows.
const MARGIN: f32 = 1.0;

/// The id the page's tree is described to screen readers by.
const PAGE: FocusId = FocusId::new("settings");

// ========================================================================
// Data Structures
// ========================================================================

/// The demo: a counter, and the settings page while it is open.
struct Demo {
    /// The count so far.
    counter: f64,
    /// Whether the input method is on: while the page edits a text.
    ime: ImeFocus,
    /// The registry's keys, rebuilt after every rebinding.
    keymap: Keymap<(), &'static str>,
    /// The page's tree.
    page: ViewHost<Prefs>,
    /// Whether the page is open.
    page_open: bool,
    /// What the page shows, kept while it is closed so it reopens as left.
    prefs: Prefs,
    /// Turns keys into commands.
    resolver: Resolver,
    /// The colors the window is drawn in.
    theme: Theme,
    /// The open window, once it is open.
    window: Option<WindowId>,
}

/// The settings the page shows and edits, and every command's keys.
struct Prefs {
    /// The settings Project sets, in either scope.
    project: BTreeSet<String>,
    /// Whether a command's keys changed since the demo last rebound them.
    rebound: bool,
    /// Every command and its keys.
    registry: CommandRegistry,
    /// The scope shown: User, or Project.
    scope: usize,
    /// The scope tabs' names.
    scope_names: Vec<String>,
    /// The sections, holding the values in force.
    sections: Vec<SettingsSection>,
    /// The section shown.
    shown: usize,
    /// The text the rows are kept to.
    filter: String,
    /// The theme's words.
    strings: Strings,
}

// ========================================================================
// Demo Implementations
// ========================================================================

impl Demo {
    /// Runs the command `id`.
    fn run_command(&mut self, cx: &mut AppContext<'_, ()>, id: &'static str) {
        match id {
            INCREMENT => self.count(),
            OPEN_SETTINGS => self.page_open = true,
            QUIT => cx.exit(),
            // No other id is registered.
            _ => {}
        }
    }

    /// Adds the counter step to the count.
    fn count(&mut self) {
        if let Some(SettingValue::Number(step)) = self.prefs.value(STEP) {
            self.counter += step;
        }
    }
}

impl Application for Demo {
    type Message = ();

    fn init(&mut self, _cx: &mut AppContext<'_, ()>, window: WindowId) {
        self.window = Some(window);
    }

    fn input(&mut self, cx: &mut AppContext<'_, ()>, window: WindowId, input: &Input) {
        cx.event_cx(window).request_redraw();
        if self.page_open {
            let used = self
                .page
                .handle(&mut self.prefs, input, &mut cx.event_cx(window));
            let escaped = matches!(input, Input::Key(key) if key.code == KeyCode::Esc);
            if !used && escaped {
                self.page_open = false;
            }
            if std::mem::take(&mut self.prefs.rebound) {
                self.keymap = bind(&self.prefs.registry);
            }
            self.theme = match self.prefs.value(THEME) {
                Some(SettingValue::Choice(1)) => Theme::light(),
                _ => Theme::dark(),
            };
            return;
        }
        match input {
            Input::CloseRequested => cx.exit(),
            Input::Key(key) => {
                let Some(key) = to_cherry(*key) else {
                    return;
                };
                let keymap = self.keymap.active(&[()]);
                for resolution in self.resolver.feed(&keymap, key, clock::now()) {
                    if let Resolution::Matched(found) = resolution {
                        self.run_command(cx, found.command);
                    }
                }
            }
            _ => {}
        }
    }

    fn about_to_wait(&mut self, cx: &mut AppContext<'_, ()>) -> Option<Instant> {
        let settled = self.resolver.tick(&self.keymap.active(&[()]), clock::now());
        if let Some(Resolution::Matched(found)) = settled {
            self.run_command(cx, found.command);
        }
        let glide = match (self.page_open, self.window) {
            (true, Some(window)) => self
                .page
                .about_to_wait(&mut self.prefs, &mut cx.event_cx(window)),
            (false, _) | (true, None) => None,
        };
        if let Some(window) = self.window {
            let caret = self.page.ime_area().filter(|_| self.page_open);
            self.ime.follow(&mut cx.event_cx(window), caret);
        }
        [self.resolver.deadline().map(clock::instant), glide]
            .into_iter()
            .flatten()
            .min()
    }

    fn render(&mut self, _window: WindowId, frame: &mut Frame) {
        frame.clear(self.theme.background.into());
        let bounds = frame.bounds();
        if self.page_open {
            self.prefs.strings = self.theme.strings.clone();
            self.page.draw(&self.prefs, frame, bounds, &self.theme);
            return;
        }
        let metrics = frame.cell_metrics();
        let hint = |id: &str| {
            self.prefs
                .registry
                .key_hint(id, &Strings::default())
                .unwrap_or_default()
        };
        let lines = [
            StyledText::new()
                .with(Span::new("Counter: ", ThemeToken::Muted.into()))
                .with(Span::new(self.counter.to_string(), ThemeToken::Accent.into()).bold()),
            StyledText::plain(
                format!(
                    "{} opens settings, {} counts, {} quits.",
                    hint(OPEN_SETTINGS),
                    hint(INCREMENT),
                    hint(QUIT)
                ),
                ThemeToken::Muted.into(),
            )
            .with_ellipsis(),
        ];
        for (row, line) in lines.iter().enumerate() {
            let y = bounds.y + (MARGIN + row as f32) * metrics.height;
            let clip = Rect::new(bounds.x, y, bounds.width, metrics.height);
            let origin = Point::new(bounds.x + MARGIN * metrics.width, y);
            frame.draw_text(line, origin, clip, &self.theme);
        }
    }
}

// ========================================================================
// Prefs Implementations
// ========================================================================

impl Prefs {
    /// The value in force of the setting `id`.
    fn value(&self, id: &str) -> Option<&SettingValue> {
        self.sections
            .iter()
            .flat_map(SettingsSection::settings)
            .find(|setting| setting.id() == id)
            .map(Setting::value)
    }

    /// Shows and "saves" `change` in the scope shown.
    fn change(&mut self, change: SettingChange) {
        let scope = SCOPES[self.scope];
        match change.edit {
            SettingEdit::Changed(value) => {
                println!("save {} = {value:?} in scope {scope}", change.id);
                for section in &mut self.sections {
                    section.set_value(&change.id, value.clone());
                }
                if self.scope > 0 {
                    self.project.insert(change.id);
                }
            }
            SettingEdit::Cleared => {
                // A real app would load the User value back in here.
                println!("drop the Project value of {}", change.id);
                self.project.remove(&change.id);
            }
            // The demo has no action rows; a kind misty adds later is
            // none of its either.
            _ => {}
        }
    }

    /// Where the setting `id` stands against the scopes.
    fn scope_of(project: &BTreeSet<String>, scope: usize, id: &str) -> SettingScope {
        match (project.contains(id), scope) {
            (false, _) => SettingScope::InForce,
            (true, 0) => SettingScope::Overridden,
            (true, _) => SettingScope::OwnValue,
        }
    }
}

impl HostedView for Prefs {
    fn view(&self) -> Element<'_, Self> {
        let (project, scope) = (&self.project, self.scope);
        settings_page("Settings", &self.sections, &self.strings)
            .shown(self.shown, |prefs: &mut Self, at| prefs.shown = at)
            .filter(&self.filter, |prefs: &mut Self, filter| {
                prefs.filter = filter
            })
            .on_change(Self::change)
            .scopes(
                &self.scope_names,
                scope,
                |prefs: &mut Self, at| {
                    // A real app would load the scope's values in here.
                    println!("show scope {}", SCOPES[at]);
                    prefs.scope = at;
                },
                move |id| Self::scope_of(project, scope, id),
            )
            .keybindings("Keybindings", &self.registry, |prefs: &mut Self, change| {
                if prefs.registry.edit_keys(change.id, &change.edit).is_ok() {
                    println!("save keys of {} = {:?}", change.id, change.edit);
                    prefs.rebound = true;
                }
            })
            .into()
    }
}

// ========================================================================
// Functions
// ========================================================================

/// The keymap of every command's keys in force in `registry`.
fn bind(registry: &CommandRegistry) -> Keymap<(), &'static str> {
    let mut keymap = Keymap::new();
    // A rebinding may leave a key that is a prefix of another; that
    // binding is refused and the rest stays bound.
    let _ = keymap::bind(registry, &mut keymap, (), KeyScope::All);
    keymap
}

/// The demo's settings.
fn sections() -> Vec<SettingsSection> {
    vec![
        SettingsSection::new("Appearance")
            .with(
                Setting::choice(THEME, "Theme", &["Dark", "Light"], 0)
                    .with_description("The colors of the whole window."),
            )
            .with(Setting::number(
                "appearance.font-size",
                "Font size",
                14.0,
                NumberRange::new(8.0, 32.0, 1.0),
            ))
            .with(Setting::color(
                "appearance.accent",
                "Accent color",
                Rgb::new(0x89, 0xb4, 0xfa),
            ))
            .with(Setting::toggle("appearance.blink", "Blinking cursor", true)),
        SettingsSection::new("Counter")
            .with(
                Setting::number(STEP, "Step", 1.0, NumberRange::new(0.5, 10.0, 0.5))
                    .with_description("How much each Increment adds."),
            )
            .with(Setting::text("counter.name", "Name", "clicks")),
    ]
}

fn main() -> Result<(), Error> {
    let registry = CommandRegistry::new(COMMANDS).expect("the command table is valid");
    let app = Demo {
        counter: 0.0,
        ime: ImeFocus::new(),
        keymap: bind(&registry),
        page: ViewHost::new(PAGE),
        page_open: true,
        prefs: Prefs {
            filter: String::new(),
            project: BTreeSet::new(),
            rebound: false,
            registry,
            scope: 0,
            scope_names: SCOPES.iter().map(|name| (*name).to_owned()).collect(),
            sections: sections(),
            shown: 0,
            strings: Strings::default(),
        },
        resolver: Resolver::new(KEY_TIMEOUT),
        theme: Theme::dark(),
        window: None,
    };
    misty::run(app, WindowOptions::new(TITLE))
}
