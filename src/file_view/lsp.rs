use super::documentation::{DocumentationPanel, hover_markdown};
use super::*;
use crate::lsp::{
    DocumentSnapshot, ServerState,
    language::{self, Encoding},
    providers,
};
use gpui_kit::{
    EventEmitter, HighlightStyle, MouseButton,
    base::input::{
        AnnotationPlacement, EditorAnnotation, TextDecoration, TextDecorationCollection,
    },
    component::button::Button,
};
use lsp_types::{
    CallHierarchyIncomingCall, CallHierarchyItem, CallHierarchyOutgoingCall, CodeLens,
    DocumentHighlight, Hover, InlayHintLabel, Location, Position, SemanticToken, SemanticTokens,
    SemanticTokensLegend, Uri,
};
use serde_json::json;
use std::rc::Rc;

#[derive(Clone)]
pub(crate) struct Navigate {
    pub uri: Uri,
    pub position: Position,
    pub encoding: Encoding,
}
impl EventEmitter<Navigate> for FileView {}

pub(super) struct LspViewState {
    pub(super) focus: FocusHandle,
    pub(super) documentation: Option<DocumentationPanel>,
    highlights: TextDecorationCollection,
    cursor_stamp: Option<(i32, usize)>,
    composing: bool,
    cursor_task: Option<Task<()>>,
    task: Option<Task<()>>,
    call_tasks: Vec<Task<()>>,
    popup: Option<Popup>,
    generation: u64,
    lens_snapshot: Option<DocumentSnapshot>,
    lenses: Vec<CodeLens>,
}
impl LspViewState {
    pub(super) fn new(input: &Entity<EditorState>, cx: &mut Context<FileView>) -> Self {
        let highlights = input.update(cx, |input, cx| {
            input.create_decorations_collection(Vec::new(), cx)
        });
        Self {
            focus: cx.focus_handle(),
            documentation: None,
            highlights,
            cursor_stamp: None,
            composing: false,
            cursor_task: None,
            task: None,
            call_tasks: Vec::new(),
            popup: None,
            generation: 0,
            lens_snapshot: None,
            lenses: Vec::new(),
        }
    }
}
#[derive(Clone, Copy, PartialEq)]
enum Direction {
    Incoming,
    Outgoing,
}
impl Direction {
    fn method(self) -> &'static str {
        match self {
            Self::Incoming => "callHierarchy/incomingCalls",
            Self::Outgoing => "callHierarchy/outgoingCalls",
        }
    }
}
struct CallNode {
    item: CallHierarchyItem,
    sites: Vec<Location>,
    children: Option<Vec<CallNode>>,
    expanded: bool,
    loading: bool,
}
impl CallNode {
    fn new(item: CallHierarchyItem, sites: Vec<Location>) -> Self {
        Self {
            item,
            sites,
            children: None,
            expanded: false,
            loading: false,
        }
    }
}
struct Popup {
    snap: DocumentSnapshot,
    encoding: Encoding,
    title: String,
    message: String,
    selected: usize,
    content: Content,
}
enum Content {
    Locations(Vec<Location>),
    Calls {
        direction: Direction,
        roots: Vec<CallNode>,
    },
}
#[derive(Clone)]
struct Row {
    label: String,
    location: Location,
    path: Vec<usize>,
    depth: usize,
    expanded: bool,
    loading: bool,
}
fn rows(content: &Content) -> Vec<Row> {
    fn visit(nodes: &[CallNode], prefix: &[usize], out: &mut Vec<Row>) {
        for (i, node) in nodes.iter().enumerate() {
            let mut path = prefix.to_vec();
            path.push(i);
            let label = format!(
                "{}{}",
                node.item.name,
                if node.sites.is_empty() {
                    String::new()
                } else {
                    format!(" · {} call site(s)", node.sites.len())
                }
            );
            out.push(Row {
                label,
                location: Location::new(node.item.uri.clone(), node.item.selection_range),
                path: path.clone(),
                depth: prefix.len(),
                expanded: node.expanded,
                loading: node.loading,
            });
            if node.expanded
                && let Some(children) = &node.children
            {
                visit(children, &path, out);
            }
        }
    }
    match content {
        Content::Locations(locations) => locations
            .iter()
            .map(|location| Row {
                label: format!(
                    "{}:{}:{}",
                    language::path(&location.uri)
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|_| location.uri.to_string()),
                    location.range.start.line + 1,
                    location.range.start.character + 1
                ),
                location: location.clone(),
                path: Vec::new(),
                depth: 0,
                expanded: false,
                loading: false,
            })
            .collect(),
        Content::Calls { roots, .. } => {
            let mut out = Vec::new();
            visit(roots, &[], &mut out);
            out
        }
    }
}
fn node_mut<'a>(nodes: &'a mut [CallNode], path: &[usize]) -> Option<&'a mut CallNode> {
    let (&index, rest) = path.split_first()?;
    let node = nodes.get_mut(index)?;
    if rest.is_empty() {
        Some(node)
    } else {
        node_mut(node.children.as_mut()?, rest)
    }
}

