//! Workspace-scoped language servers and versioned, shared document analysis.
pub(crate) mod language;
pub(crate) mod providers;
mod transport;

use crate::file_buffer::{BufferEvent, FileBuffer};
use anyhow::{Result, anyhow, bail};
use gpui_kit::{
    App, AppContext as _, Context, Entity, Task, WeakEntity, Window,
    component::{WindowExt as _, notification::Notification},
};
use language::ServerKind;
use lsp_types::{CodeLens, Diagnostic, InlayHint, SemanticTokens, Uri};
use serde_json::{Value, json};
use std::{collections::HashMap, path::PathBuf, sync::Arc, time::Duration};
pub(crate) use transport::{CommandSpec, ServerSession, ServerState};

#[derive(Clone)]
pub(crate) struct DocumentSnapshot {
    pub uri: Uri,
    pub language_id: &'static str,
    pub version: i32,
    pub text: Arc<str>,
}
#[derive(Clone)]
pub(crate) struct Versioned<T> {
    pub version: i32,
    pub value: T,
}
impl<T: Default> Default for Versioned<T> {
    fn default() -> Self {
        Self {
            version: -1,
            value: T::default(),
        }
    }
}
#[derive(Default)]
pub(crate) struct DocumentAnalysis {
    pub diagnostics: Versioned<Vec<Diagnostic>>,
    pub semantic_tokens: Versioned<Option<SemanticTokens>>,
    pub inlay_hints: Versioned<Vec<InlayHint>>,
    pub code_lenses: Versioned<Vec<CodeLens>>,
}

