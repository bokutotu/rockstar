use super::{
    DiffSession, git,
    model::*,
    presentation::{HighlightedSource, PreparedDocument},
    tree::{DiffTree, TreeAction},
};
use crate::mode::{Mode, text_key};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement as _,
    IntoElement, Keystroke, MouseButton, ParentElement as _, Pixels, Render, Role, ScrollStrategy,
    StatefulInteractiveElement as _, Styled as _, StyledText, UniformListScrollHandle, Window,
    component::ActiveTheme as _, div, point, prelude::FluentBuilder as _, px, relative, rgb,
    uniform_list,
};
use std::{ops::Range, sync::Arc};

const LINE_HEIGHT: f32 = 20.;

pub(crate) struct DiffView {
    pub(crate) session: Entity<DiffSession>,
    focus: FocusHandle,
    tree: DiffTree,
    // The displayed file is independent of tree navigation/folder selection.
    selected_entry: Option<DiffEntry>,
    revision: u64,
    request: u64,
    loading: bool,
    error: Option<String>,
    document: Option<Arc<PreparedDocument>>,
    files_scroll: UniformListScrollHandle,
    body_scroll: UniformListScrollHandle,
    horizontal: Pixels,
}

impl DiffView {
    pub(crate) fn new(session: Entity<DiffSession>, cx: &mut Context<Self>) -> Self {
        cx.observe(&session, |this, _, cx| {
            if this.revision != this.session.read(cx).revision {
                this.sync(cx);
            }
            cx.notify();
        })
        .detach();
        let id = cx.entity_id();
        cx.on_release(move |this, cx| {
            this.session
                .update(cx, |session, cx| session.set_visible(id, false, cx));
        })
        .detach();
        let state = session.read(cx);
        let revision = state.revision;
        let tree = DiffTree::new(&state.snapshot.entries);
        let selected_entry = tree.selected_entry().cloned();
        let mut this = Self {
            session,
            focus: cx.focus_handle(),
            tree,
            selected_entry,
            revision,
            request: 0,
            loading: false,
            error: None,
            document: None,
            files_scroll: UniformListScrollHandle::new(),
            body_scroll: UniformListScrollHandle::new(),
            horizontal: px(0.),
        };
        this.load_selected(true, cx);
        this
    }

    pub(crate) fn focus(&self, window: &mut Window, cx: &mut App) {
        if !cx.global::<Mode>().is_overlay() {
            if *cx.global::<Mode>() != Mode::Normal {
                cx.set_global(Mode::Normal);
            }
            self.focus.focus(window, cx);
        }
    }

    pub(crate) fn set_visible(&self, visible: bool, cx: &mut Context<Self>) {
        let id = cx.entity_id();
        self.session
            .update(cx, |session, cx| session.set_visible(id, visible, cx));
    }

    fn sync(&mut self, cx: &mut Context<Self>) {
        let state = self.session.read(cx);
        self.revision = state.revision;
        let entries = &state.snapshot.entries;
        self.tree.refresh(entries);
        let retained = self
            .selected_entry
            .as_ref()
            .and_then(|selected| entries.iter().find(|entry| entry.same_file(selected)))
            .cloned();
        let reset_scroll = retained.is_none();
        self.selected_entry = retained.or_else(|| {
            self.tree
                .selected_entry()
                .or_else(|| self.tree.first_entry())
                .cloned()
        });
        self.scroll_to_selection();
        self.load_selected(reset_scroll, cx);
    }

