//! Dynamic per-language-server behavior injected into the shared JSON-RPC transport.
use super::{haskell::HaskellAdapter, language::ServerKind, transport::CommandSpec};
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

pub(crate) trait ServerAdapter: Send {
    fn resolve_root(&self, workspace_root: &Path, file_path: &Path) -> PathBuf {
        let _ = file_path;
        workspace_root.to_path_buf()
    }

    fn command(&self, root: &Path) -> CommandSpec;

    fn customize_initialize(&mut self, root: &Path, params: &mut Value) {
        let _ = (root, params);
    }

    fn on_initialized(
        &mut self,
        root: &Path,
        result: &Value,
        writer: &mpsc::SyncSender<Value>,
    ) -> Result<()> {
        let _ = (root, result, writer);
        Ok(())
    }

    fn configuration(&self, section: Option<&str>) -> Value {
        let _ = section;
        json!({})
    }

    fn handle_client_request(&mut self, method: &str, params: &Value) -> Option<Value> {
        let _ = (method, params);
        None
    }

    fn on_client_message(
        &mut self,
        method: &str,
        params: &Value,
        writer: &mpsc::SyncSender<Value>,
    ) {
        let _ = (method, params, writer);
    }
}

pub(crate) fn create(kind: ServerKind) -> Box<dyn ServerAdapter> {
    match kind {
        ServerKind::Haskell => Box::new(HaskellAdapter::new()),
        _ => Box::new(StandardAdapter { kind }),
    }
}

pub(crate) fn lookup_section(settings: &Value, section: Option<&str>) -> Value {
    section.map_or_else(
        || settings.clone(),
        |section| {
            section
                .split('.')
                .try_fold(settings, |value, key| value.get(key))
                .cloned()
                .unwrap_or_else(|| json!({}))
        },
    )
}

struct StandardAdapter {
    kind: ServerKind,
}

impl StandardAdapter {
    fn settings(&self) -> Value {
        match self.kind {
            ServerKind::TypeScript => json!({
                "typescript": {
                    "referencesCodeLens": { "enabled": true, "showOnAllFunctions": true },
                    "implementationsCodeLens": { "enabled": true }
                }
            }),
            ServerKind::Rust => json!({
                "rust-analyzer": {
                    "lens": { "enable": true, "references": { "enable": true } }
                }
            }),
            _ => json!({}),
        }
    }
}

impl ServerAdapter for StandardAdapter {
    fn command(&self, _: &Path) -> CommandSpec {
        CommandSpec::from(self.kind)
    }

    fn customize_initialize(&mut self, _: &Path, params: &mut Value) {
        params["initializationOptions"] = match self.kind {
            ServerKind::TypeScript => json!({
                "hostInfo": "rockstar",
                "disableAutomaticTypingAcquisition": true,
                "preferences": {
                    "includeInlayParameterNameHints": "all",
                    "includeInlayParameterNameHintsWhenArgumentMatchesName": true,
                    "includeInlayFunctionParameterTypeHints": true,
                    "includeInlayVariableTypeHints": true,
                    "includeInlayPropertyDeclarationTypeHints": true,
                    "includeInlayFunctionLikeReturnTypeHints": true,
                    "includeInlayEnumMemberValueHints": true
                }
            }),
            ServerKind::Rust => self.settings()["rust-analyzer"].clone(),
            _ => Value::Null,
        };
    }

    fn on_initialized(
        &mut self,
        _: &Path,
        _: &Value,
        writer: &mpsc::SyncSender<Value>,
    ) -> Result<()> {
        if matches!(self.kind, ServerKind::TypeScript | ServerKind::Rust) {
            super::transport::queue_message(
                writer,
                &json!({
                    "jsonrpc": "2.0",
                    "method": "workspace/didChangeConfiguration",
                    "params": { "settings": self.settings() }
                }),
            )?;
        }
        Ok(())
    }

    fn configuration(&self, section: Option<&str>) -> Value {
        lookup_section(&self.settings(), section)
    }
}
