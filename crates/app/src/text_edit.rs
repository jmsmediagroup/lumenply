//! On-canvas text editing with the Text tool, as in Photoshop: a blinking
//! caret inside the layer itself, typing, keyboard navigation and
//! selection, mouse selection (drag, double-click a word, triple-click a
//! line, quadruple-click everything), the system clipboard, paragraph
//! boxes with resize handles, and Cmd-drag to move the text.
//!
//! Every change runs `SetText` coalesced under the session's own key, so a
//! whole session undoes as one step; new text closed with nothing typed
//! leaves no layer and no history step behind. The canvas holds egui's
//! keyboard focus while the session takes keys, so tool shortcuts (V, B,
//! T...) type letters instead of switching tools.

use lumenply_render::text_layout::{self as tl, layout, TextLayout};

use super::*;
use crate::text_ui::{text_click_action, TextClick};

/// The editable state: text plus caret and selection anchor, as byte
/// offsets at character boundaries.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Buffer {
    pub text: String,
    pub caret: usize,
    pub anchor: usize,
}

impl Buffer {
    pub fn new(text: &str, caret: usize) -> Self {
        let caret = caret.min(text.len());
        Buffer {
            text: text.to_string(),
            caret,
            anchor: caret,
        }
    }

    /// The selection as an ordered byte range.
    pub fn range(&self) -> (usize, usize) {
        (self.caret.min(self.anchor), self.caret.max(self.anchor))
    }

    pub fn has_selection(&self) -> bool {
        self.caret != self.anchor
    }

    pub fn selected(&self) -> &str {
        let (a, b) = self.range();
        &self.text[a..b]
    }

    /// Replace the selection with `s` (or insert it at the caret).
    pub fn insert(&mut self, s: &str) {
        let (a, b) = self.range();
        self.text.replace_range(a..b, s);
        self.caret = a + s.len();
        self.anchor = self.caret;
    }

    /// Delete the selection, or else the text between the caret and `to`
    /// (a position on either side of it).
    pub fn delete_to(&mut self, to: usize) {
        if !self.has_selection() {
            self.anchor = to.min(self.text.len());
        }
        self.insert("");
    }

    /// Move the caret; `extend` keeps the anchor (Shift).
    pub fn move_to(&mut self, i: usize, extend: bool) {
        self.caret = i.min(self.text.len());
        if !extend {
            self.anchor = self.caret;
        }
    }

    /// Select from `anchor` to `caret`.
    pub fn select(&mut self, anchor: usize, caret: usize) {
        self.anchor = anchor.min(self.text.len());
        self.caret = caret.min(self.text.len());
    }

    /// Clamp both ends to the text and to character boundaries (after the
    /// text changed underneath, e.g. from the Properties field).
    pub fn clamp(&mut self) {
        let fix = |i: usize, s: &str| {
            let mut i = i.min(s.len());
            while !s.is_char_boundary(i) {
                i -= 1;
            }
            i
        };
        self.caret = fix(self.caret, &self.text);
        self.anchor = fix(self.anchor, &self.text);
    }
}

/// A caret movement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Nav {
    Left,
    Right,
    WordLeft,
    WordRight,
    LineStart,
    LineEnd,
    Up,
    Down,
    TextStart,
    TextEnd,
}

/// What a key does while editing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum KeyAction {
    /// Move the caret; `true` extends the selection (Shift).
    Move(Nav, bool),
    /// Delete the selection or back/forward to where the movement lands.
    Delete(Nav),
    Insert(&'static str),
    SelectAll,
    Undo,
    Redo,
    /// Esc, Cmd+Enter: finish editing.
    Commit,
}

/// Map a key press to an editing action, with the platform's conventions:
/// on macOS Option jumps words and Cmd jumps to line ends; elsewhere Ctrl
/// jumps words and Home/End reach line ends.
pub(crate) fn key_action(key: Key, m: egui::Modifiers, mac: bool) -> Option<KeyAction> {
    use KeyAction as A;
    let word = if mac { m.alt } else { m.ctrl };
    let line = mac && m.command;
    let shift = m.shift;
    Some(match key {
        Key::ArrowLeft if line => A::Move(Nav::LineStart, shift),
        Key::ArrowLeft if word => A::Move(Nav::WordLeft, shift),
        Key::ArrowLeft => A::Move(Nav::Left, shift),
        Key::ArrowRight if line => A::Move(Nav::LineEnd, shift),
        Key::ArrowRight if word => A::Move(Nav::WordRight, shift),
        Key::ArrowRight => A::Move(Nav::Right, shift),
        Key::ArrowUp if line => A::Move(Nav::TextStart, shift),
        Key::ArrowUp => A::Move(Nav::Up, shift),
        Key::ArrowDown if line => A::Move(Nav::TextEnd, shift),
        Key::ArrowDown => A::Move(Nav::Down, shift),
        Key::Home if m.command => A::Move(Nav::TextStart, shift),
        Key::Home => A::Move(Nav::LineStart, shift),
        Key::End if m.command => A::Move(Nav::TextEnd, shift),
        Key::End => A::Move(Nav::LineEnd, shift),
        Key::Backspace if line => A::Delete(Nav::LineStart),
        Key::Backspace if word => A::Delete(Nav::WordLeft),
        Key::Backspace => A::Delete(Nav::Left),
        Key::Delete if word => A::Delete(Nav::WordRight),
        Key::Delete => A::Delete(Nav::Right),
        Key::Enter if m.command => A::Commit,
        Key::Enter => A::Insert("\n"),
        Key::Escape => A::Commit,
        Key::Tab if !m.command => A::Insert("\t"),
        Key::A if m.command => A::SelectAll,
        Key::Z if m.command && m.shift => A::Redo,
        Key::Z if m.command => A::Undo,
        Key::Y if m.command && !mac => A::Redo,
        _ => return None,
    })
}

/// Where `nav` takes the caret from `i`. Up/Down keep a goal x across a
/// run of vertical moves (returned for the next move); other moves drop it.
pub(crate) fn nav_target(
    text: &str,
    lay: &TextLayout,
    i: usize,
    nav: Nav,
    goal_x: Option<f32>,
) -> (usize, Option<f32>) {
    let line = lay.caret(i).line;
    match nav {
        Nav::Left => (tl::prev_char(text, i), None),
        Nav::Right => (tl::next_char(text, i), None),
        Nav::WordLeft => (tl::prev_word(text, i), None),
        Nav::WordRight => (tl::next_word(text, i), None),
        Nav::LineStart => (lay.line_bounds(line).0, None),
        Nav::LineEnd => (lay.line_bounds(line).1, None),
        Nav::TextStart => (0, None),
        Nav::TextEnd => (text.len(), None),
        Nav::Up | Nav::Down => {
            let x = goal_x.unwrap_or_else(|| lay.caret(i).x);
            let target = match nav {
                Nav::Up if line == 0 => 0,
                Nav::Up => lay.hit_in_line(line - 1, x),
                _ if line + 1 >= lay.lines.len() => text.len(),
                _ => lay.hit_in_line(line + 1, x),
            };
            (target, Some(x))
        }
    }
}

/// Paste text the way the layer stores it: newlines only.
pub(crate) fn normalize_paste(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

/// A paragraph box's resize handle: which edges it moves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Handle {
    /// -1 moves the left edge, 1 the right, 0 neither.
    pub sx: i8,
    /// -1 moves the top edge, 1 the bottom, 0 neither.
    pub sy: i8,
}

