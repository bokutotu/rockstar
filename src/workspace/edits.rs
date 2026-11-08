//! Apply LSP document edits as a validated in-memory batch; never save or
//! perform resource operations implicitly.
use super::*;
use crate::lsp::language::{self, Encoding};
use lsp_types::{DocumentChangeOperation, DocumentChanges, OneOf, WorkspaceEdit};

impl Tab {
    pub(super) fn apply_workspace_edit(
        &mut self,
        edit: WorkspaceEdit,
        encoding: Encoding,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let result = (|| -> anyhow::Result<_> {
            anyhow::ensure!(
                edit.change_annotations
                    .as_ref()
                    .is_none_or(|annotations| annotations
                        .values()
                        .all(|a| a.needs_confirmation != Some(true))),
                "This edit requires confirmation of annotated changes"
            );
            let mut edits = Vec::new();
            if let Some(changes) = edit.changes {
                for (uri, changes) in changes {
                    edits.push((language::path(&uri)?, None, changes));
                }
            }
            if let Some(changes) = edit.document_changes {
                let changes = match changes {
                    DocumentChanges::Edits(edits) => edits,
                    DocumentChanges::Operations(ops) => ops.into_iter().map(|op| match op {
                        DocumentChangeOperation::Edit(edit) => Ok(edit),
                        _ => anyhow::bail!("LSP file creation, deletion and rename require explicit file operations"),
                    }).collect::<anyhow::Result<Vec<_>>>()?,
                };
                for edit in changes {
                    let changes = edit
                        .edits
                        .into_iter()
                        .map(|edit| match edit {
                            OneOf::Left(edit) => edit,
                            OneOf::Right(edit) => edit.text_edit,
                        })
                        .collect();
                    edits.push((
                        language::path(&edit.text_document.uri)?,
                        edit.text_document.version,
                        changes,
                    ));
                }
            }
            let mut tasks = Vec::new();
            let mut paths = std::collections::HashSet::new();
            for (path, version, edits) in edits {
                let path = path.canonicalize()?;
                anyhow::ensure!(
                    paths.insert(path.clone()),
                    "Multiple edit batches for {}",
                    path.display()
                );
                let buffer = self.find_buffer(&path, cx);
                if let Some(version) = version {
                    anyhow::ensure!(
                        buffer
                            .as_ref()
                            .is_some_and(|buffer| buffer.read(cx).version == version),
                        "Stale edits for {}",
                        path.display()
                    );
                }
                let version = buffer.as_ref().map(|buffer| buffer.read(cx).version);
                let snapshot = buffer.map(|buffer| buffer.read(cx).text.clone());
                tasks.push((path, edits, snapshot, version));
            }
            Ok(tasks)
        })();
        let tasks = match result {
            Ok(tasks) => tasks,
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()), cx);
                return;
            }
        };
        self.vim.update(cx, |vim, _| {
            if vim.macros.running {
                vim.macros.waiting = true;
            }
        });
        let preparing = cx.background_spawn(async move {
            tasks
                .into_iter()
                .map(|(path, edits, snapshot, version)| {
                    let document = if snapshot.is_none() {
                        Some(FileBuffer::load(&path)?)
                    } else {
                        None
                    };
                    let before =
                        snapshot.unwrap_or_else(|| document.as_ref().unwrap().text.clone());
                    let after = language::apply_edits(&before, &edits, encoding)?;
                    Ok((path, document, before, after, version))
                })
                .collect::<anyhow::Result<Vec<_>>>()
        });
        cx.spawn_in(window, async move |this, cx| {
            let prepared = preparing.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.vim.update(cx, |vim, _| vim.macros.waiting = false);
                let result = (|| -> anyhow::Result<()> {
                    let prepared = prepared?;
                    for (path, document, before, _, version) in &prepared {
                        if let Some(buffer) = this.find_buffer(path, cx) {
                            anyhow::ensure!(
                                buffer.read(cx).text == *before
                                    && version.is_none_or(|v| buffer.read(cx).version == v),
                                "Edits discarded: {} changed while preparing",
                                path.display()
                            );
                        } else {
                            anyhow::ensure!(document.is_some(), "Target buffer was closed");
                        }
                    }
                    let source = cx.entity_id();
                    let mut changes = Vec::new();
                    for (path, document, _, after, _) in prepared {
                        let buffer = if let Some(buffer) = this.find_buffer(&path, cx) {
                            buffer
                        } else {
                            let document = document
                                .ok_or_else(|| anyhow::anyhow!("Target buffer was closed"))?;
                            let buffer = cx.new(|_| document);
                            match this.lsp.update(cx, |lsp, cx| lsp.bind(&buffer, cx)) {
                                Ok(binding) => buffer.update(cx, |buffer, _| buffer.lsp = binding),
                                Err(error) => window
                                    .push_notification(Notification::error(error.to_string()), cx),
                            }
                            this.register_item(OpenItem::File(buffer.clone()), cx);
                            buffer
                        };
                        changes.push((buffer, after));
                    }
                    // All fallible preparation precedes the first text mutation.
                    for (buffer, after) in changes {
                        buffer.update(cx, |buffer, cx| {
                            buffer.edit(after, 0, 0, source, false, None, cx)
                        });
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    this.vim.update(cx, |vim, _| vim.macros.stop());
                    window.push_notification(Notification::error(error.to_string()), cx);
                }
            });
        })
        .detach();
    }
}