    fn load_selected(&mut self, reset_scroll: bool, cx: &mut Context<Self>) {
        self.request += 1;
        self.error = None;
        let snapshot = self.session.read(cx).snapshot.clone();
        if reset_scroll {
            self.document = None;
            self.horizontal = px(0.);
            self.body_scroll
                .0
                .borrow()
                .base_handle
                .set_offset(point(px(0.), px(0.)));
        }
        let Some(entry) = self.selected_entry.clone() else {
            self.document = None;
            self.loading = false;
            cx.notify();
            return;
        };
        self.loading = true;
        let request = self.request;
        let revision = self.revision;
        let loading = cx.background_spawn(async move {
            git::load_document(&snapshot, &entry)
                .map(|document| PreparedDocument::new(&entry, document))
                .map(Arc::new)
        });
        cx.spawn(async move |this, cx| {
            let result = loading.await;
            let _ = this.update(cx, |this, cx| {
                if this.request != request || this.session.read(cx).revision != revision {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(document) => this.document = Some(document),
                    Err(error) => {
                        this.document = None;
                        this.error = Some(format!("{error:#}"));
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn scroll_to_selection(&self) {
        if self.tree.len() > 0 {
            self.files_scroll
                .scroll_to_item(self.tree.selected, ScrollStrategy::Nearest);
        }
    }

    fn tree_action(&mut self, action: TreeAction, cx: &mut Context<Self>) {
        if let Some(entry) = self.tree.apply(action)
            && self.selected_entry.as_ref() != Some(&entry)
        {
            self.selected_entry = Some(entry);
            self.load_selected(true, cx);
        }
        self.scroll_to_selection();
        cx.notify();
    }

    pub(crate) fn update_index(
        &mut self,
        action: IndexAction,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let entry = self
            .selected_entry
            .clone()
            .ok_or_else(|| anyhow::anyhow!("Select a changed file first"))?;
        self.session
            .update(cx, |session, cx| session.update_index(entry, action, cx))
    }

    fn scroll_horizontal(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let maximum = self
            .document
            .as_ref()
            .map_or(0., |doc| doc.max_columns as f32 * 16.);
        self.horizontal = (self.horizontal + delta).clamp(px(0.), px(maximum));
        cx.notify();
    }

    pub(crate) fn key(&mut self, key: &Keystroke, cx: &mut Context<Self>) -> bool {
        if *cx.global::<Mode>() != Mode::Normal
            || key.modifiers.control
            || key.modifiers.alt
            || key.modifiers.platform
        {
            return false;
        }
        match text_key(key) {
            "j" | "down" => self.tree_action(TreeAction::Down, cx),
            "k" | "up" => self.tree_action(TreeAction::Up, cx),
            "enter" => self.tree_action(TreeAction::Open, cx),
            "r" => self.session.update(cx, |session, cx| session.refresh(cx)),
            "h" | "left" => self.tree_action(TreeAction::Collapse, cx),
            "l" | "right" => self.tree_action(TreeAction::Expand, cx),
            "pageup" | "pagedown" => {
                let handle = &self.body_scroll.0.borrow().base_handle;
                let mut offset = handle.offset();
                let page = handle.bounds().size.height.max(px(LINE_HEIGHT)) * 0.9;
                offset.y += if key.key == "pageup" { page } else { -page };
                offset.y = offset.y.clamp(-handle.max_offset().y, px(0.));
                handle.set_offset(offset);
                cx.notify();
            }
            _ => return false,
        }
        true
    }

    fn render_files(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("diff-file-tree")
            .role(Role::Tree)
            .aria_label("Changed files")
            .w(px(260.))
            .max_w(relative(0.35))
            .h_full()
            .flex_shrink_0()
            .flex()
            .flex_col()
            .border_r_1()
            .border_color(cx.theme().border)
            .child(div().px_2().py_2().child("Changes"))
            .child(
                uniform_list(
                    "diff-files",
                    self.tree.len(),
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        range
                            .map(|index| {
                                let item = this.tree.item(index);
                                let expanded = this.tree.is_expanded(item);
                                let icon = if item.is_folder() {
                                    if expanded { "▾ " } else { "▸ " }
                                } else {
                                    "  "
                                };
                                div()
                                    .id(("diff-file", index))
                                    .role(Role::TreeItem)
                                    .aria_label(item.description())
                                    .aria_selected(index == this.tree.selected)
                                    .when(item.is_folder(), |row| row.aria_expanded(expanded))
                                    .h(px(24.))
                                    .w_full()
                                    .pl(px(8. + item.depth() as f32 * 14.))
                                    .pr_2()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .overflow_hidden()
                                    .cursor_pointer()
                                    .when(index == this.tree.selected, |row| {
                                        row.bg(cx.theme().accent)
                                            .text_color(cx.theme().accent_foreground)
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .truncate()
                                            .child(format!("{icon}{}", item.label())),
                                    )
                                    .when_some(item.entry(), |row, entry| {
                                        row.child(
                                            div()
                                                .flex_shrink_0()
                                                .text_xs()
                                                .child(entry.kind.label()),
                                        )
                                    })
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        if cx.global::<Mode>().is_overlay() {
                                            return;
                                        }
                                        this.focus(window, cx);
                                        this.tree.select(index);
                                        this.tree_action(TreeAction::Open, cx);
                                    }))
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.files_scroll)
                .flex_1()
                .min_h_0(),
            )
    }

    fn render_document(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        if let Some(error) = &self.error {
            return div().p_4().child(error.clone()).into_any_element();
        }
        let Some(prepared) = &self.document else {
            let message = if self.loading {
                "Loading file…"
            } else if self.selected_entry.is_none() {
                "No changes in this comparison"
            } else {
                "Select a file"
            };
            return div()
                .p_4()
                .text_color(cx.theme().muted_foreground)
                .child(message)
                .into_any_element();
        };
        let doc = &prepared.document;
        let section = self.selected_entry.as_ref().map(|e| e.section);
        let state = self.session.read(cx);
        let (before_label, after_label) = match &state.snapshot.source {
            DiffSource::PullRequest {
                head_oid,
                merge_base_oid,
                ..
            } => (
                format!("Merge base {}", &merge_base_oid[..8]),
                format!("PR head {}", &head_oid[..8]),
            ),
            _ if section == Some(DiffSection::Staged) => ("HEAD".into(), "Index".into()),
            _ => ("Index".into(), "Working tree (saved)".into()),
        };
        let header = |label: String, version: &FileVersion| {
            div()
                .w_1_2()
                .min_w_0()
                .px_2()
                .py_1()
                .child(div().text_sm().child(label))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(version.description()),
                )
        };
        let mut content = div().size_full().flex().flex_col().child(
            div()
                .flex()
                .flex_shrink_0()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(header(before_label, &doc.before))
                .child(header(after_label, &doc.after)),
        );
        if prepared.rows.is_empty() {
            let message = if doc.before.text().is_none() || doc.after.text().is_none() {
                "Non-text content — metadata shown above"
            } else {
                "No text lines — metadata shown above"
            };
            content = content.child(
                div()
                    .p_4()
                    .text_color(cx.theme().muted_foreground)
                    .child(message),
            );
        } else {
            content = content.child(
                uniform_list(
                    "diff-lines",
                    prepared.rows.len(),
                    cx.processor(|this, range: Range<usize>, _, cx| {
                        let Some(prepared) = this.document.as_ref() else {
                            return Vec::new();
                        };
                        range
                            .filter_map(|index| {
                                let row = prepared.rows.get(index)?;
                                let side =
                                    |line: Option<usize>,
                                     source: &HighlightedSource,
                                     added: bool| {
                                        let background = if !row.changed {
                                            cx.theme().background
                                        } else if line.is_none() {
                                            cx.theme().muted
                                        } else if added {
                                            rgb(0x238636).opacity(0.14).into()
                                        } else {
                                            rgb(0xcf3636).opacity(0.14).into()
                                        };
                                        div()
                                            .w_1_2()
                                            .h_full()
                                            .min_w_0()
                                            .flex()
                                            .overflow_hidden()
                                            .bg(background)
                                            .border_r_1()
                                            .border_color(cx.theme().border)
                                            .child(
                                                div()
                                                    .w(px(52.))
                                                    .h_full()
                                                    .flex_shrink_0()
                                                    .pr_2()
                                                    .text_right()
                                                    .text_color(cx.theme().muted_foreground)
                                                    .child(line.map_or_else(String::new, |n| {
                                                        format!(
                                                            "{}{}",
                                                            if row.changed {
                                                                if added { "+" } else { "−" }
                                                            } else {
                                                                " "
                                                            },
                                                            n + 1
                                                        )
                                                    })),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .h_full()
                                                    .overflow_hidden()
                                                    .child(
                                                        div()
                                                            .relative()
                                                            .left(-this.horizontal)
                                                            .whitespace_nowrap()
                                                            .child(line.map_or_else(
                                                                || StyledText::new(""),
                                                                |i| {
                                                                    source.styled_line(
                                                                        i,
                                                                        &*cx.theme()
                                                                            .highlight_theme,
                                                                    )
                                                                },
                                                            )),
                                                    ),
                                            )
                                    };
                                Some(
                                    div()
                                        .h(px(LINE_HEIGHT))
                                        .w_full()
                                        .flex()
                                        .line_height(px(LINE_HEIGHT))
                                        .child(side(row.before, &prepared.before, false))
                                        .child(side(row.after, &prepared.after, true)),
                                )
                            })
                            .collect::<Vec<_>>()
                    }),
                )
                .track_scroll(&self.body_scroll)
                .flex_1()
                .min_h_0()
                .w_full()
                .on_scroll(cx.listener(
                    |this, event: &gpui_kit::ScrollEvent, _, cx| {
                        let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
                        if event.modifiers.shift || delta.x.abs() > delta.y.abs() {
                            this.scroll_horizontal(
                                if event.modifiers.shift && delta.x == px(0.) {
                                    -delta.y
                                } else {
                                    -delta.x
                                },
                                cx,
                            );
                            cx.stop_propagation();
                        }
                    },
                )),
            );
        }
        content.into_any_element()
    }
}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.session.read(cx);
        let label = state.label();
        let pending = state.pending.or(self.loading.then_some("Loading file…"));
        let error = state.error.clone();
        let source = match &state.snapshot.source {
            DiffSource::Local(_) => format!("{} · saved files only", state.snapshot.root.display()),
            DiffSource::PullRequest {
                base_oid, head_oid, ..
            } => format!(
                "{} · base {} · head {}",
                state.snapshot.root.display(),
                &base_oid[..8],
                &head_oid[..8]
            ),
        };
        let path = self
            .selected_entry
            .as_ref()
            .map(DiffEntry::label)
            .unwrap_or_default();
        div().id("diff-view").role(Role::Group).aria_label(label.clone()).track_focus(&self.focus)
            .size_full().min_w_0().min_h_0().flex().flex_col().overflow_hidden()
            .bg(cx.theme().background).text_color(cx.theme().foreground)
            .font_family(cx.theme().mono_font_family.clone()).text_size(px(12.))
            .on_mouse_down(MouseButton::Left, cx.listener(|this, _, window, cx| this.focus(window, cx)))
            .child(div().px_2().py_1().flex_shrink_0().border_b_1().border_color(cx.theme().border)
                .child(format!("{label}{}", pending.map_or(String::new(), |s| format!(" · {s}"))))
                .child(div().text_xs().text_color(cx.theme().muted_foreground).truncate().child(source)))
            .when_some(error, |view, error| view.child(div().p_2().text_color(rgb(0xcf3636)).child(error)))
            .child(div().flex_1().min_h_0().w_full().flex()
                .child(self.render_files(cx))
                .child(div().flex_1().min_w_0().h_full().flex().flex_col()
                    .child(div().px_2().py_1().flex_shrink_0().truncate().child(path))
                    .child(div().flex_1().min_h_0().child(self.render_document(cx)))))
            .child(div().px_2().py_1().flex_shrink_0().border_t_1().border_color(cx.theme().border)
                .text_xs().text_color(cx.theme().muted_foreground).truncate()
                .child("j/k select · Enter open · h/l tree · PgUp/PgDn scroll · Shift+wheel horizontal · r refresh · :stage / :unstage"))
    }
}