impl Handle {
    pub const ALL: [Handle; 8] = [
        Handle { sx: -1, sy: -1 },
        Handle { sx: 0, sy: -1 },
        Handle { sx: 1, sy: -1 },
        Handle { sx: 1, sy: 0 },
        Handle { sx: 1, sy: 1 },
        Handle { sx: 0, sy: 1 },
        Handle { sx: -1, sy: 1 },
        Handle { sx: -1, sy: 0 },
    ];

    /// The handle's spot on box [x, y, w, h].
    pub fn at(self, b: [f32; 4]) -> (f32, f32) {
        let f = |s: i8, o: f32, l: f32| o + l * (s as f32 + 1.0) / 2.0;
        (f(self.sx, b[0], b[2]), f(self.sy, b[1], b[3]))
    }

    /// Drag this handle of `start` [x, y, w, h] by (dx, dy); the box never
    /// shrinks below `min` and the opposite edges stay put.
    pub fn resize(self, start: [f32; 4], dx: f32, dy: f32, min: f32) -> [f32; 4] {
        let [mut x, mut y, mut w, mut h] = start;
        match self.sx {
            -1 => {
                let nw = (w - dx).max(min);
                x += w - nw;
                w = nw;
            }
            1 => w = (w + dx).max(min),
            _ => {}
        }
        match self.sy {
            -1 => {
                let nh = (h - dy).max(min);
                y += h - nh;
                h = nh;
            }
            1 => h = (h + dy).max(min),
            _ => {}
        }
        [x, y, w, h]
    }

    fn cursor(self) -> egui::CursorIcon {
        use egui::CursorIcon as C;
        match (self.sx, self.sy) {
            (0, _) => C::ResizeVertical,
            (_, 0) => C::ResizeHorizontal,
            (a, b) if a == b => C::ResizeNwSe,
            _ => C::ResizeNeSw,
        }
    }
}

/// How a mouse drag inside the text selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Unit {
    Char,
    Word,
    Line,
}

/// What the pointer is doing in a session.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Grab {
    /// Selecting; `origin` is the word or line the drag began on.
    Select { unit: Unit, origin: (usize, usize) },
    /// Cmd-drag: moving the whole text from its start position.
    Move { from: (f32, f32), start: (f32, f32) },
    /// Dragging a paragraph box handle.
    Resize {
        handle: Handle,
        from: (f32, f32),
        start: [f32; 4],
    },
}

/// One open editing session.
pub(crate) struct TextSession {
    pub layer: LayerId,
    /// Working copy of the layer's text and style; mirrors the document.
    pub t: TextLayer,
    pub buf: Buffer,
    /// Layout of `t`, refreshed after every change.
    pub lay: TextLayout,
    /// Coalescing key shared by every change of this session.
    pub key: String,
    /// The session made the layer (closing it empty removes it).
    pub created: bool,
    /// The document tab the session belongs to.
    tab: usize,
    goal_x: Option<f32>,
    /// When the caret last moved: the blink restarts from here.
    moved_at: f64,
    grab: Option<Grab>,
    /// Last press: time, document position, click count.
    last_press: (f64, (f32, f32), u32),
    undo: Vec<Buffer>,
    redo: Vec<Buffer>,
    /// The last change was typing, so more typing joins its undo entry.
    typing: bool,
    focus_pending: bool,
}

impl TextSession {
    fn new(layer: LayerId, t: TextLayer, buf: Buffer, key: String, created: bool, tab: usize) -> Self {
        TextSession {
            layer,
            lay: layout(&t),
            t,
            buf,
            key,
            created,
            tab,
            goal_x: None,
            moved_at: 0.0,
            grab: None,
            last_press: (f64::MIN, (0.0, 0.0), 0),
            undo: Vec::new(),
            redo: Vec::new(),
            typing: false,
            focus_pending: true,
        }
    }

    /// [x, y, w, h] of the paragraph box, if this is box text.
    fn box_rect(&self) -> Option<[f32; 4]> {
        self.t.box_size.map(|[w, h]| [self.t.x, self.t.y, w, h])
    }

    /// Is document point p on the text (with a grab margin)?
    fn contains(&self, p: (f32, f32), margin: f32) -> bool {
        let [x0, y0, x1, y1] = match self.box_rect() {
            Some([x, y, w, h]) => [x, y, x + w, y + h],
            None => self.lay.bounds(),
        };
        p.0 >= x0 - margin && p.0 <= x1 + margin && p.1 >= y0 - margin && p.1 <= y1 + margin
    }
}

/// The Text tool's state between frames.
#[derive(Default)]
pub(crate) struct TypeTool {
    pub session: Option<TextSession>,
    /// A press on empty canvas: a click there starts point text, a drag
    /// from it draws a paragraph box.
    press: Option<(f32, f32)>,
    /// The canvas widget, so a session can take keyboard focus at once.
    canvas_id: Option<egui::Id>,
    /// Makes every session's coalescing key unique.
    serial: u64,
}

/// Where the caret goes when editing starts.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum EditStart {
    /// At the character nearest this document point.
    At(f32, f32),
    /// Everything selected (double-click on the layer thumbnail).
    All,
    /// After the last character.
    End,
}

/// Smallest paragraph box a drag creates or a handle leaves, doc px.
const MIN_BOX: f32 = 8.0;

impl App {
    /// A text session is open (it takes keys while the canvas has focus).
    pub(crate) fn text_editing(&self) -> bool {
        self.typer.session.is_some()
    }

    fn next_text_key(&mut self) -> String {
        self.typer.serial += 1;
        format!("type-{}", self.typer.serial)
    }

    fn focus_canvas(&mut self, ctx: &egui::Context) {
        if let (Some(id), Some(s)) = (self.typer.canvas_id, self.typer.session.as_mut()) {
            ctx.memory_mut(|m| m.request_focus(id));
            s.focus_pending = false;
        }
    }

    /// Start editing text layer `id` with the caret placed by `start`.
    pub(crate) fn begin_text_edit(&mut self, ctx: &egui::Context, id: LayerId, start: EditStart) {
        if self.typer.session.as_ref().is_some_and(|s| s.layer == id) {
            return;
        }
        self.commit_text_edit();
        let Some(t) = self.editor.doc().layer(id).and_then(|l| l.text_layer()).cloned() else {
            return;
        };
        self.set_active(Some(id));
        if let Some(why) = self.lock_block(layer_actions::LockNeed::Paint) {
            self.status = why.into();
            return;
        }
        self.tool = Tool::Text;
        self.text_new_armed = false;
        let lay = layout(&t);
        let mut buf = Buffer::new(&t.text, t.text.len());
        match start {
            EditStart::At(x, y) => buf.move_to(lay.hit(x, y), false),
            EditStart::All => buf.select(0, t.text.len()),
            EditStart::End => {}
        }
        let key = self.next_text_key();
        let mut s = TextSession::new(id, t, buf, key, false, self.cur_tab);
        s.moved_at = ctx.input(|i| i.time);
        self.typer.session = Some(s);
        self.focus_canvas(ctx);
        self.status = "Editing text · Esc, Cmd+Enter or a click outside commits".into();
    }

