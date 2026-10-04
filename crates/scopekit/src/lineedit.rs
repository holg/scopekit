//! A command line that edits like a shell (bash, Emacs keys): for the
//! input line of a REPL in a scopekit app, or inline in a plain terminal
//! with [`read_line`].
//!
//! [`LineEditor::key`] takes the crossterm key events scopekit delivers in
//! both modes and handles the editing keys; it returns `false` for the keys
//! the app decides on itself (Enter, Tab, Ctrl-C, Ctrl-D on an empty line,
//! Ctrl-L, Esc, PageUp/PageDown, function keys).
//!
//! | keys | |
//! |---|---|
//! | Ctrl-A, Ctrl-E, Home, End | start, end of the line |
//! | Ctrl-Home, Ctrl-End, Alt-<, Alt-> | start, end of the whole input |
//! | Ctrl-B, Ctrl-F, ←, → | a character back, forward |
//! | Alt-B, Alt-F, Alt/Ctrl-←, → | a word back, forward |
//! | ↑, ↓, Ctrl-P, Ctrl-N | a line up, down in input of several lines, else the history |
//! | Backspace, Ctrl-H, Delete, Ctrl-D | delete a character |
//! | Ctrl-W, Alt-Backspace, Alt-D, Alt-Delete | cut a word back, forward |
//! | Ctrl-K, Ctrl-U | cut to the end, the start of the line |
//! | Ctrl-Y | paste what was cut (cuts in a row add up) |
//! | Ctrl-T | swap two characters |
//! | Alt-U, Alt-L, Alt-C | the word in upper, lower case, capitalized |
//! | Ctrl-_, Ctrl-/, Ctrl-Z | undo |
//! | Ctrl-R | search the history (Ctrl-R older, Ctrl-S newer, Esc edit, Ctrl-G cancel) |
//!
//! ```
//! use scopekit::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
//! use scopekit::lineedit::LineEditor;
//!
//! let mut line = LineEditor::new();
//! line.insert_str("(setq a 1)");
//! assert!(line.key(KeyEvent::new(KeyCode::Char('w'), KeyModifiers::CONTROL)));
//! assert_eq!(line.text(), "(setq a ");
//! ```

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Undo steps kept
const UNDO_STEPS: usize = 200;

/// Ctrl-R: the text searched for, the history entry found, and the input
/// before (Ctrl-G puts it back)
#[derive(Debug, Clone)]
struct Search {
    query: String,
    found: Option<usize>,
    failed: bool,
    saved: (Vec<char>, usize),
}

/// What Tab completion did (see [`LineEditor::complete`])
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Completion {
    /// Nothing before the cursor to complete
    Nothing,
    /// No name starts with the word before the cursor
    NoMatch(String),
    /// Completed to the only name
    Unique,
    /// Completed to the longest common prefix; these are the names
    Several(Vec<String>),
}

/// The text of a command line, its cursor, history, cut text and undo
/// steps; see the [module](self) for the keys.
#[derive(Debug, Clone)]
pub struct LineEditor {
    /// The input, characters (several lines for a form not closed yet)
    pub input: Vec<char>,
    /// Index into `input`
    pub cursor: usize,
    /// Whether a character belongs to a word (Alt-B, Ctrl-W, completion):
    /// by default everything but blanks, brackets, quotes, `;` and `,`
    pub word_char: fn(char) -> bool,
    history: Vec<String>,
    history_pos: Option<usize>,
    killed: String,
    last_kill: bool,
    undo: Vec<(Vec<char>, usize)>,
    last_typing: bool,
    search: Option<Search>,
}

impl Default for LineEditor {
    fn default() -> Self {
        Self::new()
    }
}

/// The default word characters: Lisp symbols, numbers, paths
pub fn default_word_char(c: char) -> bool {
    !(c.is_whitespace() || "()[]{}'\"`;,".contains(c))
}

impl LineEditor {
    /// An empty line without history
    pub fn new() -> Self {
        LineEditor {
            input: Vec::new(),
            cursor: 0,
            word_char: default_word_char,
            history: Vec::new(),
            history_pos: None,
            killed: String::new(),
            last_kill: false,
            undo: Vec::new(),
            last_typing: false,
            search: None,
        }
    }

    /// The input as a string
    pub fn text(&self) -> String {
        self.input.iter().collect()
    }

