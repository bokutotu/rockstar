//! Adapt the native editor's providers to a versioned document connection.
use super::{DocumentBinding, DocumentSnapshot, language::Encoding};
use anyhow::{Result, anyhow, bail};
use gpui_kit::{
    App, Entity, Task, WeakEntity, Window,
    base::input::{CompletionProvider, DefinitionProvider, EditorState, HoverProvider, Rope},
};
use lsp_types::{
    CompletionContext, CompletionResponse, CompletionTextEdit, Hover, LocationLink, TextEdit,
};
use serde_json::{Value, json};
use std::rc::Rc;

pub(crate) struct Provider {
    document: WeakEntity<DocumentBinding>,
    editor: WeakEntity<EditorState>,
}
impl Provider {
    pub fn new(document: &Entity<DocumentBinding>, editor: &Entity<EditorState>) -> Rc<Self> {
        Rc::new(Self {
            document: document.downgrade(),
            editor: editor.downgrade(),
        })
    }
    fn fetch(
        &self,
        text: &Rope,
        offset: usize,
        method: &'static str,
        capability: &'static str,
        extra: Value,
        cx: &App,
    ) -> Task<Result<(Value, DocumentSnapshot, Encoding)>> {
        let document = self.document.clone();
        let editor = self.editor.clone();
        let expected = text.to_string();
        cx.spawn(async move |cx| {
            let (response, snap, encoding) = document.update(cx, |doc, cx| -> Result<_> {
                let snap = doc.snapshot(cx)?;
                if snap.text.as_ref() != expected {
                    bail!("Document changed");
                }
                if !doc.session.supports(capability) {
                    bail!("Language server does not support {method}");
                }
                let encoding = doc.session.encoding();
                let mut params = extra;
                params["position"] = json!(encoding.position(&snap.text, offset)?);
                let response = doc.request(method, &snap, params, cx)?;
                Ok((response, snap, encoding))
            })??;
            let value = response.recv().await?;
            if !document.read_with(cx, |doc, cx| doc.is_current(&snap, cx))? {
                bail!("Stale LSP response");
            }
            if method == "textDocument/completion"
                && editor.read_with(cx, |editor, _| editor.cursor())? != offset
            {
                bail!("Cursor moved");
            }
            Ok((value, snap, encoding))
        })
    }
}
impl HoverProvider for Provider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<Hover>>> {
        let response = self.fetch(
            text,
            offset,
            "textDocument/hover",
            "hoverProvider",
            json!({}),
            cx,
        );
        cx.spawn(async move |_| {
            let (value, snap, encoding) = response.await?;
            let mut hover: Option<Hover> = serde_json::from_value(value)?;
            if let Some(hover) = &mut hover
                && let Some(range) = hover.range
            {
                hover.range = Some(encoding.editor_range(&snap.text, range)?);
            }
            Ok(hover)
        })
    }
}
impl DefinitionProvider for Provider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<LocationLink>>> {
        let response = self.fetch(
            text,
            offset,
            "textDocument/definition",
            "definitionProvider",
            json!({}),
            cx,
        );
        cx.spawn(async move |_| {
            let (value, snap, encoding) = response.await?;
            let mut locations = locations(value)?;
            for location in &mut locations {
                if let Some(range) = location.origin_selection_range {
                    location.origin_selection_range =
                        Some(encoding.editor_range(&snap.text, range)?);
                }
            }
            Ok(locations)
        })
    }
}
impl CompletionProvider for Provider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        // The editor also labels ordinary identifier typing as TRIGGER_CHARACTER. Only send
        // a trigger character if the server actually advertised that single character.
        let mut context = json!({"triggerKind":1});
        if let Some(trigger) = trigger.trigger_character {
            let valid = self
                .document
                .read_with(cx, |doc, _| {
                    doc.session.capabilities()["completionProvider"]["triggerCharacters"]
                        .as_array()
                        .is_some_and(|chars| chars.iter().any(|c| c.as_str() == Some(&trigger)))
                })
                .unwrap_or(false);
            if valid {
                context = json!({"triggerKind":2,"triggerCharacter":trigger});
            }
        }
        let response = self.fetch(
            text,
            offset,
            "textDocument/completion",
            "completionProvider",
            json!({"context":context}),
            cx,
        );
        cx.spawn(async move |_| {
            let (value, snap, encoding) = response.await?;
            let mut result: CompletionResponse =
                serde_json::from_value(if value.is_null() { json!([]) } else { value })?;
            let items = match &mut result {
                CompletionResponse::Array(items) => items,
                CompletionResponse::List(list) => &mut list.items,
            };
            let start = snap.text[..offset]
                .char_indices()
                .rev()
                .take_while(|(_, c)| c.is_alphanumeric() || *c == '_')
                .last()
                .map_or(offset, |(i, _)| i);
            for item in items {
                if let Some(edit) = &mut item.text_edit {
                    match edit {
                        CompletionTextEdit::Edit(edit) => {
                            edit.range = encoding.editor_range(&snap.text, edit.range)?
                        }
                        CompletionTextEdit::InsertAndReplace(edit) => {
                            edit.insert = encoding.editor_range(&snap.text, edit.insert)?;
                            edit.replace = encoding.editor_range(&snap.text, edit.replace)?;
                        }
                    }
                } else {
                    item.text_edit = Some(CompletionTextEdit::Edit(TextEdit::new(
                        lsp_types::Range::new(
                            Encoding::Utf32.position(&snap.text, start)?,
                            Encoding::Utf32.position(&snap.text, offset)?,
                        ),
                        item.insert_text
                            .clone()
                            .unwrap_or_else(|| item.label.clone()),
                    )));
                }
                if let Some(edits) = &mut item.additional_text_edits {
                    for edit in edits.iter_mut() {
                        edit.range = encoding.editor_range(&snap.text, edit.range)?;
                    }
                    let mut all = edits.clone();
                    match item.text_edit.as_ref().unwrap() {
                        CompletionTextEdit::Edit(edit) => all.push(edit.clone()),
                        CompletionTextEdit::InsertAndReplace(edit) => {
                            all.push(TextEdit::new(edit.replace, edit.new_text.clone()))
                        }
                    }
                    // Reject overlapping additional edits before exposing an unsafe completion.
                    super::language::apply_edits(&snap.text, &all, Encoding::Utf32)?;
                }
            }
            Ok(result)
        })
    }
    fn is_completion_trigger(&self, _: usize, new_text: &str, cx: &mut App) -> bool {
        if new_text
            .chars()
            .last()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            return true;
        }
        self.document
            .read_with(cx, |doc, _| {
                doc.session.capabilities()["completionProvider"]["triggerCharacters"]
                    .as_array()
                    .is_some_and(|chars| chars.iter().any(|c| c.as_str() == Some(new_text)))
            })
            .unwrap_or(false)
    }
}
pub(crate) fn locations(value: Value) -> Result<Vec<LocationLink>> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    let values = if let Value::Array(values) = value {
        values
    } else {
        vec![value]
    };
    values
        .into_iter()
        .map(|value| {
            if value.get("targetUri").is_some() {
                Ok(serde_json::from_value(value)?)
            } else {
                let location: lsp_types::Location = serde_json::from_value(value)
                    .map_err(|e| anyhow!("Invalid definition response: {e}"))?;
                Ok(LocationLink {
                    origin_selection_range: None,
                    target_uri: location.uri,
                    target_range: location.range,
                    target_selection_range: location.range,
                })
            }
        })
        .collect()
}