    /// Start new text: point text anchored at (x, y), or a paragraph box
    /// with its top-left corner there. Nothing stays unless something is
    /// typed before the session ends.
    pub(crate) fn begin_new_text(&mut self, ctx: &egui::Context, x: f32, y: f32, box_size: Option<[f32; 2]>) {
        self.commit_text_edit();
        // New text follows the style of the text layer in hand.
        self.adopt_active_text_style();
        let mut t = TextLayer::new("", x, y, self.text_size, linear_rgba(self.brush_rgb, 1.0));
        t.bold = self.text_bold;
        t.italic = self.text_italic;
        t.font = self.text_font.clone();
        t.align = self.text_align;
        t.box_size = box_size;
        if box_size.is_none() && t.align == TextAlign::Justify {
            t.align = TextAlign::Left;
        }
        let key = self.next_text_key();
        let id = self.editor.doc().next_id();
        self.run_coalescing(
            &AddTextLayer {
                text: t.clone(),
                above: self.active,
            },
            &key,
        );
        if self.editor.doc().layer(id).is_none() {
            return;
        }
        self.set_active(Some(id));
        self.tool = Tool::Text;
        self.text_new_armed = false;
        let mut s = TextSession::new(id, t, Buffer::default(), key, true, self.cur_tab);
        s.moved_at = ctx.input(|i| i.time);
        self.typer.session = Some(s);
        self.focus_canvas(ctx);
        self.status = if box_size.is_some() {
            "Paragraph text: type, then Esc or click outside to commit".into()
        } else {
            "Type your text · Esc, Cmd+Enter or a click outside commits".into()
        };
    }

    /// End the session. Empty text leaves nothing behind: new text is
    /// dropped without a history step, emptied text loses its layer.
    pub(crate) fn commit_text_edit(&mut self) {
        let Some(s) = self.typer.session.take() else {
            return;
        };
        let live = self
            .editor
            .doc()
            .layer(s.layer)
            .is_some_and(|l| l.text_layer().is_some());
        let open = self.editor.coalescing(&s.key);
        if live && s.t.text.trim().is_empty() {
            if s.created && open {
                self.editor.discard_coalescing(&s.key);
                self.below.note_change(self.editor.doc(), None);
                let r = self.editor.last_affected();
                self.mark(r);
                self.fix_active();
                self.status = "Empty text discarded".into();
                return;
            }
            if open {
                self.run_coalescing(&RemoveLayer { layer: s.layer }, &s.key);
            } else {
                self.run(&RemoveLayer { layer: s.layer });
            }
            self.fix_active();
            self.status = "Empty text layer removed".into();
        } else if live {
            self.status = "Text committed".into();
        }
        if self.editor.coalescing(&s.key) {
            self.editor.end_coalescing();
        }
    }

    /// Once a frame: close the session when the tool, the active layer or
    /// the document changed, and follow edits made to the layer elsewhere
    /// (the options bar, Properties, undo).
    pub(crate) fn text_edit_guard(&mut self, ctx: &egui::Context) {
        let Some(s) = &self.typer.session else {
            // A finished session hands the keyboard back to the shortcuts.
            if let Some(id) = self.typer.canvas_id {
                ctx.memory_mut(|m| m.surrender_focus(id));
            }
            return;
        };
        if s.tab != self.cur_tab || self.no_doc {
            // The run stays in that tab's history; nothing to finish here.
            self.typer.session = None;
            return;
        }
        let current = self
            .editor
            .doc()
            .layer(s.layer)
            .and_then(|l| l.text_layer())
            .cloned();
        let Some(current) = current else {
            self.typer.session = None; // deleted or rasterized
            return;
        };
        if self.tool != Tool::Text || self.active != Some(s.layer) {
            self.commit_text_edit();
            return;
        }
        if let Some(s) = self.typer.session.as_mut() {
            if current != s.t {
                s.buf.text = current.text.clone();
                s.buf.clamp();
                s.t = current;
                s.lay = layout(&s.t);
            }
        }
    }

    /// Push the session's working text into the document (coalesced).
    fn text_apply(&mut self) {
        let Some(s) = &self.typer.session else {
            return;
        };
        let (layer, text, key) = (s.layer, s.t.clone(), s.key.clone());
        self.run_coalescing(&SetText { layer, text }, &key);
        // A refused edit (a lock) leaves the document as it was: follow it.
        let doc_t = self
            .editor
            .doc()
            .layer(layer)
            .and_then(|l| l.text_layer())
            .cloned();
        if let (Some(s), Some(d)) = (self.typer.session.as_mut(), doc_t) {
            if d != s.t {
                s.buf.text = d.text.clone();
                s.buf.clamp();
                s.t = d;
            }
            s.lay = layout(&s.t);
        }
    }

    /// Change the text through `f`, recording an in-session undo step
    /// (consecutive typing shares one).
    fn text_change(&mut self, now: f64, typing: bool, f: impl FnOnce(&mut Buffer)) {
        let Some(s) = self.typer.session.as_mut() else {
            return;
        };
        let before = s.buf.clone();
        f(&mut s.buf);
        s.goal_x = None;
        s.moved_at = now;
        if s.buf.text == before.text {
            return;
        }
        if !(typing && s.typing) {
            s.undo.push(before);
        }
        s.redo.clear();
        s.typing = typing;
        s.t.text = s.buf.text.clone();
        self.text_apply();
    }

    fn text_restore(&mut self, now: f64, redo: bool) {
        let Some(s) = self.typer.session.as_mut() else {
            return;
        };
        let popped = if redo { s.redo.pop() } else { s.undo.pop() };
        let Some(b) = popped else {
            return;
        };
        let current = std::mem::replace(&mut s.buf, b);
        if redo {
            s.undo.push(current);
        } else {
            s.redo.push(current);
        }
        s.typing = false;
        s.moved_at = now;
        s.t.text = s.buf.text.clone();
        self.text_apply();
    }