    /// Whether the input is empty
    pub fn is_empty(&self) -> bool {
        self.input.is_empty()
    }

    /// Replaces the input; the cursor goes to its end
    pub fn set_text(&mut self, text: &str) {
        self.input = text.chars().collect();
        self.cursor = self.input.len();
    }

    /// Empties the input (Esc, Ctrl-C in most apps); undoable
    pub fn clear(&mut self) {
        if !self.input.is_empty() {
            self.undo
                .push((std::mem::take(&mut self.input), self.cursor));
        }
        self.cursor = 0;
    }

    /// Inserts a character at the cursor
    pub fn insert(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
    }

    /// Inserts text at the cursor (a paste: newlines kept, tabs as blanks,
    /// other control characters left out); one undo step
    pub fn insert_str(&mut self, text: &str) {
        self.undo.push((self.input.clone(), self.cursor));
        for c in text.replace("\r\n", "\n").replace('\r', "\n").chars() {
            match c {
                '\t' => self.insert(' '),
                c if c == '\n' || !c.is_control() => self.insert(c),
                _ => {}
            }
        }
        self.last_typing = false;
    }

    /// The history, oldest first
    pub fn history(&self) -> &[String] {
        &self.history
    }

    /// Replaces the history (e.g. loaded from a [`Store`](crate::store::Store))
    pub fn set_history(&mut self, history: Vec<String>) {
        self.history = history;
        self.history_pos = None;
    }

    /// Adds an entry unless it repeats the last one
    pub fn push_history(&mut self, entry: &str) {
        if !entry.trim().is_empty() && self.history.last().map(String::as_str) != Some(entry) {
            self.history.push(entry.to_owned());
        }
        self.history_pos = None;
    }