pub(crate) struct DocumentBinding {
    pub buffer: WeakEntity<FileBuffer>,
    pub uri: Uri,
    pub language_id: &'static str,
    pub session: Arc<ServerSession>,
    pub analysis: DocumentAnalysis,
    sent: Option<DocumentSnapshot>,
    refresh: Option<Task<()>>,
    requests: Vec<Task<()>>,
}
impl Drop for DocumentBinding {
    fn drop(&mut self) {
        if self.sent.is_some() {
            let _ = self.session.notify(
                "textDocument/didClose",
                json!({"textDocument":{"uri":self.uri}}),
            );
        }
    }
}
impl DocumentBinding {
    fn new(
        buffer: &Entity<FileBuffer>,
        uri: Uri,
        language_id: &'static str,
        session: Arc<ServerSession>,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.observe(buffer, |this, buffer, cx| {
            let version = buffer.read(cx).version;
            let analysis = &this.analysis;
            if [
                analysis.diagnostics.version,
                analysis.semantic_tokens.version,
                analysis.inlay_hints.version,
                analysis.code_lenses.version,
            ]
            .iter()
            .any(|&v| v >= 0 && v != version)
            {
                this.analysis = DocumentAnalysis::default();
                cx.notify();
            }
            let _ = this.sync(cx);
        })
        .detach();
        cx.subscribe(buffer, |this, _, event: &BufferEvent, cx| match event {
            BufferEvent::Saved(text) => {
                let _ = this.sync(cx);
                let caps = this.session.capabilities();
                let save = &caps["textDocumentSync"]["save"];
                if save.is_object() || save.as_bool() == Some(true) {
                    let mut params = json!({"textDocument":{"uri":this.uri}});
                    if save["includeText"] == true {
                        params["text"] = json!(text);
                    }
                    let _ = this.session.notify("textDocument/didSave", params);
                }
            }
        })
        .detach();
        Self {
            buffer: buffer.downgrade(),
            uri,
            language_id,
            session,
            analysis: DocumentAnalysis::default(),
            sent: None,
            refresh: None,
            requests: Vec::new(),
        }
    }
    pub fn snapshot(&self, cx: &App) -> Result<DocumentSnapshot> {
        let buffer = self
            .buffer
            .upgrade()
            .ok_or_else(|| anyhow!("Document closed"))?;
        let buffer = buffer.read(cx);
        if let Some(sent) = &self.sent
            && sent.version == buffer.version
        {
            return Ok(sent.clone());
        }
        Ok(DocumentSnapshot {
            uri: self.uri.clone(),
            language_id: self.language_id,
            version: buffer.version,
            text: Arc::from(buffer.text.as_str()),
        })
    }
    pub fn is_current(&self, snap: &DocumentSnapshot, cx: &App) -> bool {
        self.uri == snap.uri
            && self
                .buffer
                .upgrade()
                .is_some_and(|buffer| buffer.read(cx).version == snap.version)
    }
    pub fn sync(&mut self, cx: &mut Context<Self>) -> Result<bool> {
        if !matches!(self.session.state(), ServerState::Ready { .. }) {
            return Ok(false);
        }
        let now = self.snapshot(cx)?;
        if self.sent.as_ref().is_some_and(|s| s.version == now.version) {
            return Ok(false);
        }
        if let Some(before) = &self.sent {
            let caps = self.session.capabilities();
            let sync = &caps["textDocumentSync"];
            let kind = sync
                .as_u64()
                .or_else(|| sync["change"].as_u64())
                .unwrap_or(0);
            if kind != 0 {
                let change =
                    language::change(&before.text, &now.text, self.session.encoding(), kind == 2)?;
                self.session.notify("textDocument/didChange",json!({"textDocument":{"uri":now.uri,"version":now.version},"contentChanges":[change]}))?;
            }
            self.analysis = DocumentAnalysis::default();
        } else {
            self.session.notify("textDocument/didOpen",json!({"textDocument":{"uri":now.uri,"languageId":now.language_id,"version":now.version,"text":now.text.as_ref()}}))?;
        }
        self.sent = Some(now);
        self.schedule_refresh(cx);
        cx.notify();
        Ok(true)
    }
    pub fn request(
        &mut self,
        method: &str,
        snap: &DocumentSnapshot,
        mut params: Value,
        cx: &mut Context<Self>,
    ) -> Result<transport::Response> {
        self.sync(cx)?;
        if !self.is_current(snap, cx) {
            bail!("Document changed before LSP request");
        }
        match self.session.state() {
            ServerState::Ready { .. } => {}
            ServerState::Failed(error) => bail!("{error}"),
            _ => bail!("Language server is not ready"),
        }
        params["textDocument"] = json!({"uri":self.uri});
        Ok(self.session.request(method, params))
    }
    pub fn request_at(
        &mut self,
        method: &str,
        snap: &DocumentSnapshot,
        offset: usize,
        cx: &mut Context<Self>,
    ) -> Result<transport::Response> {
        self.request(
            method,
            snap,
            json!({"position":self.session.encoding().position(&snap.text,offset)?}),
            cx,
        )
    }
    pub fn schedule_refresh(&mut self, cx: &mut Context<Self>) {
        self.requests.clear();
        self.refresh = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            let _ = this.update(cx, |this, cx| this.refresh_analysis(cx));
        }));
    }
    fn refresh_analysis(&mut self, cx: &mut Context<Self>) {
        let Ok(snap) = self.snapshot(cx) else {
            return;
        };
        let caps = self.session.capabilities();
        let encoding = self.session.encoding();
        let mut queries = Vec::new();
        if self.session.supports("inlayHintProvider")
            && let Ok(end) = encoding.position(&snap.text, snap.text.len())
        {
            queries.push((
                "textDocument/inlayHint",
                json!({"range":{"start":{"line":0,"character":0},"end":end}}),
            ));
        }
        if self.session.supports("codeLensProvider") {
            queries.push(("textDocument/codeLens", json!({})));
        }
        if self.session.supports("diagnosticProvider") {
            queries.push(("textDocument/diagnostic", json!({})));
        }
        if self.session.supports("semanticTokensProvider") {
            if caps["semanticTokensProvider"]["full"].is_object()
                || caps["semanticTokensProvider"]["full"] == true
            {
                queries.push(("textDocument/semanticTokens/full", json!({})));
            } else if (caps["semanticTokensProvider"]["range"].is_object()
                || caps["semanticTokensProvider"]["range"] == true)
                && let Ok(end) = encoding.position(&snap.text, snap.text.len())
            {
                queries.push((
                    "textDocument/semanticTokens/range",
                    json!({"range":{"start":{"line":0,"character":0},"end":end}}),
                ));
            }
        }
        for (method, params) in queries {
            let Ok(response) = self.request(method, &snap, params, cx) else {
                continue;
            };
            let snap = snap.clone();
            let session = self.session.clone();
            self.requests.push(cx.spawn(async move |this, cx| {
                let Ok(mut value) = response.recv().await else {
                    return;
                };
                if method == "textDocument/codeLens"
                    && session.capabilities()["codeLensProvider"]["resolveProvider"] == true
                    && let Some(lenses) = value.as_array_mut()
                {
                    for lens in lenses {
                        if lens.get("command").is_none_or(Value::is_null) {
                            let current = this
                                .read_with(cx, |this, cx| this.is_current(&snap, cx))
                                .unwrap_or(false);
                            if !current {
                                return;
                            }
                            if let Ok(resolved) = session
                                .request("codeLens/resolve", lens.clone())
                                .recv()
                                .await
                            {
                                *lens = resolved;
                            }
                        }
                    }
                }
                let _ = this.update(cx, |this, cx| {
                    if !this.is_current(&snap, cx) {
                        return;
                    }
                    let version = snap.version;
                    match method {
                        "textDocument/inlayHint" => {
                            if let Ok(value) = serde_json::from_value(null_array(value)) {
                                this.analysis.inlay_hints = Versioned { version, value };
                            }
                        }
                        "textDocument/codeLens" => {
                            if let Ok(value) = serde_json::from_value(null_array(value)) {
                                this.analysis.code_lenses = Versioned { version, value };
                            }
                        }
                        "textDocument/diagnostic" => {
                            if let Some(items) = value.get("items")
                                && let Ok(value) = serde_json::from_value(items.clone())
                            {
                                this.analysis.diagnostics = Versioned { version, value };
                            }
                        }
                        _ => {
                            if let Ok(value) = serde_json::from_value(value) {
                                this.analysis.semantic_tokens = Versioned { version, value };
                            }
                        }
                    }
                    cx.notify();
                });
            }));
        }
    }
}
fn null_array(value: Value) -> Value {
    if value.is_null() { json!([]) } else { value }
}

