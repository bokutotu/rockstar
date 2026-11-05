use std::{path::PathBuf, sync::Arc};

use async_channel::Sender;
use gpui_kit::{
    App, AppContext as _, Context, Entity, EntityId, Focusable as _, InteractiveElement as _,
    IntoElement, MouseButton, ParentElement as _, Role, StatefulInteractiveElement as _,
    Styled as _, Task, View, Window,
    base::TestSupportExt as _,
    component::{
        ActiveTheme as _, WindowExt as _,
        input::{Enter, IndentInline, InputEvent, MoveDown, MoveUp, Textarea, TextareaState},
        notification::Notification,
    },
    div,
    prelude::FluentBuilder as _,
};

use crate::{
    agent::{DISCONNECTED_MESSAGE, PromptRequest},
    file_search,
    mode::Mode,
};

/// Candidates kept for an `@` mention; the popup scrolls through them.
const MAX_MATCHES: usize = 50;
/// Rows shown at once in the mention popup.
const VISIBLE_MATCHES: usize = 10;

fn input_hint(mode: Mode) -> &'static str {
    if mode == Mode::Insert {
        "Ask pi…"
    } else {
        "NORMAL — click to type · : commands"
    }
}

/// The `@` mention being typed and the files that match it.
struct FileMention {
    /// Byte offset of the `@` in the draft.
    start: usize,
    query: String,
    matches: Vec<String>,
    selected: usize,
}

/// The workspace files offered for `@` mentions.
struct FileIndex {
    root: PathBuf,
    paths: Arc<Vec<String>>,
    _scan: Option<Task<()>>,
}

pub(super) struct ComposerView {
    input: Entity<TextareaState>,
    prompts: Sender<PromptRequest>,
    files: FileIndex,
    mention: Option<FileMention>,
}