    /// Takes the input to run it: into the history, the line empty again
    /// (Ctrl-_ brings it back)
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.push_history(&text);
        self.clear();
        self.search = None;
        text
    }

    /// While Ctrl-R searches: the line to show (the query and the keys)
    pub fn search_prompt(&self) -> Option<String> {
        self.search.as_ref().map(|search| {
            format!(
                "{}reverse-i-search: {}_   Ctrl-R older · Ctrl-S newer · Enter run · Esc edit · Ctrl-G cancel",
                if search.failed { "failing " } else { "" },
                search.query
            )
        })
    }

    /// Whether Ctrl-R is searching
    pub fn searching(&self) -> bool {
        self.search.is_some()
    }

    /// Handles an editing key; `false`: not one, the app decides (Enter,
    /// Tab, Ctrl-C, Ctrl-D on an empty line, Ctrl-L, Esc, PageUp ...)
    pub fn key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if self.search.is_some() && self.search_key(key) {
            return true;
        }
        let before = (self.input.clone(), self.cursor);
        let typing = matches!(key.code, KeyCode::Char(c)
            if !(ctrl || c.is_whitespace() || alt && c.is_ascii_alphanumeric()));
        let killing = self.last_kill;
        self.last_kill = false;
        match key.code {
            // Moving
            KeyCode::Char('a') if ctrl => self.cursor = self.line_start(),
            KeyCode::Char('e') if ctrl => self.cursor = self.line_end(),
            KeyCode::Home if ctrl => self.cursor = 0,
            KeyCode::End if ctrl => self.cursor = self.input.len(),
            KeyCode::Home => self.cursor = self.line_start(),
            KeyCode::End => self.cursor = self.line_end(),
            KeyCode::Char('<') if alt => self.cursor = 0,
            KeyCode::Char('>') if alt => self.cursor = self.input.len(),
            KeyCode::Char('b') if ctrl => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char('f') if ctrl => self.cursor = (self.cursor + 1).min(self.input.len()),
            KeyCode::Char('b') if alt => self.cursor = self.word_back(),
            KeyCode::Char('f') if alt => self.cursor = self.word_forward(),
            KeyCode::Left if ctrl || alt => self.cursor = self.word_back(),
            KeyCode::Right if ctrl || alt => self.cursor = self.word_forward(),
            KeyCode::Left => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Right => self.cursor = (self.cursor + 1).min(self.input.len()),
            KeyCode::Up => self.line_or_history(true),
            KeyCode::Down => self.line_or_history(false),
            KeyCode::Char('p') if ctrl => self.line_or_history(true),
            KeyCode::Char('n') if ctrl => self.line_or_history(false),
            KeyCode::Char('r') if ctrl => self.search_start(),
            // Deleting
            KeyCode::Backspace if ctrl || alt => self.kill(self.word_back(), self.cursor, killing),
            KeyCode::Char('w') if ctrl => self.kill(self.word_back(), self.cursor, killing),
            KeyCode::Char('h') if ctrl => self.delete_back(),
            KeyCode::Backspace => self.delete_back(),
            KeyCode::Delete if ctrl || alt => self.kill(self.cursor, self.word_forward(), killing),
            KeyCode::Char('d') if alt => self.kill(self.cursor, self.word_forward(), killing),
            KeyCode::Char('d') if ctrl && !self.input.is_empty() => self.delete_forward(),
            KeyCode::Delete => self.delete_forward(),
            KeyCode::Char('k') if ctrl => {
                // At the end of a line: the newline, as in Emacs
                let end = self.line_end();
                let end = if end == self.cursor {
                    (end + 1).min(self.input.len())
                } else {
                    end
                };
                self.kill(self.cursor, end, killing);
            }
            KeyCode::Char('u') if ctrl => self.kill(self.line_start(), self.cursor, killing),
            KeyCode::Char('y') if ctrl => {
                for c in self.killed.clone().chars() {
                    self.insert(c);
                }
            }
            KeyCode::Char('t') if ctrl => self.transpose(),
            KeyCode::Char('u') if alt => self.change_word(|w| w.to_uppercase()),
            KeyCode::Char('l') if alt => self.change_word(|w| w.to_lowercase()),
            KeyCode::Char('c') if alt => self.change_word(|w| {
                let mut chars = w.chars();
                chars.next().map_or_else(String::new, |first| {
                    first
                        .to_uppercase()
                        .chain(chars.flat_map(char::to_lowercase))
                        .collect()
                })
            }),
            // Undo: Ctrl-_ (terminals send it for Ctrl-/ and Ctrl-7 too) or Ctrl-Z
            KeyCode::Char('_' | '/' | '7' | 'z') if ctrl => {
                if let Some((input, cursor)) = self.undo.pop() {
                    self.input = input;
                    self.cursor = cursor;
                }
                self.last_typing = false;
                return true;
            }
            // Left to the app: Ctrl-C, Ctrl-D on an empty line, Ctrl-J/M
            // (Enter), Ctrl-L, Cmd-keys
            KeyCode::Char('c' | 'd' | 'j' | 'm' | 'l' | 'g') if ctrl => return false,
            KeyCode::Char(_) if key.modifiers.contains(KeyModifiers::SUPER) => return false,
            // Other control keys and Alt with a letter or digit (a shell's
            // Meta keys) aren't typing; Option characters ({ [ @ | \ ~ on a
            // German keyboard, ∫ ç ...) are
            KeyCode::Char(c) if ctrl || (alt && c.is_ascii_alphanumeric()) => {}
            KeyCode::Char(c) => self.insert(c),
            _ => return false,
        }
        // A changed input can be undone; typed letters in a row as one step
        if self.input != before.0 && !(typing && self.last_typing) {
            self.undo.push(before);
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
        }
        self.last_typing = typing;
        true
    }

    /// Completes the word before the cursor from `names` (compared in lower
    /// case): the only one, else the longest common prefix
    pub fn complete(&mut self, names: &[String]) -> Completion {
        let word_char = self.word_char;
        let start = self.input[..self.cursor]
            .iter()
            .rposition(|&c| !word_char(c))
            .map_or(0, |i| i + 1);
        let prefix: String = self.input[start..self.cursor]
            .iter()
            .collect::<String>()
            .to_lowercase();
        if prefix.is_empty() {
            return Completion::Nothing;
        }
        let matches: Vec<String> = names
            .iter()
            .filter(|n| n.to_lowercase().starts_with(&prefix))
            .cloned()
            .collect();
        let Some(first) = matches.first() else {
            return Completion::NoMatch(prefix);
        };
        let common: String = matches
            .iter()
            .skip(1)
            .fold(first.to_lowercase(), |acc, name| {
                acc.chars()
                    .zip(name.to_lowercase().chars())
                    .take_while(|(a, b)| a == b)
                    .map(|(a, _)| a)
                    .collect()
            });
        self.undo.push((self.input.clone(), self.cursor));
        self.last_typing = false;
        for c in common.chars().skip(prefix.chars().count()) {
            self.insert(c);
        }
        if matches.len() == 1 {
            self.insert(' ');
            Completion::Unique
        } else {
            Completion::Several(matches)
        }
    }

    /// Start of the cursor's line
    pub fn line_start(&self) -> usize {
        self.input[..self.cursor]
            .iter()
            .rposition(|&c| c == '\n')
            .map_or(0, |i| i + 1)
    }

    /// End of the cursor's line
    pub fn line_end(&self) -> usize {
        self.input[self.cursor..]
            .iter()
            .position(|&c| c == '\n')
            .map_or(self.input.len(), |i| self.cursor + i)
    }

    fn delete_back(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.cursor);
        }
    }

    fn delete_forward(&mut self) {
        if self.cursor < self.input.len() {
            self.input.remove(self.cursor);
        }
    }

    /// Cuts `from..to` for Ctrl-Y; kills in a row add up, as in a shell
    fn kill(&mut self, from: usize, to: usize, append: bool) {
        if from >= to {
            self.last_kill = append;
            return;
        }
        let text: String = self.input.drain(from..to).collect();
        if !append {
            self.killed.clear();
        }
        if from < self.cursor {
            self.killed.insert_str(0, &text);
        } else {
            self.killed.push_str(&text);
        }
        self.cursor = from;
        self.last_kill = true;
    }

    /// Swaps the characters before and at the cursor (at the end: the last two)
    fn transpose(&mut self) {
        let len = self.input.len();
        if len < 2 || self.cursor == 0 {
            return;
        }
        let at = if self.cursor == len {
            len - 1
        } else {
            self.cursor
        };
        self.input.swap(at - 1, at);
        self.cursor = (at + 1).min(len);
    }

    /// Replaces the word from the cursor on (Alt-U, Alt-L, Alt-C)
    fn change_word(&mut self, change: impl Fn(&str) -> String) {
        let word_char = self.word_char;
        let mut from = self.cursor;
        while from < self.input.len() && !word_char(self.input[from]) {
            from += 1;
        }
        let mut to = from;
        while to < self.input.len() && word_char(self.input[to]) {
            to += 1;
        }
        let word: String = self.input[from..to].iter().collect();
        let changed: Vec<char> = change(&word).chars().collect();
        self.cursor = from + changed.len();
        self.input.splice(from..to, changed);
    }

    fn word_back(&self) -> usize {
        let mut i = self.cursor;
        while i > 0 && !(self.word_char)(self.input[i - 1]) {
            i -= 1;
        }
        while i > 0 && (self.word_char)(self.input[i - 1]) {
            i -= 1;
        }
        i
    }

    fn word_forward(&self) -> usize {
        let mut i = self.cursor;
        while i < self.input.len() && !(self.word_char)(self.input[i]) {
            i += 1;
        }
        while i < self.input.len() && (self.word_char)(self.input[i]) {
            i += 1;
        }
        i
    }

    /// Up and Down: a line up or down in input of several lines, else the
    /// history
    fn line_or_history(&mut self, up: bool) {
        let start = self.line_start();
        let column = self.cursor - start;
        if up && start > 0 {
            let above = self.input[..start - 1]
                .iter()
                .rposition(|&c| c == '\n')
                .map_or(0, |i| i + 1);
            self.cursor = (above + column).min(start - 1);
        } else if !up && self.line_end() < self.input.len() {
            let below = self.line_end() + 1;
            let below_end = self.input[below..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(self.input.len(), |i| below + i);
            self.cursor = (below + column).min(below_end);
        } else {
            self.history_step(up);
        }
    }

    fn history_step(&mut self, back: bool) {
        if self.history.is_empty() {
            return;
        }
        let pos = match (self.history_pos, back) {
            (None, true) => Some(self.history.len() - 1),
            (None, false) => None,
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) if p + 1 < self.history.len() => Some(p + 1),
            (Some(_), false) => None,
        };
        self.history_pos = pos;
        self.input = pos.map_or_else(Vec::new, |p| self.history[p].chars().collect());
        self.cursor = self.input.len();
    }

    fn search_start(&mut self) {
        self.search = Some(Search {
            query: String::new(),
            found: None,
            failed: false,
            saved: (self.input.clone(), self.cursor),
        });
    }

    /// A key while searching; false: the search ends, the key edits the
    /// form found (or the app runs it: Enter)
    fn search_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(search) = self.search.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Char('r') if ctrl => {
                let older = search.found.unwrap_or(self.history.len());
                self.search_from(older, false);
            }
            KeyCode::Char('s') if ctrl => {
                let newer = search.found.map_or(self.history.len(), |i| i + 1);
                self.search_from(newer, true);
            }
            KeyCode::Char('g' | 'c') if ctrl => {
                let (input, cursor) = search.saved.clone();
                self.input = input;
                self.cursor = cursor;
                self.search = None;
            }
            // Esc keeps the form found, to edit it
            KeyCode::Esc => self.search = None,
            KeyCode::Backspace => {
                search.query.pop();
                self.search_from(self.history.len(), false);
            }
            KeyCode::Char(c) if !ctrl && !key.modifiers.contains(KeyModifiers::ALT) => {
                search.query.push(c);
                // Still the same entry if it has the longer query
                let from = search.found.map_or(self.history.len(), |i| i + 1);
                self.search_from(from, false);
            }
            _ => {
                self.search = None;
                return false;
            }
        }
        true
    }

    /// The next history entry with the query, older than `from` (or newer
    /// from it on, `forward`), shown in the input
    fn search_from(&mut self, from: usize, forward: bool) {
        let Some(search) = self.search.as_mut() else {
            return;
        };
        let query = search.query.to_lowercase();
        let matches = |entry: &String| entry.to_lowercase().contains(&query);
        let found = if forward {
            (from..self.history.len()).find(|&i| matches(&self.history[i]))
        } else {
            (0..from.min(self.history.len()))
                .rev()
                .find(|&i| matches(&self.history[i]))
        };
        match found {
            Some(i) => {
                search.found = Some(i);
                search.failed = false;
                let entry = self.history[i].to_lowercase();
                let at = entry.find(&query).unwrap_or(0);
                self.input = self.history[i].chars().collect();
                self.cursor = entry[..at].chars().count();
                self.history_pos = Some(i);
            }
            None => search.failed = !query.is_empty(),
        }
    }
}