    /// Handle this frame's keyboard, text and clipboard events.
    fn text_keys(&mut self, ctx: &egui::Context) {
        let (events, now) = ctx.input(|i| (i.events.clone(), i.time));
        let mac = cfg!(target_os = "macos");
        for e in events {
            if self.typer.session.is_none() {
                break;
            }
            match e {
                egui::Event::Text(s) | egui::Event::Ime(egui::ImeEvent::Commit(s))
                    if !s.is_empty() && s != "\n" && s != "\r" =>
                {
                    self.text_change(now, true, |b| b.insert(&s));
                }
                egui::Event::Paste(s) => {
                    let s = normalize_paste(&s);
                    self.text_change(now, false, |b| b.insert(&s));
                }
                egui::Event::Copy | egui::Event::Cut => {
                    let sel = self.typer.session.as_ref().map(|s| s.buf.selected().to_string());
                    if let Some(sel) = sel.filter(|s| !s.is_empty()) {
                        ctx.copy_text(sel);
                        if matches!(e, egui::Event::Cut) {
                            self.text_change(now, false, |b| b.insert(""));
                        }
                    }
                }
                egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } => {
                    if let Some(a) = key_action(key, modifiers, mac) {
                        self.text_key(now, a);
                    }
                }
                _ => {}
            }
        }
        // Nothing else this frame may act on these keys.
        ctx.input_mut(|i| {
            i.events.retain(|e| {
                !matches!(
                    e,
                    egui::Event::Text(_)
                        | egui::Event::Key { .. }
                        | egui::Event::Paste(_)
                        | egui::Event::Copy
                        | egui::Event::Cut
                        | egui::Event::Ime(_)
                )
            })
        });
    }

    fn text_key(&mut self, now: f64, a: KeyAction) {
        match a {
            KeyAction::Commit => self.commit_text_edit(),
            KeyAction::Insert(s) => self.text_change(now, s != "\n", |b| b.insert(s)),
            KeyAction::SelectAll => {
                if let Some(s) = self.typer.session.as_mut() {
                    s.buf.select(0, s.buf.text.len());
                    s.typing = false;
                    s.moved_at = now;
                }
            }
            KeyAction::Undo => self.text_restore(now, false),
            KeyAction::Redo => self.text_restore(now, true),
            KeyAction::Move(nav, extend) => {
                let Some(s) = self.typer.session.as_mut() else {
                    return;
                };
                s.typing = false;
                s.moved_at = now;
                // Left/Right without Shift first collapse a selection.
                if s.buf.has_selection() && !extend && matches!(nav, Nav::Left | Nav::Right) {
                    let (a, b) = s.buf.range();
                    s.buf.move_to(if nav == Nav::Left { a } else { b }, false);
                    s.goal_x = None;
                    return;
                }
                let (i, goal) = nav_target(&s.buf.text, &s.lay, s.buf.caret, nav, s.goal_x);
                s.buf.move_to(i, extend);
                s.goal_x = goal;
            }
            KeyAction::Delete(nav) => {
                let Some(s) = self.typer.session.as_ref() else {
                    return;
                };
                let (to, _) = nav_target(&s.buf.text, &s.lay, s.buf.caret, nav, None);
                self.text_change(now, false, |b| b.delete_to(to));
            }
        }
    }

    /// The Text tool's canvas input: presses, drags and releases, keys
    /// while editing, and Enter to edit the active text layer.
    pub(crate) fn type_tool_input(
        &mut self,
        ctx: &egui::Context,
        resp: &egui::Response,
        to_doc: &dyn Fn(Pos2) -> (f32, f32),
    ) {
        let primary = egui::PointerButton::Primary;
        self.typer.canvas_id = Some(resp.id);
        let (mods, now) = ctx.input(|i| (i.modifiers, i.time));

        // Keyboard focus: the session takes keys while the canvas has it.
        match self.typer.session.as_mut() {
            Some(s) if s.focus_pending => {
                resp.request_focus();
                s.focus_pending = false;
            }
            None if holds_keys(resp) => resp.surrender_focus(),
            _ => {}
        }
        // egui drops focus on an Esc it was not told to keep (the frame
        // right after focus moved): that Esc still means "commit".
        if self.typer.session.is_some()
            && ctx.memory(|m| m.focused().is_none())
            && ctx.input(|i| i.key_pressed(Key::Escape))
        {
            self.commit_text_edit();
        }
        if self.typer.session.is_some() && holds_keys(resp) {
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: true,
                    },
                )
            });
            self.text_keys(ctx);
        } else if self.typer.session.is_none()
            && !ctx.wants_keyboard_input()
            && self.active_text().is_some()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Enter))
        {
            // Enter edits the active text layer, caret at the end.
            if let Some(id) = self.active {
                self.begin_text_edit(ctx, id, EditStart::End);
            }
        }

        // The pointer.
        let hover = resp.hover_pos().map(to_doc);
        let grab_r = 6.0 / self.zoom.max(0.01);
        if let Some(p) = hover {
            let icon = match self.typer.session.as_ref() {
                Some(s) if s.grab.is_none() => match handle_at(s, p, grab_r) {
                    Some(h) => h.cursor(),
                    None if mods.command => egui::CursorIcon::Move,
                    None => egui::CursorIcon::Text,
                },
                Some(TextSession {
                    grab: Some(Grab::Move { .. }),
                    ..
                }) => egui::CursorIcon::Move,
                Some(TextSession {
                    grab: Some(Grab::Resize { handle, .. }),
                    ..
                }) => handle.cursor(),
                _ => egui::CursorIcon::Text,
            };
            ctx.set_cursor_icon(icon);
        }

        let pressed = resp.is_pointer_button_down_on() && ctx.input(|i| i.pointer.primary_pressed());
        if pressed {
            if let Some(p) = ctx.input(|i| i.pointer.press_origin()).map(to_doc) {
                // Re-requesting focus would reset its key filter (arrows,
                // Tab and Esc would leave the canvas for a frame).
                if self.text_press(ctx, p, mods, now, grab_r) && !holds_keys(resp) {
                    resp.request_focus();
                }
            }
        }
        if resp.dragged_by(primary) {
            if let Some(p) = resp.interact_pointer_pos().map(to_doc) {
                self.text_drag(p);
            }
        }
        if resp.clicked_by(primary) || resp.drag_stopped_by(primary) {
            if let Some(s) = self.typer.session.as_mut() {
                s.grab = None;
            }
            if let Some(p0) = self.typer.press.take() {
                let p1 = resp
                    .interact_pointer_pos()
                    .or_else(|| ctx.input(|i| i.pointer.latest_pos()))
                    .map(to_doc)
                    .unwrap_or(p0);
                let (w, h) = ((p1.0 - p0.0).abs(), (p1.1 - p0.1).abs());
                if resp.drag_stopped_by(primary) && w.max(h) * self.zoom >= 6.0 {
                    let (x, y) = (p0.0.min(p1.0), p0.1.min(p1.1));
                    self.begin_new_text(ctx, x, y, Some([w.max(MIN_BOX), h.max(MIN_BOX)]));
                } else {
                    self.begin_new_text(ctx, p0.0, p0.1, None);
                }
            }
        }

        if self.typer.session.is_none() && holds_keys(resp) {
            resp.surrender_focus();
        }
        // Caret blink and the IME candidate window follow the caret.
        if let Some(s) = &self.typer.session {
            if holds_keys(resp) {
                let c = s.lay.caret(s.buf.caret);
                let origin = resp.rect.min + self.pan;
                let at = |x: f32, y: f32| egui::pos2(origin.x + x * self.zoom, origin.y + y * self.zoom);
                let cursor_rect = egui::Rect::from_min_max(at(c.x, c.top), at(c.x + 1.0, c.bottom));
                ctx.output_mut(|o| {
                    o.ime = Some(egui::output::IMEOutput {
                        rect: resp.rect,
                        cursor_rect,
                    })
                });
                let phase = (now - s.moved_at).rem_euclid(1.0);
                let next = if phase < 0.6 { 0.6 - phase } else { 1.0 - phase };
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(next.max(0.016)));
            }
        }
    }

    /// A primary press at document point p. Returns true when a session
    /// takes (or keeps) the keyboard.
    fn text_press(
        &mut self,
        ctx: &egui::Context,
        p: (f32, f32),
        mods: egui::Modifiers,
        now: f64,
        grab_r: f32,
    ) -> bool {
        if let Some(s) = self.typer.session.as_mut() {
            if let (Some(h), Some(b)) = (handle_at(s, p, grab_r), s.box_rect()) {
                s.grab = Some(Grab::Resize {
                    handle: h,
                    from: p,
                    start: b,
                });
            } else if mods.command {
                s.grab = Some(Grab::Move {
                    from: p,
                    start: (s.t.x, s.t.y),
                });
            } else if s.contains(p, grab_r) {
                let (t0, p0, n0) = s.last_press;
                let again = now - t0 < 0.45 && (p.0 - p0.0).hypot(p.1 - p0.1) < grab_r;
                let count = if again { n0 % 4 + 1 } else { 1 };
                s.last_press = (now, p, count);
                s.moved_at = now;
                s.goal_x = None;
                s.typing = false;
                let i = s.lay.hit(p.0, p.1);
                let unit = match count {
                    1 => {
                        s.buf.move_to(i, mods.shift);
                        Unit::Char
                    }
                    2 => {
                        let (a, b) = tl::word_at(&s.buf.text, i);
                        s.buf.select(a, b);
                        Unit::Word
                    }
                    3 => {
                        let l = &s.lay.lines[s.lay.line_at_y(p.1)];
                        let (a, b) = (l.start, l.end);
                        s.buf.select(a, b);
                        Unit::Line
                    }
                    _ => {
                        s.buf.select(0, s.buf.text.len());
                        Unit::Line
                    }
                };
                s.grab = Some(Grab::Select {
                    unit,
                    origin: s.buf.range(),
                });
            } else {
                // A press outside the text commits; it starts nothing new.
                self.commit_text_edit();
                return false;
            }
            return true;
        }

        let force_new = mods.shift || std::mem::take(&mut self.text_new_armed);
        match text_click_action(self.editor.doc(), p.0, p.1, force_new) {
            TextClick::Edit(id) => {
                self.begin_text_edit(ctx, id, EditStart::At(p.0, p.1));
                if let Some(s) = self.typer.session.as_mut() {
                    s.last_press = (now, p, 1);
                    s.grab = Some(Grab::Select {
                        unit: Unit::Char,
                        origin: s.buf.range(),
                    });
                }
            }
            // Point text on release, or a paragraph box if it drags.
            TextClick::New => self.typer.press = Some(p),
        }
        self.typer.session.is_some()
    }

    fn text_drag(&mut self, p: (f32, f32)) {
        let Some(s) = self.typer.session.as_mut() else {
            return;
        };
        match s.grab {
            Some(Grab::Select { unit, origin }) => {
                let i = s.lay.hit(p.0, p.1);
                let (a, b) = match unit {
                    Unit::Char => (i, i),
                    Unit::Word => tl::word_at(&s.buf.text, i),
                    Unit::Line => {
                        let l = &s.lay.lines[s.lay.line_at_y(p.1)];
                        (l.start, l.end)
                    }
                };
                if unit == Unit::Char {
                    s.buf.caret = i;
                } else if a < origin.0 {
                    s.buf.select(origin.1, a);
                } else {
                    s.buf.select(origin.0, b.max(origin.1));
                }
            }
            Some(Grab::Move { from, start }) => {
                s.t.x = start.0 + (p.0 - from.0).round();
                s.t.y = start.1 + (p.1 - from.1).round();
                self.text_apply();
            }
            Some(Grab::Resize { handle, from, start }) => {
                let [x, y, w, h] = handle.resize(start, p.0 - from.0, p.1 - from.1, MIN_BOX);
                s.t.x = x;
                s.t.y = y;
                s.t.box_size = Some([w, h]);
                self.text_apply();
            }
            None => {}
        }
    }

    /// The session overlay: text bounds or the paragraph box with its
    /// handles, the selection highlight and the blinking caret. Returns
    /// false when no session is open.
    pub(crate) fn paint_text_session(
        &self,
        ctx: &egui::Context,
        painter: &egui::Painter,
        resp: &egui::Response,
    ) -> bool {
        let origin = resp.rect.min + self.pan;
        let zoom = self.zoom;
        let at = |x: f32, y: f32| egui::pos2(origin.x + x * zoom, origin.y + y * zoom);
        let Some(s) = &self.typer.session else {
            // A paragraph box being drawn.
            if let (Some(p0), true) = (self.typer.press, resp.dragged()) {
                if let Some(q) = resp.interact_pointer_pos() {
                    let r = egui::Rect::from_two_pos(at(p0.0, p0.1), q);
                    painter.rect_stroke(r, 0.0, Stroke::new(3.0, Color32::from_black_alpha(110)));
                    painter.rect_stroke(r, 0.0, Stroke::new(1.0, ACCENT));
                }
            }
            return false;
        };
        let focused = holds_keys(resp);
        let ink = Stroke::new(3.0, Color32::from_black_alpha(110));
        // Selection under the caret.
        let (a, b) = s.buf.range();
        let fill = Color32::from_rgba_unmultiplied(0x47, 0x8C, 0xFF, if focused { 110 } else { 60 });
        for [x0, y0, x1, y1] in s.lay.selection_rects(a, b) {
            painter.rect_filled(egui::Rect::from_min_max(at(x0, y0), at(x1, y1)), 0.0, fill);
        }
        // The text's frame.
        match s.box_rect() {
            Some([x, y, w, h]) => {
                let r = egui::Rect::from_min_max(at(x, y), at(x + w, y + h));
                painter.rect_stroke(r, 0.0, ink);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, ACCENT));
                for hd in Handle::ALL {
                    let (hx, hy) = hd.at([x, y, w, h]);
                    let hr = egui::Rect::from_center_size(at(hx, hy), egui::vec2(7.0, 7.0));
                    painter.rect_filled(hr, 1.0, Color32::WHITE);
                    painter.rect_stroke(hr, 1.0, Stroke::new(1.0, Color32::from_black_alpha(200)));
                }
                // Overflow: Photoshop's plus in the bottom-right handle.
                if s.lay.overflows() {
                    let c = at(x + w, y + h) + egui::vec2(12.0, 12.0);
                    let hr = egui::Rect::from_center_size(c, egui::vec2(11.0, 11.0));
                    painter.rect_filled(hr, 1.0, Color32::WHITE);
                    painter.rect_stroke(hr, 1.0, Stroke::new(1.0, DANGER));
                    let k = Stroke::new(1.5, DANGER);
                    painter.line_segment([c - egui::vec2(3.0, 0.0), c + egui::vec2(3.0, 0.0)], k);
                    painter.line_segment([c - egui::vec2(0.0, 3.0), c + egui::vec2(0.0, 3.0)], k);
                }
            }
            None => {
                let [x0, y0, x1, y1] = s.lay.bounds();
                let r = egui::Rect::from_min_max(at(x0, y0), at(x1, y1)).expand(3.0);
                painter.rect_stroke(r, 0.0, Stroke::new(1.0, ACCENT.gamma_multiply(0.8)));
                // The anchor: where the first baseline starts (or centres).
                let a = at(s.t.x, s.t.y);
                painter.rect_filled(egui::Rect::from_center_size(a, egui::vec2(5.0, 5.0)), 1.0, ACCENT);
            }
        }
        // The caret: a two-tone bar that shows on light and dark pixels.
        let now = ctx.input(|i| i.time);
        let blink_on = (now - s.moved_at).rem_euclid(1.0) < 0.6;
        if focused && blink_on && s.grab.is_none_or(|g| matches!(g, Grab::Select { .. })) {
            let c = s.lay.caret(s.buf.caret);
            let (top, bottom) = (at(c.x, c.top), at(c.x, c.bottom));
            painter.line_segment([top, bottom], Stroke::new(3.0, Color32::from_black_alpha(170)));
            painter.line_segment([top, bottom], Stroke::new(1.5, Color32::WHITE));
        }
        true
    }

    /// Debug tokens for editing (`text:...`): `text:box=X:Y:W:H` starts a
    /// paragraph box, `text:edit` edits the active text layer (caret at the
    /// end), `text:select=A:B` selects byte range A..B, `text:caret=I`
    /// places the caret, `text:commit` ends the session, `text:leading=N`,
    /// `text:size=N`, `text:shift=N`, `text:caps`, `text:justify` restyle the
    /// active text (`size` also sets the size for new text).
    pub(crate) fn debug_text_edit(&mut self, ctx: &egui::Context, rest: &str) -> bool {
        let nums = |s: &str| -> Vec<f32> { s.split(':').filter_map(|v| v.trim().parse().ok()).collect() };
        if let Some(arg) = rest.strip_prefix("box=") {
            if let [x, y, w, h] = nums(arg)[..] {
                self.tool = Tool::Text;
                self.begin_new_text(ctx, x, y, Some([w, h]));
            }
        } else if rest == "edit" {
            if let Some(id) = self.active {
                self.begin_text_edit(ctx, id, EditStart::End);
            }
        } else if let Some(arg) = rest.strip_prefix("select=") {
            if let (Some(s), [a, b]) = (self.typer.session.as_mut(), &nums(arg)[..]) {
                s.buf.select(*a as usize, *b as usize);
                s.buf.clamp();
            }
        } else if let Some(arg) = rest.strip_prefix("caret=") {
            if let (Some(s), [i]) = (self.typer.session.as_mut(), &nums(arg)[..]) {
                s.buf.move_to(*i as usize, false);
                s.buf.clamp();
            }
        } else if rest == "commit" {
            self.commit_text_edit();
        } else {
            let num = |p: &str, d: f32| rest.strip_prefix(p).map(|v| v.parse::<f32>().unwrap_or(d));
            let restyle = |t: &mut TextLayer| -> bool {
                if let Some(v) = num("leading=", 1.2) {
                    t.line_height = v;
                } else if let Some(v) = num("size=", 72.0) {
                    t.size = v;
                } else if let Some(v) = num("shift=", 0.0) {
                    t.baseline_shift = v;
                } else if rest == "caps" {
                    t.all_caps = !t.all_caps;
                } else if rest == "justify" {
                    t.align = TextAlign::Justify;
                } else {
                    return false;
                }
                true
            };
            let mut probe = TextLayer::new("", 0.0, 0.0, 1.0, [0.0; 4]);
            if !restyle(&mut probe) {
                return false;
            }
            if let Some(v) = num("size=", 72.0) {
                self.text_size = v;
            }
            if let (Some(id), Some(mut t)) = (self.active, self.active_text()) {
                restyle(&mut t);
                self.run(&SetText { layer: id, text: t });
                self.text_edit_guard(ctx);
            }
        }
        true
    }
}