impl ComposerView {
    pub(super) fn new(
        prompts: Sender<PromptRequest>,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let hint = input_hint(*cx.global::<Mode>());
        let input = cx.new(|cx| TextareaState::new(window, cx).rows(3).placeholder(hint));
        if *cx.global::<Mode>() == Mode::Insert {
            input.focus_handle(cx).focus(window, cx);
        }
        cx.observe_global_in::<Mode>(window, |this, window, cx| {
            let mode = *cx.global::<Mode>();
            let hint = input_hint(mode);
            this.input
                .update(cx, |input, cx| input.set_placeholder(hint, window, cx));
            // Insert mode always types into the message input.
            if mode == Mode::Insert {
                this.input.focus_handle(cx).focus(window, cx);
            } else {
                // Escape leaves Insert mode and dismisses the mention popup.
                this.mention = None;
            }
            cx.notify();
        })
        .detach();
        cx.on_focus(&input.focus_handle(cx), window, |_, _, cx| {
            if *cx.global::<Mode>() == Mode::Normal {
                cx.set_global(Mode::Insert);
            }
        })
        .detach();
        cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
            InputEvent::Change => this.update_mention(cx),
            InputEvent::Blur => {
                this.mention = None;
                cx.notify();
            }
            _ => {}
        })
        .detach();
        Self {
            input,
            prompts,
            files: FileIndex {
                root,
                paths: Arc::default(),
                _scan: None,
            },
            mention: None,
        }
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.mention = None;
        cx.notify();
    }

    /// Opens, refines, or closes the mention popup for the text at the cursor.
    fn update_mention(&mut self, cx: &mut Context<Self>) {
        let input = self.input.read(cx);
        let text = input.value();
        let found = file_search::mention_at(&text, input.cursor())
            .map(|(start, query)| (start, query.to_string()));
        let Some((start, query)) = found else {
            self.mention = None;
            cx.notify();
            return;
        };
        if self.mention.is_none() {
            // Rescan on every new mention so the list follows the workspace.
            self.rescan(cx);
        }
        let matches = file_search::fuzzy_match(&self.files.paths, &query, MAX_MATCHES);
        self.mention = Some(FileMention {
            start,
            query,
            matches,
            selected: 0,
        });
        cx.notify();
    }

    fn rescan(&mut self, cx: &mut Context<Self>) {
        let root = self.files.root.clone();
        let scan = cx.background_spawn(async move { file_search::scan(&root) });
        self.files._scan = Some(cx.spawn(async move |this, cx| {
            let paths = Arc::new(scan.await);
            this.update(cx, |this, cx| {
                this.files.paths = paths;
                if let Some(mention) = &mut this.mention {
                    mention.matches =
                        file_search::fuzzy_match(&this.files.paths, &mention.query, MAX_MATCHES);
                    mention.selected = 0;
                    cx.notify();
                }
            })
            .ok();
        }));
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) -> bool {
        let Some(mention) = &mut self.mention else {
            return false;
        };
        let count = mention.matches.len();
        if count == 0 {
            return false;
        }
        mention.selected = if down {
            (mention.selected + 1) % count
        } else {
            (mention.selected + count - 1) % count
        };
        cx.notify();
        true
    }

    /// Replaces the `@query` before the cursor with the chosen path.
    fn accept_mention(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(mention) = self.mention.take() else {
            return false;
        };
        let Some(path) = mention.matches.get(index) else {
            self.mention = Some(mention);
            return false;
        };
        let replacement = file_search::mention_text(path);
        self.input.update(cx, |input, cx| {
            let cursor = input.cursor();
            input.set_selected_range(mention.start..cursor, cx);
            input.replace(replacement, window, cx);
        });
        // A directory keeps the popup open to complete inside it.
        self.update_mention(cx);
        true
    }

    fn handle_enter(&mut self, action: &Enter, window: &mut Window, cx: &mut Context<Self>) {
        if !action.secondary {
            let selected = self.mention.as_ref().map(|mention| mention.selected);
            if let Some(selected) = selected
                && self.accept_mention(selected, window, cx)
            {
                cx.stop_propagation();
            }
            return;
        }
        if *cx.global::<Mode>() == Mode::Insert {
            self.submit(window, cx);
        }
        cx.stop_propagation();
    }

    fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        match self.prompts.try_send(PromptRequest { text }) {
            Ok(()) => self.clear(window, cx),
            Err(_) => window.push_notification(
                Notification::error(DISCONNECTED_MESSAGE).autohide(false),
                cx,
            ),
        }
    }

    fn render_mention(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let mention = self.mention.as_ref()?;
        let theme = cx.theme();
        let first = mention
            .selected
            .saturating_sub(VISIBLE_MATCHES - 1)
            .min(mention.matches.len().saturating_sub(VISIBLE_MATCHES));
        let rows = mention
            .matches
            .iter()
            .enumerate()
            .skip(first)
            .take(VISIBLE_MATCHES)
            .map(|(index, path)| {
                let selected = index == mention.selected;
                div()
                    .id(index)
                    .test_support()
                    .role(Role::ListBoxOption)
                    .aria_label(path.clone())
                    .aria_selected(selected)
                    .px_2()
                    .py_0p5()
                    .rounded_sm()
                    .cursor_pointer()
                    .when(selected, |row| {
                        row.bg(theme.accent).text_color(theme.accent_foreground)
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, window, cx| {
                            this.accept_mention(index, window, cx);
                            cx.stop_propagation();
                        }),
                    )
                    .child(path.clone())
            })
            .collect::<Vec<_>>();
        let empty = rows.is_empty();
        Some(
            div()
                .id("file-mention-popup")
                .test_support()
                .role(Role::ListBox)
                .aria_label("Files")
                .occlude()
                .absolute()
                .left_4()
                .right_4()
                .bottom_full()
                .mb_1()
                .p_1()
                .flex()
                .flex_col()
                .rounded_md()
                .border_1()
                .border_color(theme.border)
                .bg(theme.popover)
                .text_color(theme.popover_foreground)
                .shadow_md()
                .children(rows)
                .when(empty, |popup| {
                    popup.child(
                        div()
                            .px_2()
                            .py_0p5()
                            .text_color(theme.muted_foreground)
                            .child("No matching files"),
                    )
                }),
        )
    }
}

#[derive(IntoElement)]
pub(super) struct Composer {
    state: Entity<ComposerView>,
}

impl Composer {
    pub(super) fn new(state: &Entity<ComposerView>) -> Self {
        Self {
            state: state.clone(),
        }
    }
}

impl View for Composer {
    fn entity_id(&self) -> Option<EntityId> {
        Some(self.state.entity_id())
    }

    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        self.state.update(cx, |state, cx| {
            let mode = *cx.global::<Mode>();
            div()
                // Capture Ctrl+Enter before the textarea inserts a newline.
                .capture_action(cx.listener(|this, action: &Enter, window, cx| {
                    this.handle_enter(action, window, cx);
                }))
                // While the mention popup is open, the arrows and Tab drive it.
                .capture_action(cx.listener(|this, _: &MoveUp, _, cx| {
                    if this.move_selection(false, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &MoveDown, _, cx| {
                    if this.move_selection(true, cx) {
                        cx.stop_propagation();
                    }
                }))
                .capture_action(cx.listener(|this, _: &IndentInline, window, cx| {
                    let selected = this.mention.as_ref().map(|mention| mention.selected);
                    if let Some(selected) = selected
                        && this.accept_mention(selected, window, cx)
                    {
                        cx.stop_propagation();
                    }
                }))
                .relative()
                .flex()
                .p_4()
                .border_t_1()
                .border_color(cx.theme().border)
                .child(
                    Textarea::new(&state.input)
                        .flex_1()
                        .readonly(mode != Mode::Insert)
                        .aria_label("Message to pi"),
                )
                .children(state.render_mention(cx))
        })
    }
}