/// Reads a line in a plain terminal (not a scopekit app) with the
/// [`LineEditor`]'s keys: raw mode for the time of the call, the prompt
/// and the input drawn inline. Enter returns the input when `complete`
/// says it is (else a new line, as for an open Lisp form; Alt-Enter
/// always); Tab completes from `names`. `None`: Ctrl-D on an empty line.
/// Ctrl-C returns an empty line.
pub fn read_line(
    editor: &mut LineEditor,
    prompt: &str,
    complete: impl Fn(&str) -> bool,
    names: &[String],
) -> std::io::Result<Option<String>> {
    use crossterm::terminal;
    terminal::enable_raw_mode()?;
    let result = read_line_raw(editor, prompt, &complete, names);
    terminal::disable_raw_mode()?;
    println!();
    result
}

fn read_line_raw(
    editor: &mut LineEditor,
    prompt: &str,
    complete: &dyn Fn(&str) -> bool,
    names: &[String],
) -> std::io::Result<Option<String>> {
    use crossterm::event::{self, Event, KeyEventKind};
    let mut screen = Inline::default();
    let mut note = String::new();
    loop {
        let shown = editor.search_prompt().unwrap_or_else(|| note.clone());
        screen.draw(editor, prompt, &shown)?;
        note.clear();
        let key = match event::read()? {
            Event::Key(key) if key.kind != KeyEventKind::Release => key,
            Event::Paste(text) => {
                editor.insert_str(&text);
                continue;
            }
            _ => continue,
        };
        if editor.key(key) {
            continue;
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Enter | KeyCode::Char('j' | 'm') if key.code == KeyCode::Enter || ctrl => {
                if !alt && complete(&editor.text()) {
                    editor.cursor = editor.input.len();
                    screen.draw(editor, prompt, "")?;
                    return Ok(Some(editor.take()));
                }
                editor.insert('\n');
                editor.insert(' ');
                editor.insert(' ');
            }
            KeyCode::Tab => match editor.complete(names) {
                Completion::NoMatch(prefix) => note = format!("no name starts with {prefix}"),
                Completion::Several(found) => {
                    let mut shown: Vec<&str> = found.iter().take(12).map(String::as_str).collect();
                    if found.len() > 12 {
                        shown.push("...");
                    }
                    note = shown.join("  ");
                }
                Completion::Nothing | Completion::Unique => {}
            },
            KeyCode::Char('c') if ctrl => {
                editor.cursor = editor.input.len();
                screen.draw(editor, prompt, "")?;
                editor.clear();
                return Ok(Some(String::new()));
            }
            KeyCode::Char('d') if ctrl => return Ok(None),
            KeyCode::Char('l') if ctrl => {
                use crossterm::{cursor::MoveTo, execute, terminal::Clear, terminal::ClearType};
                execute!(std::io::stdout(), Clear(ClearType::All), MoveTo(0, 0))?;
                screen = Inline::default();
            }
            KeyCode::Esc => editor.clear(),
            _ => {}
        }
    }
}