/// The canvas holds egui's keyboard focus. Unlike `Response::has_focus`
/// this ignores whether the window itself is focused, so a headless
/// screenshot run shows the caret too (no keys arrive unfocused anyway).
fn holds_keys(resp: &egui::Response) -> bool {
    resp.ctx.memory(|m| m.has_focus(resp.id))
}

/// The paragraph-box handle within `r` (doc px) of document point p.
fn handle_at(s: &TextSession, p: (f32, f32), r: f32) -> Option<Handle> {
    let b = s.box_rect()?;
    Handle::ALL.into_iter().find(|h| {
        let (hx, hy) = h.at(b);
        (p.0 - hx).abs() <= r && (p.1 - hy).abs() <= r
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Modifiers as M;

    const BLACK: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

    #[test]
    fn buffer_inserts_replaces_and_deletes() {
        let mut b = Buffer::new("Hello", 5);
        b.insert(" world");
        assert_eq!((b.text.as_str(), b.caret, b.anchor), ("Hello world", 11, 11));
        // Typing over a selection replaces it.
        b.select(0, 5);
        assert_eq!(b.selected(), "Hello");
        b.insert("Hi");
        assert_eq!((b.text.as_str(), b.caret), ("Hi world", 2));
        // Backspace over a two-byte character, then forward delete.
        let mut b = Buffer::new("aé", 3);
        let to = tl::prev_char(&b.text, b.caret);
        b.delete_to(to);
        assert_eq!((b.text.as_str(), b.caret), ("a", 1));
        let mut b = Buffer::new("abc", 1);
        b.delete_to(2);
        assert_eq!((b.text.as_str(), b.caret), ("ac", 1));
        // With a selection, delete takes the selection whatever the target.
        let mut b = Buffer::new("abcdef", 0);
        b.select(4, 1);
        b.delete_to(0);
        assert_eq!((b.text.as_str(), b.caret, b.anchor), ("aef", 1, 1));
        // Clamp pulls stale positions back inside the text.
        let mut b = Buffer {
            text: "aé".into(),
            caret: 2,
            anchor: 9,
        };
        b.clamp();
        assert_eq!((b.caret, b.anchor), (1, 3));
    }

    #[test]
    fn keys_follow_the_platform_conventions() {
        let mac_alt = M { alt: true, ..M::NONE };
        let mac_cmd = M {
            mac_cmd: true,
            command: true,
            ..M::NONE
        };
        let shift = M::SHIFT;
        use KeyAction as A;
        assert_eq!(
            key_action(Key::ArrowLeft, M::NONE, true),
            Some(A::Move(Nav::Left, false))
        );
        assert_eq!(
            key_action(Key::ArrowLeft, shift, true),
            Some(A::Move(Nav::Left, true))
        );
        assert_eq!(
            key_action(Key::ArrowLeft, mac_alt, true),
            Some(A::Move(Nav::WordLeft, false))
        );
        assert_eq!(
            key_action(Key::ArrowRight, mac_cmd, true),
            Some(A::Move(Nav::LineEnd, false))
        );
        assert_eq!(
            key_action(Key::ArrowUp, mac_cmd, true),
            Some(A::Move(Nav::TextStart, false))
        );
        assert_eq!(
            key_action(Key::Backspace, mac_alt, true),
            Some(A::Delete(Nav::WordLeft))
        );
        assert_eq!(
            key_action(Key::Backspace, mac_cmd, true),
            Some(A::Delete(Nav::LineStart))
        );
        assert_eq!(key_action(Key::Enter, M::NONE, true), Some(A::Insert("\n")));
        assert_eq!(key_action(Key::Enter, mac_cmd, true), Some(A::Commit));
        assert_eq!(key_action(Key::Escape, M::NONE, true), Some(A::Commit));
        assert_eq!(key_action(Key::A, mac_cmd, true), Some(A::SelectAll));
        assert_eq!(key_action(Key::Z, mac_cmd, true), Some(A::Undo));
        assert_eq!(
            key_action(
                Key::Z,
                M {
                    shift: true,
                    ..mac_cmd
                },
                true
            ),
            Some(A::Redo)
        );
        // Tool letters are not editing keys: they arrive as typed text.
        for k in [Key::V, Key::B, Key::T, Key::Space] {
            assert_eq!(key_action(k, M::NONE, true), None, "{k:?}");
        }
        // Elsewhere: Ctrl jumps words, Home/End reach line ends, Ctrl+Y redoes.
        let ctrl = M {
            ctrl: true,
            command: true,
            ..M::NONE
        };
        assert_eq!(
            key_action(Key::ArrowRight, ctrl, false),
            Some(A::Move(Nav::WordRight, false))
        );
        assert_eq!(
            key_action(Key::Home, M::NONE, false),
            Some(A::Move(Nav::LineStart, false))
        );
        assert_eq!(
            key_action(Key::End, ctrl, false),
            Some(A::Move(Nav::TextEnd, false))
        );
        assert_eq!(key_action(Key::Y, ctrl, false), Some(A::Redo));
        assert_eq!(key_action(Key::Y, mac_cmd, true), None);
    }

    #[test]
    fn navigation_moves_by_character_word_and_line() {
        let t = TextLayer::new("one two\nthree", 0.0, 50.0, 20.0, BLACK);
        let lay = layout(&t);
        let text = t.text.as_str();
        assert_eq!(nav_target(text, &lay, 3, Nav::Left, None), (2, None));
        assert_eq!(nav_target(text, &lay, 3, Nav::Right, None), (4, None));
        assert_eq!(nav_target(text, &lay, 0, Nav::WordRight, None), (3, None));
        assert_eq!(nav_target(text, &lay, 7, Nav::WordRight, None), (13, None));
        assert_eq!(nav_target(text, &lay, 13, Nav::WordLeft, None), (8, None));
        assert_eq!(nav_target(text, &lay, 10, Nav::LineStart, None), (8, None));
        assert_eq!(nav_target(text, &lay, 2, Nav::LineEnd, None), (7, None));
        assert_eq!(nav_target(text, &lay, 4, Nav::TextEnd, None), (13, None));
        // Down from "one t|wo" keeps the x: in "three" the nearest stop.
        let x = lay.caret(5).x;
        let (down, goal) = nav_target(text, &lay, 5, Nav::Down, None);
        assert_eq!(goal, Some(x));
        assert_eq!(down, lay.hit_in_line(1, x));
        assert!((8..=13).contains(&down));
        // Up from the first line goes to the start, Down from the last to the end.
        assert_eq!(nav_target(text, &lay, 5, Nav::Up, None).0, 0);
        assert_eq!(nav_target(text, &lay, 9, Nav::Down, None).0, 13);
        // The goal x survives a short line in between.
        let (up, _) = nav_target(text, &lay, 13, Nav::Up, Some(x));
        assert_eq!(up, 5);
    }

    #[test]
    fn box_handles_resize_from_the_opposite_edge() {
        let b = [100.0, 50.0, 200.0, 80.0];
        let se = Handle { sx: 1, sy: 1 };
        assert_eq!(se.at(b), (300.0, 130.0));
        assert_eq!(se.resize(b, 20.0, -10.0, 8.0), [100.0, 50.0, 220.0, 70.0]);
        let nw = Handle { sx: -1, sy: -1 };
        assert_eq!(nw.at(b), (100.0, 50.0));
        assert_eq!(nw.resize(b, 30.0, 10.0, 8.0), [130.0, 60.0, 170.0, 70.0]);
        // Never thinner than the minimum; the right edge stays at 300.
        assert_eq!(nw.resize(b, 500.0, 0.0, 8.0), [292.0, 50.0, 8.0, 80.0]);
        let e = Handle { sx: 1, sy: 0 };
        assert_eq!(e.at(b), (300.0, 90.0));
        assert_eq!(e.resize(b, -50.0, 99.0, 8.0), [100.0, 50.0, 150.0, 80.0]);
        assert_eq!(normalize_paste("a\r\nb\rc"), "a\nb\nc");
    }

    // ---- the whole app: real frames, real key events ----------------------------

    /// A 240×160 document with "Hello" (24 px at 20, 80) on white, the
    /// Text tool up, no autosave while frames run.
    fn app() -> (App, egui::Context, LayerId) {
        let mut app = App::launch(&[]);
        app.dialog = None;
        app.last_autosave = std::time::Instant::now() + std::time::Duration::from_secs(24 * 3600);
        app.set_doc(blank(240, 160), None);
        let id = app.editor.doc().next_id();
        app.run(&AddTextLayer {
            text: TextLayer::new("Hello", 20.0, 80.0, 24.0, BLACK),
            above: None,
        });
        app.tool = Tool::Text;
        let ctx = crate::a11y_tests::ctx();
        frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        (app, ctx, id)
    }

    fn frame(app: &mut App, ctx: &egui::Context, events: Vec<egui::Event>, modifiers: egui::Modifiers) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1200.0, 800.0))),
            events,
            modifiers,
            ..Default::default()
        };
        let _ = ctx.run(raw, |ctx| app.frame(ctx));
    }

    fn key(key: Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn text_of(app: &App, id: LayerId) -> Option<String> {
        app.editor.doc().layer(id)?.text_layer().map(|t| t.text.clone())
    }

    #[test]
    fn typing_on_canvas_is_one_undo_step_and_tool_keys_type_letters() {
        let (mut app, ctx, id) = app();
        let steps = app.editor.history().len();
        app.begin_text_edit(&ctx, id, EditStart::End);
        frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        assert!(ctx.wants_keyboard_input(), "the canvas holds the keyboard");
        // "V", "B" and "T" are tool shortcuts: here they arrive as text.
        let typed = |s: &str, k: Key| vec![key(k, egui::Modifiers::NONE), egui::Event::Text(s.into())];
        for (s, k) in [(" ", Key::Space), ("V", Key::V), ("b", Key::B), ("t", Key::T)] {
            frame(&mut app, &ctx, typed(s, k), egui::Modifiers::NONE);
        }
        assert_eq!(app.tool, Tool::Text, "no tool switch while typing");
        assert_eq!(text_of(&app, id).as_deref(), Some("Hello Vbt"));
        // Backspace, then Shift+Left twice and a replacement.
        frame(
            &mut app,
            &ctx,
            vec![key(Key::Backspace, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        for _ in 0..2 {
            frame(
                &mut app,
                &ctx,
                vec![key(Key::ArrowLeft, egui::Modifiers::SHIFT)],
                egui::Modifiers::SHIFT,
            );
        }
        assert_eq!(app.typer.session.as_ref().unwrap().buf.range(), (6, 8));
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Text("!".into())],
            egui::Modifiers::NONE,
        );
        assert_eq!(text_of(&app, id).as_deref(), Some("Hello !"));
        // Esc commits: the whole session is one history step.
        frame(
            &mut app,
            &ctx,
            vec![key(Key::Escape, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert!(!app.text_editing());
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last().copied(), Some("Edit text"));
        assert!(app.editor.undo().is_some());
        assert_eq!(text_of(&app, id).as_deref(), Some("Hello"));
        // Without a session, T is the Text tool shortcut again.
        app.tool = Tool::Brush;
        frame(
            &mut app,
            &ctx,
            vec![key(Key::T, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert_eq!(app.tool, Tool::Text);
    }

    #[test]
    fn new_text_left_empty_leaves_no_layer_and_no_step() {
        let (mut app, ctx, _) = app();
        let (layers, steps) = (app.editor.doc().layer_count(), app.editor.history().len());
        app.begin_new_text(&ctx, 50.0, 120.0, None);
        assert_eq!(
            app.editor.doc().layer_count(),
            layers + 1,
            "the layer exists while editing"
        );
        frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        frame(
            &mut app,
            &ctx,
            vec![key(Key::Escape, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        assert_eq!(app.editor.doc().layer_count(), layers);
        assert_eq!(app.editor.history().len(), steps);
        assert!(!app.editor.can_redo());

        // Typed then committed: one "Add text" step holding the text.
        app.begin_new_text(&ctx, 50.0, 120.0, None);
        let id = app.active.unwrap();
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Text("Hi".into())],
            egui::Modifiers::NONE,
        );
        let cmd_enter = egui::Modifiers {
            mac_cmd: cfg!(target_os = "macos"),
            ctrl: !cfg!(target_os = "macos"),
            command: true,
            ..egui::Modifiers::NONE
        };
        frame(&mut app, &ctx, vec![key(Key::Enter, cmd_enter)], cmd_enter);
        assert!(!app.text_editing(), "Cmd+Enter commits");
        assert_eq!(text_of(&app, id).as_deref(), Some("Hi"));
        assert_eq!(app.editor.history().len(), steps + 1);
        assert_eq!(app.editor.history().last().copied(), Some("Add text"));
        // Emptying existing text and committing removes its layer, undoably.
        app.begin_text_edit(&ctx, id, EditStart::All);
        frame(
            &mut app,
            &ctx,
            vec![key(Key::Delete, egui::Modifiers::NONE)],
            egui::Modifiers::NONE,
        );
        app.commit_text_edit();
        assert!(app.editor.doc().layer(id).is_none());
        assert_eq!(app.editor.history().len(), steps + 2);
        app.editor.undo();
        assert_eq!(text_of(&app, id).as_deref(), Some("Hi"));
    }

    #[test]
    fn clipboard_select_all_and_in_session_undo() {
        let (mut app, ctx, id) = app();
        app.begin_text_edit(&ctx, id, EditStart::End);
        frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        let cmd = egui::Modifiers {
            mac_cmd: cfg!(target_os = "macos"),
            ctrl: !cfg!(target_os = "macos"),
            command: true,
            ..egui::Modifiers::NONE
        };
        // Cmd+A, Copy: the clipboard gets the whole text.
        frame(&mut app, &ctx, vec![key(Key::A, cmd)], cmd);
        assert_eq!(app.typer.session.as_ref().unwrap().buf.range(), (0, 5));
        let out = ctx.run(
            egui::RawInput {
                events: vec![egui::Event::Copy],
                ..Default::default()
            },
            |ctx| app.frame(ctx),
        );
        assert_eq!(out.platform_output.copied_text, "Hello");
        // Paste over the selection, with Windows line ends normalised.
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Paste("A\r\nB".into())],
            egui::Modifiers::NONE,
        );
        assert_eq!(text_of(&app, id).as_deref(), Some("A\nB"));
        // Cut the "B".
        frame(
            &mut app,
            &ctx,
            vec![key(Key::ArrowLeft, egui::Modifiers::SHIFT)],
            egui::Modifiers::SHIFT,
        );
        frame(&mut app, &ctx, vec![egui::Event::Cut], egui::Modifiers::NONE);
        assert_eq!(text_of(&app, id).as_deref(), Some("A\n"));
        // Cmd+Z steps back through the session: before the cut, before the paste.
        frame(&mut app, &ctx, vec![key(Key::Z, cmd)], cmd);
        assert_eq!(text_of(&app, id).as_deref(), Some("A\nB"));
        frame(&mut app, &ctx, vec![key(Key::Z, cmd)], cmd);
        assert_eq!(text_of(&app, id).as_deref(), Some("Hello"));
        let shift_cmd = egui::Modifiers { shift: true, ..cmd };
        frame(&mut app, &ctx, vec![key(Key::Z, shift_cmd)], shift_cmd);
        assert_eq!(text_of(&app, id).as_deref(), Some("A\nB"));
        assert!(app.text_editing(), "undo inside a session keeps editing");
    }

    #[test]
    fn switching_tool_or_layer_commits_and_box_text_wraps() {
        let (mut app, ctx, id) = app();
        let steps = app.editor.history().len();
        app.begin_text_edit(&ctx, id, EditStart::End);
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Text("!".into())],
            egui::Modifiers::NONE,
        );
        app.tool = Tool::Move;
        frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        assert!(!app.text_editing(), "another tool commits");
        assert_eq!(app.editor.history().len(), steps + 1);

        // A paragraph box 60 px wide wraps "aaa bbb ccc" at 24 px.
        app.tool = Tool::Text;
        app.begin_new_text(&ctx, 10.0, 10.0, Some([60.0, 140.0]));
        let boxed = app.active.unwrap();
        frame(
            &mut app,
            &ctx,
            vec![egui::Event::Text("aaa bbb ccc".into())],
            egui::Modifiers::NONE,
        );
        let t = app
            .editor
            .doc()
            .layer(boxed)
            .unwrap()
            .text_layer()
            .unwrap()
            .clone();
        assert_eq!(t.box_size, Some([60.0, 140.0]));
        let lines = layout(&t).lines.len();
        assert!(lines >= 2, "{lines} lines");
        let b = app
            .editor
            .doc()
            .layer(boxed)
            .unwrap()
            .raster_store()
            .unwrap()
            .content_bounds()
            .unwrap();
        assert!(b.x >= 10 && b.right() <= 72, "glyphs stay in the box: {b:?}");
        // Selecting another layer commits too.
        app.set_active(Some(id));
        frame(&mut app, &ctx, Vec::new(), egui::Modifiers::NONE);
        assert!(!app.text_editing());
        assert_eq!(app.editor.history().last().copied(), Some("Add text"));
    }

    #[test]
    fn the_mouse_places_the_caret_and_selects_words_lines_and_ranges() {
        let (mut app, ctx, id) = app();
        let mut t = app.editor.doc().layer(id).unwrap().text_layer().unwrap().clone();
        t.text = "Hello world\nsecond line".into();
        app.run(&SetText {
            layer: id,
            text: t.clone(),
        });
        let steps = app.editor.history().len();
        let lay = layout(&t);
        let none = egui::Modifiers::NONE;
        let y0 = lay.lines[0].baseline - 5.0;
        let y1 = lay.lines[1].baseline - 5.0;
        let at = |i: usize, y: f32| (lay.caret(i).x + 1.0, y);
        let range = |app: &App| app.typer.session.as_ref().unwrap().buf.range();

        // A press on "w" starts editing there; quick repeats grow the
        // selection: the word, the line, everything.
        assert!(app.text_press(&ctx, at(6, y0), none, 10.0, 4.0));
        assert_eq!(range(&app), (6, 6));
        app.text_press(&ctx, at(6, y0), none, 10.2, 4.0);
        assert_eq!(range(&app), (6, 11), "double-click: the word");
        app.text_press(&ctx, at(6, y0), none, 10.4, 4.0);
        assert_eq!(range(&app), (0, 11), "triple-click: the line");
        app.text_press(&ctx, at(6, y0), none, 10.6, 4.0);
        assert_eq!(range(&app), (0, 23), "quadruple-click: everything");

        // Later: a press and a drag down to line 2 selects across lines.
        app.text_press(&ctx, at(6, y0), none, 20.0, 4.0);
        app.text_drag(at(18, y1));
        assert_eq!(range(&app), (6, 18));
        // Shift+press extends from the anchor.
        app.text_press(&ctx, at(2, y0), egui::Modifiers::SHIFT, 30.0, 4.0);
        let s = &app.typer.session.as_ref().unwrap().buf;
        assert_eq!((s.anchor, s.caret), (6, 2));
        assert_eq!(app.editor.history().len(), steps, "selecting edits nothing");

        // Cmd-drag moves the text by whole pixels.
        app.text_press(&ctx, at(6, y0), egui::Modifiers::COMMAND, 40.0, 4.0);
        app.text_drag((at(6, y0).0 + 30.4, y0 + 15.0));
        let moved = app.editor.doc().layer(id).unwrap().text_layer().unwrap().clone();
        assert_eq!((moved.x, moved.y), (50.0, 95.0));
        // A press well outside the text commits and starts nothing.
        assert!(!app.text_press(&ctx, (235.0, 10.0), none, 50.0, 4.0));
        assert!(!app.text_editing());
        assert_eq!(app.editor.history().len(), steps + 1, "the move is one step");
    }

    #[test]
    fn box_handles_resize_the_paragraph_while_editing() {
        let (mut app, ctx, _) = app();
        app.begin_new_text(&ctx, 10.0, 10.0, Some([100.0, 60.0]));
        let id = app.active.unwrap();
        app.text_change(1.0, true, |b| b.insert("one two three four"));
        // Grab the bottom-right handle and pull it out by (30, 20).
        assert!(app.text_press(&ctx, (110.0, 70.0), egui::Modifiers::NONE, 2.0, 4.0));
        app.text_drag((140.0, 90.0));
        let t = app.editor.doc().layer(id).unwrap().text_layer().unwrap().clone();
        assert_eq!((t.x, t.y, t.box_size), (10.0, 10.0, Some([130.0, 80.0])));
        // The top-left handle moves that corner; the far edges stay.
        app.text_press(&ctx, (10.0, 10.0), egui::Modifiers::NONE, 3.0, 4.0);
        app.text_drag((30.0, 0.0));
        let t = app.editor.doc().layer(id).unwrap().text_layer().unwrap().clone();
        assert_eq!((t.x, t.y, t.box_size), (30.0, 0.0, Some([110.0, 90.0])));
        // Still one step: typing and resizing in a single session.
        app.commit_text_edit();
        assert_eq!(app.editor.history().last().copied(), Some("Add text"));
        assert!(app.editor.undo().is_some());
        assert!(app.editor.doc().layer(id).is_none());
    }
}