pub(crate) struct LspWorkspace {
    root: PathBuf,
    sessions: HashMap<ServerKind, Arc<ServerSession>>,
    documents: HashMap<Uri, Entity<DocumentBinding>>,
    events: async_channel::Sender<transport::Event>,
}
impl LspWorkspace {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (events, rx) = async_channel::unbounded();
        cx.on_app_quit(|this, _| {
            let stopped = this
                .sessions
                .values()
                .map(|session| session.stop())
                .collect::<Vec<_>>();
            async move {
                for done in stopped {
                    let _ = done.recv().await;
                }
            }
        })
        .detach();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(event) = rx.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.handle(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        Self {
            root,
            sessions: HashMap::new(),
            documents: HashMap::new(),
            events,
        }
    }
    pub fn bind(
        &mut self,
        buffer: &Entity<FileBuffer>,
        cx: &mut Context<Self>,
    ) -> Result<Option<Entity<DocumentBinding>>> {
        let file = buffer.read(cx);
        let Some((language_id, kind)) = language::language(&file.path) else {
            return Ok(None);
        };
        let uri = language::uri(&file.path)?;
        if let Some(doc) = self.documents.get(&uri) {
            return Ok(Some(doc.clone()));
        }
        let session = self
            .sessions
            .entry(kind)
            .or_insert_with(|| {
                let command = CommandSpec::from(kind);
                ServerSession::start(kind, self.root.clone(), command, self.events.clone())
            })
            .clone();
        let binding =
            cx.new(|cx| DocumentBinding::new(buffer, uri.clone(), language_id, session, cx));
        binding.update(cx, |doc, cx| {
            let _ = doc.sync(cx);
            doc.schedule_refresh(cx);
        });
        self.documents.insert(uri, binding.clone());
        Ok(Some(binding))
    }
    pub(crate) fn stop(&mut self, _: &mut Context<Self>) {
        self.documents.clear();
        for session in self.sessions.values() {
            session.stop();
        }
        self.sessions.clear();
    }

    pub fn unbind(&mut self, buffer: &Entity<FileBuffer>, cx: &mut Context<Self>) {
        if let Some(binding) = buffer.update(cx, |buffer, _| buffer.lsp.take()) {
            self.documents.remove(&binding.read(cx).uri);
        }
    }

    fn handle(&mut self, event: transport::Event, window: &mut Window, cx: &mut Context<Self>) {
        use transport::Event;
        match event {
            Event::State(kind) => {
                let Some(session) = self.sessions.get(&kind) else {
                    return;
                };
                if let ServerState::Failed(error) = session.state() {
                    window.push_notification(Notification::error(error), cx);
                }
                for doc in self.documents.values() {
                    if doc.read(cx).session.kind == kind {
                        doc.update(cx, |doc, cx| {
                            let _ = doc.sync(cx);
                            doc.schedule_refresh(cx);
                            cx.notify();
                        });
                    }
                }
            }
            Event::Notification(kind, method, params) => {
                if method == "textDocument/publishDiagnostics" {
                    let Some(uri) = params
                        .get("uri")
                        .and_then(Value::as_str)
                        .and_then(|s| s.parse::<Uri>().ok())
                    else {
                        return;
                    };
                    let Some(doc) = self.documents.get(&uri) else {
                        return;
                    };
                    if doc.read(cx).session.kind != kind {
                        return;
                    }
                    doc.update(cx, |doc, cx| {
                        let Ok(snap) = doc.snapshot(cx) else {
                            return;
                        };
                        if params
                            .get("version")
                            .and_then(Value::as_i64)
                            .is_some_and(|v| v != i64::from(snap.version))
                        {
                            return;
                        }
                        if let Ok(value) = serde_json::from_value(params["diagnostics"].clone()) {
                            doc.analysis.diagnostics = Versioned {
                                version: snap.version,
                                value,
                            };
                            cx.notify();
                        }
                    });
                } else if method.ends_with("/refresh") {
                    for doc in self.documents.values() {
                        if doc.read(cx).session.kind == kind {
                            doc.update(cx, |doc, cx| doc.schedule_refresh(cx));
                        }
                    }
                } else if method == "window/showMessage"
                    && params["type"] == 1
                    && let Some(message) = params["message"].as_str()
                {
                    window.push_notification(
                        Notification::error(format!("{}: {message}", kind.command().0)),
                        cx,
                    );
                }
            }
            Event::Request(kind, id, _method, _params) => {
                // A server cannot unexpectedly steal focus or launch an external application.
                if let Some(session) = self.sessions.get(&kind) {
                    session.respond(id, json!({"success":false}));
                }
            }
        }
    }
}