impl FileView {
    pub(super) fn focus_lsp_popup(&self, window: &mut Window, cx: &mut App) -> bool {
        if let Some(panel) = &self.lsp_ui.documentation {
            cx.set_global(Mode::LspPopup);
            panel.content.focus_handle(cx).focus(window, cx);
            return true;
        }
        if self.lsp_ui.popup.is_none() {
            return false;
        }
        cx.set_global(Mode::LspPopup);
        self.lsp_ui.focus.focus(window, cx);
        true
    }
    fn cancel_lsp_request(&mut self) {
        self.lsp_ui.generation += 1;
        self.lsp_ui.task = None;
    }
    pub(super) fn init_lsp(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return;
        };
        // Losing the source focus cancels pending requests even if the user
        // returns to the same position before a delayed reply arrives.
        cx.on_blur(&self.input.focus_handle(cx), window, |this, _, _| {
            if this.lsp_ui.popup.is_none() && this.lsp_ui.documentation.is_none() {
                this.cancel_lsp_request();
            }
        })
        .detach();
        cx.on_focus_out(&self.lsp_ui.focus, window, |this, _, window, cx| {
            if this.lsp_ui.documentation.is_some() {
                this.close_documentation(window, cx);
            }
        })
        .detach();
        let provider = providers::Provider::new(&doc, &self.input);
        let view = cx.entity().downgrade();
        let connection = doc.read(cx).session.clone();
        self.input.update(cx, |input, _| {
            input.lsp_mut().completion_provider = Some(provider.clone());
            input.lsp_mut().hover_provider = Some(provider.clone());
            input.lsp_mut().definition_provider = Some(provider);
            input.lsp_mut().show_document = Some(Rc::new(move |params, _, cx| {
                let Some(selection) = params.selection else {
                    return false;
                };
                let _ = view.update(cx, |_, cx| {
                    cx.emit(Navigate {
                        uri: params.uri.clone(),
                        position: selection.start,
                        encoding: connection.encoding(),
                    })
                });
                true
            }));
        });
        cx.observe_in(&doc, window, |this, _, window, cx| {
            this.sync_lsp(cx);
            this.schedule_lsp_cursor(window, cx);
        })
        .detach();
        self.sync_lsp(cx);
    }
    pub(super) fn lsp_status(&self, cx: &App) -> String {
        let Some(doc) = &self.buffer.read(cx).lsp else {
            return String::new();
        };
        let doc = doc.read(cx);
        let label = match doc.session.state() {
            ServerState::Starting => "starting",
            ServerState::Ready { .. } => "ready",
            ServerState::Failed(_) => "error",
            ServerState::Stopped => "stopped",
        };
        format!(" · {}: {label}", doc.session.kind.command().0)
    }
    pub(super) fn sync_lsp(&mut self, cx: &mut Context<Self>) {
        // Server positions refer to committed text, not the temporary IME preedit.
        if self.input.read(cx).is_composing() {
            return;
        }
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return;
        };
        let doc = doc.read(cx);
        let Ok(snap) = doc.snapshot(cx) else {
            return;
        };
        let encoding = doc.session.encoding();
        if self.lsp_ui.cursor_task.is_none() && doc.session.supports("documentHighlightProvider") {
            self.lsp_ui.cursor_stamp = None;
        }
        let analysis = &doc.analysis;
        let mut diagnostics = Vec::new();
        if analysis.diagnostics.version == snap.version {
            for diagnostic in &analysis.diagnostics.value {
                let mut diagnostic = diagnostic.clone();
                if let Ok(range) = encoding.editor_range(&snap.text, diagnostic.range) {
                    diagnostic.range = range;
                    diagnostics.push(diagnostic);
                }
            }
        }
        let mut annotations = Vec::new();
        if analysis.inlay_hints.version == snap.version {
            for hint in &analysis.inlay_hints.value {
                let Ok(offset) = encoding.offset(&snap.text, hint.position) else {
                    continue;
                };
                let label = match &hint.label {
                    InlayHintLabel::String(label) => label.clone(),
                    InlayHintLabel::LabelParts(parts) => {
                        parts.iter().map(|p| p.value.as_str()).collect()
                    }
                };
                let label = format!(
                    "{}{}{}",
                    if hint.padding_left == Some(true) {
                        " "
                    } else {
                        ""
                    },
                    label,
                    if hint.padding_right == Some(true) {
                        " "
                    } else {
                        ""
                    }
                )
                .replace(['\n', '\r'], " ");
                annotations.push(EditorAnnotation {
                    offset,
                    label: label.into(),
                    placement: AnnotationPlacement::Inline,
                    action: None,
                });
            }
        }
        self.lsp_ui.lenses.clear();
        if analysis.code_lenses.version == snap.version {
            for lens in &analysis.code_lenses.value {
                let Some(command) = &lens.command else {
                    continue;
                };
                let Ok(offset) = encoding.offset(&snap.text, lens.range.start) else {
                    continue;
                };
                let id = self.lsp_ui.lenses.len();
                self.lsp_ui.lenses.push(lens.clone());
                annotations.push(EditorAnnotation {
                    offset,
                    label: command.title.replace(['\n', '\r'], " ").into(),
                    placement: AnnotationPlacement::AboveLine,
                    action: Some(id),
                });
            }
        }
        self.lsp_ui.lens_snapshot = Some(snap.clone());
        let tokens = if analysis.semantic_tokens.version == snap.version {
            analysis
                .semantic_tokens
                .value
                .as_ref()
                .and_then(|t| editor_tokens(t, &snap.text, encoding).ok())
                .unwrap_or_default()
        } else {
            SemanticTokens::default()
        };
        let mut legend: SemanticTokensLegend = serde_json::from_value(
            doc.session.capabilities()["semanticTokensProvider"]["legend"].clone(),
        )
        .unwrap_or_default();
        legend.token_types = legend
            .token_types
            .into_iter()
            .map(|name| match name.as_str() {
                "parameter" => "variable.parameter".into(),
                "typeParameter" => "type".into(),
                "enumMember" => "constant".into(),
                "method" => "function.method".into(),
                "decorator" => "attribute".into(),
                _ => name,
            })
            .collect();
        self.input.update(cx, |input, cx| {
            if let Some(set) = input.diagnostics_mut() {
                set.clear();
                set.extend(diagnostics);
            }
            input.set_annotations(annotations, cx);
            input.set_semantic_tokens(&tokens, &legend, cx);
            cx.notify();
        });
        cx.notify();
    }
    pub(super) fn schedule_lsp_cursor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let composing = self.input.read(cx).is_composing();
        let was_composing = std::mem::replace(&mut self.lsp_ui.composing, composing);
        if composing {
            self.lsp_ui.cursor_task = None;
            return;
        }
        if was_composing {
            self.sync_lsp(cx);
            self.lsp_ui.cursor_stamp = None;
        }
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return;
        };
        let Ok(snap) = doc.read(cx).snapshot(cx) else {
            return;
        };
        let pos = self.vim.cursor;
        let stamp = (snap.version, pos);
        if self
            .lsp_ui
            .documentation
            .as_ref()
            .is_some_and(|panel| panel.version != snap.version || panel.position != pos)
        {
            self.close_documentation(window, cx);
        }
        if self
            .lsp_ui
            .cursor_stamp
            .is_some_and(|previous| previous != stamp)
            && self.lsp_ui.popup.is_none()
        {
            self.cancel_lsp_request();
        }
        if self.lsp_ui.cursor_stamp == Some(stamp) {
            return;
        }
        self.lsp_ui.cursor_stamp = Some(stamp);
        self.lsp_ui.cursor_task = None;
        self.lsp_ui.highlights.clear(cx);
        if self
            .lsp_ui
            .popup
            .as_ref()
            .is_some_and(|p| p.snap.version != snap.version)
        {
            self.close_lsp_popup(window, cx);
        }
        if !doc.read(cx).session.supports("documentHighlightProvider") {
            return;
        }
        self.lsp_ui.cursor_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let Ok(response) = doc.update(cx, |doc, cx| {
                doc.request_at("textDocument/documentHighlight", &snap, pos, cx)
            }) else {
                return;
            };
            let Ok(value) = response.recv().await else {
                return;
            };
            let Ok(highlights) = serde_json::from_value::<Option<Vec<DocumentHighlight>>>(value)
            else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                if this.lsp_ui.cursor_stamp != Some(stamp) || !doc.read(cx).is_current(&snap, cx) {
                    return;
                }
                let encoding = doc.read(cx).session.encoding();
                let highlights = highlights
                    .unwrap_or_default()
                    .iter()
                    .filter_map(|h| {
                        let range = encoding.range(&snap.text, h.range).ok()?;
                        Some(TextDecoration::new(
                            range,
                            HighlightStyle {
                                background_color: Some(cx.theme().accent.opacity(0.45)),
                                ..Default::default()
                            },
                        ))
                    })
                    .collect();
                this.lsp_ui.highlights.set(highlights, cx);
            });
        }));
    }
    pub(crate) fn jump_to(
        &mut self,
        position: Position,
        encoding: Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match encoding.offset(&self.buffer.read(cx).text, position) {
            Ok(offset) => {
                self.vim.cursor = offset;
                self.set_mode(Mode::Normal, cx);
                self.select(cx);
                self.focus(window, cx);
            }
            Err(error) => Self::lsp_error(error, window, cx),
        }
    }
    fn lsp_error(error: impl std::fmt::Display, window: &mut Window, cx: &mut App) {
        window.push_notification(Notification::error(format!("LSP: {error}")), cx);
    }
    fn start_lsp_request(
        &mut self,
        method: &'static str,
        capability: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            Self::lsp_error("No language server for this file", window, cx);
            return;
        };
        let session = doc.read(cx).session.clone();
        if !session.supports(capability) {
            Self::lsp_error(
                match session.state() {
                    ServerState::Failed(e) => e,
                    ServerState::Starting => "Language server is starting".into(),
                    _ => format!("{} does not support {method}", session.kind.command().0),
                },
                window,
                cx,
            );
            return;
        }
        let Ok(snap) = doc.read(cx).snapshot(cx) else {
            return;
        };
        let pos = self.vim.cursor;
        let response = doc.update(cx, |doc, cx| doc.request_at(method, &snap, pos, cx));
        let Ok(response) = response else {
            Self::lsp_error(response.err().unwrap(), window, cx);
            return;
        };
        self.lsp_ui.generation += 1;
        let generation = self.lsp_ui.generation;
        let encoding = session.encoding();
        if method == "textDocument/prepareCallHierarchy" {
            self.show_popup(
                Popup {
                    snap: snap.clone(),
                    encoding,
                    title: "Call hierarchy".into(),
                    message: "Loading…".into(),
                    selected: 0,
                    content: Content::Calls {
                        direction: Direction::Incoming,
                        roots: Vec::new(),
                    },
                },
                window,
                cx,
            );
        }
        self.lsp_ui.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = response.recv().await;
            let _ = this.update_in(cx, |this, window, cx| {
                if this.lsp_ui.generation != generation
                    || !doc.read(cx).is_current(&snap, cx)
                    || this.vim.cursor != pos
                    || (method == "textDocument/hover" && this.vim.mode != Mode::Normal)
                    || (!this.input.focus_handle(cx).is_focused(window)
                        && !this.lsp_ui.focus.is_focused(window))
                {
                    return;
                }
                let value = match result {
                    Ok(v) => v,
                    Err(e) => {
                        Self::lsp_error(e, window, cx);
                        if let Some(p) = &mut this.lsp_ui.popup {
                            p.message = "Request failed".into();
                        }
                        cx.notify();
                        return;
                    }
                };
                match method {
                    "textDocument/definition" => match providers::locations(value) {
                        Ok(locations) => this.show_locations(
                            locations
                                .into_iter()
                                .map(|l| Location::new(l.target_uri, l.target_selection_range))
                                .collect(),
                            snap,
                            encoding,
                            "Definitions",
                            window,
                            cx,
                        ),
                        Err(e) => Self::lsp_error(e, window, cx),
                    },
                    "textDocument/hover" => match serde_json::from_value::<Option<Hover>>(value) {
                        Ok(hover) => {
                            match hover.and_then(|hover| hover_markdown(&hover.contents)) {
                                Some(markdown) => {
                                    this.show_documentation(markdown, snap.version, pos, window, cx)
                                }
                                None => window.push_notification(
                                    Notification::info("No documentation at this position"),
                                    cx,
                                ),
                            }
                        }
                        Err(error) => Self::lsp_error(error, window, cx),
                    },
                    _ => match serde_json::from_value::<Option<Vec<CallHierarchyItem>>>(value) {
                        Ok(items) => {
                            if let Some(p) = &mut this.lsp_ui.popup {
                                let roots = items
                                    .unwrap_or_default()
                                    .into_iter()
                                    .map(|i| CallNode::new(i, Vec::new()))
                                    .collect::<Vec<_>>();
                                p.message = if roots.is_empty() {
                                    "No call hierarchy at this position".into()
                                } else {
                                    String::new()
                                };
                                p.content = Content::Calls {
                                    direction: Direction::Incoming,
                                    roots,
                                };
                            }
                            this.expand_call(window, cx);
                        }
                        Err(e) => Self::lsp_error(e, window, cx),
                    },
                }
                cx.notify();
            });
        }));
    }
    fn show_locations(
        &mut self,
        locations: Vec<Location>,
        snap: DocumentSnapshot,
        encoding: Encoding,
        title: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if locations.len() == 1 {
            let l = &locations[0];
            cx.emit(Navigate {
                uri: l.uri.clone(),
                position: l.range.start,
                encoding,
            });
        } else if locations.is_empty() {
            window.push_notification(Notification::info(format!("{title}: no results")), cx);
        } else {
            self.show_popup(
                Popup {
                    snap,
                    encoding,
                    title: title.into(),
                    message: String::new(),
                    selected: 0,
                    content: Content::Locations(locations),
                },
                window,
                cx,
            );
        }
    }
    fn show_popup(&mut self, popup: Popup, window: &mut Window, cx: &mut Context<Self>) {
        if self.lsp_ui.documentation.is_some() {
            self.close_documentation(window, cx);
        }
        self.lsp_ui.popup = Some(popup);
        cx.set_global(Mode::LspPopup);
        self.lsp_ui.focus.focus(window, cx);
        cx.notify();
    }
    pub(super) fn close_lsp_popup(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let documentation = self.lsp_ui.documentation.take();
        let focused = self.lsp_ui.focus.contains_focused(window, cx)
            || documentation
                .as_ref()
                .is_some_and(|panel| panel.content.focus_handle(cx).is_focused(window));
        self.lsp_ui.popup = None;
        self.cancel_lsp_request();
        self.lsp_ui.call_tasks.clear();
        if focused || (documentation.is_some() && *cx.global::<Mode>() == Mode::LspPopup) {
            cx.set_global(self.vim.mode);
        }
        // Focus may already belong to a different pane. Closing stale content
        // there must not steal it back.
        if focused {
            self.input.focus_handle(cx).focus(window, cx);
        }
        cx.notify();
    }
    fn switch_calls(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popup) = &mut self.lsp_ui.popup else {
            return;
        };
        let Content::Calls {
            direction: current,
            roots,
        } = &mut popup.content
        else {
            return;
        };
        if *current == direction {
            return;
        }
        *current = direction;
        popup.selected = 0;
        popup.message = String::new();
        for node in roots {
            node.children = None;
            node.expanded = false;
            node.loading = false;
        }
        self.lsp_ui.generation += 1;
        self.lsp_ui.call_tasks.clear();
        self.expand_call(window, cx);
        cx.notify();
    }
    fn expand_call(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(popup) = &mut self.lsp_ui.popup else {
            return;
        };
        let Some(row) = rows(&popup.content).get(popup.selected).cloned() else {
            return;
        };
        let Content::Calls { direction, roots } = &mut popup.content else {
            return;
        };
        let direction = *direction;
        let Some(node) = node_mut(roots, &row.path) else {
            return;
        };
        node.expanded = true;
        if node.children.is_some() || node.loading {
            cx.notify();
            return;
        }
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return;
        };
        if !doc.read(cx).is_current(&popup.snap, cx) {
            return;
        }
        node.loading = true;
        let item = node.item.clone();
        let snap = popup.snap.clone();
        let generation = self.lsp_ui.generation;
        let response = doc
            .read(cx)
            .session
            .request(direction.method(), json!({"item":item}));
        self.lsp_ui
            .call_tasks
            .push(cx.spawn_in(window, async move |this, cx| {
                let result = response.recv().await.and_then(|value| {
                    if direction == Direction::Incoming {
                        let calls: Option<Vec<CallHierarchyIncomingCall>> =
                            serde_json::from_value(value)?;
                        Ok(calls
                            .unwrap_or_default()
                            .into_iter()
                            .map(|c| {
                                let sites = c
                                    .from_ranges
                                    .into_iter()
                                    .map(|r| Location::new(c.from.uri.clone(), r))
                                    .collect();
                                CallNode::new(c.from, sites)
                            })
                            .collect())
                    } else {
                        let calls: Option<Vec<CallHierarchyOutgoingCall>> =
                            serde_json::from_value(value)?;
                        Ok(calls
                            .unwrap_or_default()
                            .into_iter()
                            .map(|c| {
                                let sites = c
                                    .from_ranges
                                    .into_iter()
                                    .map(|r| Location::new(item.uri.clone(), r))
                                    .collect();
                                CallNode::new(c.to, sites)
                            })
                            .collect())
                    }
                });
                let _ = this.update_in(cx, |this, window, cx| {
                    if this.lsp_ui.generation != generation || !doc.read(cx).is_current(&snap, cx) {
                        return;
                    }
                    let Some(popup) = &mut this.lsp_ui.popup else {
                        return;
                    };
                    let Content::Calls { roots, .. } = &mut popup.content else {
                        return;
                    };
                    let Some(node) = node_mut(roots, &row.path) else {
                        return;
                    };
                    node.loading = false;
                    match result {
                        Ok(children) => node.children = Some(children),
                        Err(e) => {
                            node.expanded = false;
                            Self::lsp_error(e, window, cx);
                        }
                    }
                    cx.notify();
                });
            }));
        cx.notify();
    }
    fn accept_lsp_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(p) = &self.lsp_ui.popup else {
            return;
        };
        let Some(row) = rows(&p.content).get(p.selected).cloned() else {
            return;
        };
        let encoding = p.encoding;
        self.close_lsp_popup(window, cx);
        cx.emit(Navigate {
            uri: row.location.uri,
            position: row.location.range.start,
            encoding,
        });
    }
    pub(super) fn lsp_key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.lsp_ui.popup.is_some() && self.lsp_ui.focus.is_focused(window) {
            match text_key(key) {
                "escape" | "f11" => self.close_lsp_popup(window, cx),
                "enter" => self.accept_lsp_row(window, cx),
                "tab" => {
                    let direction = match self.lsp_ui.popup.as_ref().map(|p| &p.content) {
                        Some(Content::Calls {
                            direction: Direction::Incoming,
                            ..
                        }) => Direction::Outgoing,
                        _ => Direction::Incoming,
                    };
                    self.switch_calls(direction, window, cx);
                }
                "down" | "j" | "n" => {
                    let p = self.lsp_ui.popup.as_mut().unwrap();
                    let len = rows(&p.content).len();
                    if len > 0 {
                        p.selected = (p.selected + 1).min(len - 1);
                    }
                }
                "up" | "k" | "p" => {
                    let p = self.lsp_ui.popup.as_mut().unwrap();
                    p.selected = p.selected.saturating_sub(1);
                }
                "right" | "l" => self.expand_call(window, cx),
                "left" | "h" => {
                    let p = self.lsp_ui.popup.as_mut().unwrap();
                    let all = rows(&p.content);
                    if let Some(row) = all.get(p.selected)
                        && let Content::Calls { roots, .. } = &mut p.content
                        && let Some(node) = node_mut(roots, &row.path)
                    {
                        if node.expanded {
                            node.expanded = false;
                        } else if row.path.len() > 1 {
                            let parent = &row.path[..row.path.len() - 1];
                            p.selected = all.iter().position(|r| r.path == parent).unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            cx.notify();
            return true;
        }
        if !self.input.focus_handle(cx).is_focused(window) || self.search_open {
            return false;
        }
        match key.key.as_str() {
            "f12" => {
                self.start_lsp_request("textDocument/definition", "definitionProvider", window, cx)
            }
            "f11" => self.start_lsp_request(
                "textDocument/prepareCallHierarchy",
                "callHierarchyProvider",
                window,
                cx,
            ),
            _ if text_key(key) == "K" && self.vim.mode == Mode::Normal => {
                self.start_lsp_request("textDocument/hover", "hoverProvider", window, cx)
            }
            "space" if key.modifiers.control && self.vim.mode == Mode::Insert => {
                if self
                    .buffer
                    .read(cx)
                    .lsp
                    .as_ref()
                    .is_some_and(|doc| doc.read(cx).session.supports("completionProvider"))
                {
                    self.input
                        .update(cx, |input, cx| input.show_completions(window, cx));
                } else {
                    Self::lsp_error(
                        "Completion unavailable; check the language-server status and project devShell",
                        window,
                        cx,
                    );
                }
            }
            "escape"
                if self.vim.mode == Mode::Insert
                    && self.input.read(cx).completion_menu_state().open =>
            {
                self.input
                    .update(cx, |input, cx| input.dismiss_completion_overlay(cx));
            }
            "n" | "p"
                if key.modifiers.control
                    && self.vim.mode == Mode::Insert
                    && self.input.read(cx).completion_menu_state().open =>
            {
                self.input.update(cx, |input, cx| {
                    let action: Box<dyn gpui_kit::Action> = if key.key == "n" {
                        Box::new(gpui_kit::base::input::MoveDown)
                    } else {
                        Box::new(gpui_kit::base::input::MoveUp)
                    };
                    input.route_overlay_action(action, window, cx);
                });
            }
            "escape" => {
                self.cancel_lsp_request();
                self.input
                    .update(cx, |input, cx| input.clear_hover_state(cx));
                return false;
            }
            _ => return false,
        }
        true
    }
    pub(super) fn render_lsp_popup(&self, cx: &mut Context<Self>) -> Option<gpui_kit::Div> {
        let popup = self.lsp_ui.popup.as_ref()?;
        let all = rows(&popup.content);
        let first = popup.selected.saturating_sub(7);
        let is_calls = matches!(popup.content, Content::Calls { .. });
        let direction = match popup.content {
            Content::Calls { direction, .. } => direction,
            _ => Direction::Incoming,
        };
        let heading = div()
            .flex()
            .gap_2()
            .child(popup.title.clone())
            .when(is_calls, |heading| {
                heading
                    .child(
                        Button::new("incoming-calls")
                            .label(if direction == Direction::Incoming {
                                "● Incoming calls"
                            } else {
                                "Incoming calls"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.switch_calls(Direction::Incoming, window, cx)
                            })),
                    )
                    .child(
                        Button::new("outgoing-calls")
                            .label(if direction == Direction::Outgoing {
                                "● Outgoing calls"
                            } else {
                                "Outgoing calls"
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.switch_calls(Direction::Outgoing, window, cx)
                            })),
                    )
            });
        let body = div()
            .id("lsp-popup")
            .test_support()
            .role(Role::Group)
            .aria_label(popup.title.clone())
            .track_focus(&self.lsp_ui.focus)
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(heading)
            .when(!popup.message.is_empty(), |body| {
                body.child(popup.message.clone())
            })
            .children(
                all.iter()
                    .enumerate()
                    .skip(first)
                    .take(9)
                    .map(|(index, row)| {
                        let prefix = if !is_calls {
                            ""
                        } else if row.loading {
                            "… "
                        } else if row.expanded {
                            "▾ "
                        } else {
                            "▸ "
                        };
                        div()
                            .id(("lsp-result", index))
                            .test_support()
                            .role(Role::ListBoxOption)
                            .aria_label(row.label.clone())
                            .aria_selected(index == popup.selected)
                            .pl(px(8. + row.depth as f32 * 16.))
                            .py_1()
                            .cursor_pointer()
                            .when(index == popup.selected, |row| row.bg(cx.theme().accent))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(
                                    move |this, event: &gpui_kit::MouseDownEvent, window, cx| {
                                        if let Some(popup) = &mut this.lsp_ui.popup {
                                            popup.selected = index;
                                        }
                                        if event.click_count == 2 {
                                            this.accept_lsp_row(window, cx);
                                        } else {
                                            this.lsp_ui.focus.focus(window, cx);
                                            cx.notify();
                                        }
                                    },
                                ),
                            )
                            .child(format!("{prefix}{}", row.label))
                    }),
            )
            .child(
                div()
                    .text_color(cx.theme().muted_foreground)
                    .child("↑/↓ Select · ←/→ Expand · Tab Direction · Enter Open · Esc Close"),
            );
        Some(
            div()
                .w_full()
                .flex()
                .flex_col()
                .border_t_1()
                .border_color(cx.theme().border)
                .bg(cx.theme().popover)
                .text_color(cx.theme().popover_foreground)
                .child(body),
        )
    }
    pub(super) fn invoke_lens(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.buffer.read(cx).lsp.clone() else {
            return;
        };
        let Some(snap) = self.lsp_ui.lens_snapshot.clone() else {
            return;
        };
        if !doc.read(cx).is_current(&snap, cx) {
            return;
        }
        let Some(command) = self.lsp_ui.lenses.get(id).and_then(|l| l.command.clone()) else {
            return;
        };
        let encoding = doc.read(cx).session.encoding();
        let args = command.arguments.clone().unwrap_or_default();
        if command.command.ends_with("showReferences")
            && let Some(value) = args.get(2)
            && let Ok(locations) = serde_json::from_value::<Vec<Location>>(value.clone())
        {
            self.show_locations(locations, snap, encoding, "References", window, cx);
            return;
        }
        if command.command.ends_with("gotoLocation")
            && let Some(value) = args.first()
            && let Ok(locations) = providers::locations(value.clone())
        {
            self.show_locations(
                locations
                    .into_iter()
                    .map(|l| Location::new(l.target_uri, l.target_selection_range))
                    .collect(),
                snap,
                encoding,
                "Location",
                window,
                cx,
            );
            return;
        }
        let supported = doc.read(cx).session.capabilities()["executeCommandProvider"]["commands"]
            .as_array()
            .is_some_and(|list| list.iter().any(|v| v.as_str() == Some(&command.command)));
        if !supported {
            Self::lsp_error(
                format!("Unsupported Code lens command: {}", command.command),
                window,
                cx,
            );
            return;
        }
        let response = doc.read(cx).session.request(
            "workspace/executeCommand",
            json!({"command":command.command,"arguments":args}),
        );
        self.lsp_ui.task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = response.recv().await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Err(e) => Self::lsp_error(e, window, cx),
                Ok(value) => {
                    if doc.read(cx).is_current(&snap, cx)
                        && !value.is_null()
                        && let Ok(locations) = providers::locations(value)
                    {
                        this.show_locations(
                            locations
                                .into_iter()
                                .map(|l| Location::new(l.target_uri, l.target_selection_range))
                                .collect(),
                            snap,
                            encoding,
                            "Code lens",
                            window,
                            cx,
                        );
                    }
                }
            });
        }));
    }
}
fn editor_tokens(
    tokens: &SemanticTokens,
    text: &str,
    encoding: Encoding,
) -> anyhow::Result<SemanticTokens> {
    // Index once; rescanning the entire document for every token is quadratic.
    let lines = text.split('\n').collect::<Vec<_>>();
    let mut out = Vec::with_capacity(tokens.data.len());
    let (mut line, mut col, mut old_line, mut old_col) = (0u32, 0u32, 0u32, 0u32);
    for t in &tokens.data {
        line = line
            .checked_add(t.delta_line)
            .ok_or_else(|| anyhow::anyhow!("Invalid semantic token"))?;
        col = if t.delta_line == 0 {
            col.checked_add(t.delta_start)
                .ok_or_else(|| anyhow::anyhow!("Invalid semantic token"))?
        } else {
            t.delta_start
        };
        let text = lines
            .get(line as usize)
            .ok_or_else(|| anyhow::anyhow!("Invalid semantic token line"))?;
        let start = encoding.offset(text, Position::new(0, col))?;
        let end = encoding.offset(
            text,
            Position::new(
                0,
                col.checked_add(t.length)
                    .ok_or_else(|| anyhow::anyhow!("Invalid semantic token"))?,
            ),
        )?;
        let start = Encoding::Utf32.position(text, start)?;
        let end = Encoding::Utf32.position(text, end)?;
        out.push(SemanticToken {
            delta_line: line - old_line,
            delta_start: if line == old_line {
                start.character - old_col
            } else {
                start.character
            },
            length: end.character - start.character,
            token_type: t.token_type,
            token_modifiers_bitset: t.token_modifiers_bitset,
        });
        old_line = line;
        old_col = start.character;
    }
    Ok(SemanticTokens {
        result_id: tokens.result_id.clone(),
        data: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn semantic_tokens_use_editor_scalar_columns_for_utf16_and_utf8() {
        let text = "日本🎸 abc\nz";
        let tokens =
            serde_json::from_value(json!({"data":[0,0,2,0,0, 0,2,2,0,0, 0,3,3,0,0, 1,0,1,0,0]}))
                .unwrap();
        let converted = editor_tokens(&tokens, text, Encoding::Utf16).unwrap();
        assert_eq!(
            serde_json::to_value(converted).unwrap()["data"],
            json!([0, 0, 2, 0, 0, 0, 2, 1, 0, 0, 0, 2, 3, 0, 0, 1, 0, 1, 0, 0])
        );
        let tokens = serde_json::from_value(json!({"data":[0,6,4,0,0, 0,5,3,0,0]})).unwrap();
        let converted = editor_tokens(&tokens, text, Encoding::Utf8).unwrap();
        assert_eq!(
            serde_json::to_value(converted).unwrap()["data"],
            json!([0, 2, 1, 0, 0, 0, 2, 3, 0, 0])
        );
        let split_surrogate = serde_json::from_value(json!({"data":[0,3,1,0,0]})).unwrap();
        assert!(editor_tokens(&split_surrogate, text, Encoding::Utf16).is_err());
    }
}
