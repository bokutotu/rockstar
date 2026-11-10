//! Retained read-only diff items; view state stays local to each pane.
mod git;
mod model;
mod presentation;
mod tree;
mod view;
mod watch;

pub(crate) use git::resolve_watch_target;
pub(crate) use model::DiffSnapshot;
pub(crate) use model::{DiffRequest, IndexAction, LocalFilter};
pub(crate) use view::DiffView;
pub(crate) use watch::DiffWatches;

use anyhow::{Result, ensure};
use gpui_kit::{AppContext as _, Context, EntityId};
use model::DiffEntry;
use std::{collections::HashSet, path::Path, sync::Arc};

/// Leave room for the file tree and two source columns when opening a diff.
/// Never shrink a user's window or request a size larger than the display.
pub(crate) fn opening_size(
    current: gpui_kit::Size<gpui_kit::Pixels>,
    available: gpui_kit::Size<gpui_kit::Pixels>,
) -> gpui_kit::Size<gpui_kit::Pixels> {
    gpui_kit::size(
        current.width.max(gpui_kit::px(960.).min(available.width)),
        current.height.max(gpui_kit::px(600.).min(available.height)),
    )
}

pub(crate) fn load(directory: &Path, request: DiffRequest) -> Result<DiffSnapshot> {
    git::load(directory, request)
}

pub(crate) struct DiffSession {
    snapshot: Arc<DiffSnapshot>,
    revision: u64,
    pending: Option<&'static str>,
    error: Option<String>,
    visible_views: HashSet<EntityId>,
    needs_refresh: bool,
}

impl DiffSession {
    pub(crate) fn new(snapshot: DiffSnapshot) -> Self {
        Self {
            snapshot: Arc::new(snapshot),
            revision: 0,
            pending: None,
            error: None,
            visible_views: HashSet::new(),
            needs_refresh: false,
        }
    }

    pub(crate) fn label(&self) -> String {
        self.snapshot.source.label()
    }

    fn set_visible(&mut self, view: EntityId, visible: bool, cx: &mut Context<Self>) {
        if visible {
            let first = self.visible_views.is_empty();
            self.visible_views.insert(view);
            if first && self.needs_refresh {
                self.refresh(cx);
            }
        } else if self.visible_views.remove(&view) && self.visible_views.is_empty() {
            self.needs_refresh = true;
        }
    }

    fn invalidate(&mut self, cx: &mut Context<Self>) {
        self.needs_refresh = true;
        if !self.visible_views.is_empty() && self.pending.is_none() {
            self.run(None, cx);
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.needs_refresh = true;
        if self.pending.is_none() {
            self.run(None, cx);
        }
    }

    fn update_index(
        &mut self,
        entry: DiffEntry,
        action: IndexAction,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        ensure!(
            self.pending.is_none(),
            "A diff operation is already running; please wait"
        );
        git::validate_action(&self.snapshot, &entry, action)?;
        self.run(Some((entry, action)), cx);
        Ok(())
    }

    fn run(&mut self, operation: Option<(DiffEntry, IndexAction)>, cx: &mut Context<Self>) {
        // Only the operation consuming this flag clears it. Notifications received
        // during a read/index write must schedule another read after completion.
        self.needs_refresh = false;
        let snapshot = self.snapshot.clone();
        self.pending = Some(
            operation
                .as_ref()
                .map_or("Refreshing…", |(_, action)| action.label()),
        );
        self.error = None;
        let loading = cx.background_spawn(async move {
            let write_error = operation.as_ref().and_then(|(entry, action)| {
                git::update_index(&snapshot, entry, *action)
                    .err()
                    .map(|error| format!("{error:#}"))
            });
            let refreshed = git::refresh_diff(&snapshot);
            (operation.is_some(), write_error, refreshed)
        });
        // The task holds only a weak entity. Closing/wiping an item cannot
        // resurrect it when a read or index operation completes.
        cx.spawn(async move |this, cx| {
            let (wrote, write_error, result) = loading.await;
            let _ = this.update(cx, |this, cx| {
                this.pending = None;
                this.error = write_error;
                match result {
                    Ok(snapshot) => {
                        this.snapshot = Arc::new(snapshot);
                        this.revision += 1;
                    }
                    Err(error) => {
                        let prefix = if wrote && this.error.is_none() {
                            "Index updated, but diff refresh failed"
                        } else {
                            "Diff refresh failed"
                        };
                        let message = format!("{prefix}: {error:#}");
                        this.error = Some(this.error.take().map_or_else(
                            || message.clone(),
                            |previous| format!("{previous}\n{message}"),
                        ));
                    }
                }
                if this.needs_refresh && !this.visible_views.is_empty() {
                    this.run(None, cx);
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }
}