/// The rows the inline input takes on the screen, to redraw it in place
#[derive(Default)]
struct Inline {
    /// Rows from the first row of the input to the cursor's
    cursor_row: u16,
}

impl Inline {
    fn draw(&mut self, editor: &LineEditor, prompt: &str, note: &str) -> std::io::Result<()> {
        use crossterm::cursor::{MoveToColumn, MoveUp};
        use crossterm::style::Print;
        use crossterm::terminal::{self, Clear, ClearType};
        use crossterm::{queue, QueueableCommand};
        use std::io::Write;
        let width = terminal::size()
            .map(|(w, _)| w.max(10) as usize)
            .unwrap_or(80);
        let indent = " ".repeat(prompt.chars().count());
        let mut out = std::io::stdout();
        if self.cursor_row > 0 {
            out.queue(MoveUp(self.cursor_row))?;
        }
        queue!(out, MoveToColumn(0), Clear(ClearType::FromCursorDown))?;
        // Each line after the prompt or its indent; long lines wrap
        let (mut row, mut cursor_at) = (0usize, (0usize, 0usize));
        let mut index = 0;
        let text = editor.text();
        for (n, line) in text.split('\n').enumerate() {
            let lead = if n == 0 { prompt } else { indent.as_str() };
            if n > 0 {
                out.queue(Print("\r\n"))?;
                row += 1;
            }
            out.queue(Print(lead))?;
            let mut column = lead.chars().count();
            for c in line.chars() {
                if index == editor.cursor {
                    cursor_at = (row, column);
                }
                if column == width {
                    out.queue(Print("\r\n"))?;
                    row += 1;
                    column = 0;
                }
                out.queue(Print(c))?;
                column += 1;
                index += 1;
            }
            if index == editor.cursor {
                cursor_at = if column == width {
                    (row + 1, 0)
                } else {
                    (row, column)
                };
            }
            index += 1;
        }
        if !note.is_empty() {
            let note: String = note.chars().take(width.saturating_sub(1)).collect();
            out.queue(Print(format!("\r\n{note}")))?;
            row += 1;
        }
        // Back to the cursor
        let up = row.saturating_sub(cursor_at.0);
        if up > 0 {
            out.queue(MoveUp(up as u16))?;
        }
        out.queue(MoveToColumn(cursor_at.1 as u16))?;
        out.flush()?;
        self.cursor_row = cursor_at.0 as u16;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(line: &mut LineEditor, keys: &str) {
        // "^x" Ctrl-x, "~x" Alt-x, anything else typed
        let mut chars = keys.chars();
        while let Some(c) = chars.next() {
            let (code, modifiers) = match c {
                '^' => (KeyCode::Char(chars.next().unwrap()), KeyModifiers::CONTROL),
                '~' => (KeyCode::Char(chars.next().unwrap()), KeyModifiers::ALT),
                c => (KeyCode::Char(c), KeyModifiers::NONE),
            };
            line.key(KeyEvent::new(code, modifiers));
        }
    }

    #[test]
    fn edits_like_a_shell() {
        let mut a = LineEditor::new();
        press(&mut a, "(setq abc 12)^a");
        assert_eq!(a.cursor, 0);
        press(&mut a, "^e^b^b^b");
        assert_eq!(a.cursor, 10);
        // Words: Lisp symbols, parentheses aren't part of them
        press(&mut a, "~b");
        assert_eq!(a.cursor, 6);
        press(&mut a, "~f~f");
        assert_eq!(a.cursor, 12);
        press(&mut a, "^a^d");
        assert_eq!(a.text(), "setq abc 12)");
        press(&mut a, "^e^h");
        assert_eq!(a.text(), "setq abc 12");
        // Ctrl-W twice cuts both words, Ctrl-Y puts them back
        press(&mut a, "^w^w");
        assert_eq!(a.text(), "setq ");
        press(&mut a, "^a^k");
        assert_eq!(a.text(), "");
        press(&mut a, "^y");
        assert_eq!(a.text(), "setq ");
        press(&mut a, "^u^y");
        assert_eq!(a.text(), "setq ");
        press(&mut a, "^a~d");
        assert_eq!(a.text(), " ");
        // Option characters of a window (German [ { @ ...) are typed
        a.key(KeyEvent::new(KeyCode::Char('['), KeyModifiers::ALT));
        a.key(KeyEvent::new(KeyCode::Char('∫'), KeyModifiers::ALT));
        assert_eq!(a.text(), "[∫ ");
        // Ctrl-D: a delete on a line, the app's (quit) on an empty one
        press(&mut a, "^a^k");
        assert!(!a.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL)));
        assert!(!a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn transposes_cases_and_undoes() {
        let mut a = LineEditor::new();
        press(&mut a, "(setq ab)^t");
        assert_eq!(a.text(), "(setq a)b");
        press(&mut a, "^a~u");
        assert_eq!(a.text(), "(SETQ a)b");
        press(&mut a, "~b~c");
        assert_eq!(a.text(), "(Setq a)b");
        press(&mut a, "^_");
        assert_eq!(a.text(), "(SETQ a)b");
        press(&mut a, "^_^_");
        assert_eq!(a.text(), "(setq ab)");
        // Typed letters in a row undo together, a blank ends the word
        press(&mut a, "^_");
        assert_eq!(a.text(), "(setq ");
        press(&mut a, "^_");
        assert_eq!(a.text(), "(setq");
        press(&mut a, "^_");
        assert_eq!(a.text(), "");
    }

    #[test]
    fn moves_between_lines_then_history() {
        let mut a = LineEditor::new();
        a.set_history(vec!["(old)".into()]);
        a.set_text("(a\n  bcd)");
        a.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(a.cursor, 2, "same column, clipped to the line above");
        a.key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(a.text(), "(old)");
    }

    #[test]
    fn searches_the_history() {
        let mut a = LineEditor::new();
        a.set_history(vec![
            "(setq x 1)".into(),
            "(command \"LINE\")".into(),
            "(setq y 2)".into(),
        ]);
        press(&mut a, "^rsetq");
        assert_eq!(a.text(), "(setq y 2)");
        assert_eq!(a.cursor, 1);
        press(&mut a, "^r");
        assert_eq!(a.text(), "(setq x 1)");
        press(&mut a, "^r");
        assert!(a.search_prompt().unwrap().starts_with("failing"));
        assert_eq!(a.text(), "(setq x 1)");
        // Any other key ends the search and edits the entry found
        press(&mut a, "^e");
        assert!(!a.searching());
        assert_eq!(a.cursor, a.input.len());
        // Ctrl-G puts the input back
        press(&mut a, "^rline^g");
        assert_eq!(a.text(), "(setq x 1)");
        // Enter is the app's: it runs the entry found
        press(&mut a, "^rcomm");
        assert!(!a.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(a.take(), "(command \"LINE\")");
        assert_eq!(a.history().last().unwrap(), "(command \"LINE\")");
    }

    #[test]
    fn completes_names() {
        let names: Vec<String> = ["setq", "setvar", "strcat"].map(String::from).to_vec();
        let mut a = LineEditor::new();
        a.set_text("(se");
        assert_eq!(
            a.complete(&names),
            Completion::Several(vec!["setq".into(), "setvar".into()])
        );
        assert_eq!(a.text(), "(set");
        a.set_text("(st");
        assert_eq!(a.complete(&names), Completion::Unique);
        assert_eq!(a.text(), "(strcat ");
        a.set_text("(zz");
        assert_eq!(a.complete(&names), Completion::NoMatch("zz".into()));
    }
}
