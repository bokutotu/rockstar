//! Window-level navigation. Each tab retains its own editing and process state.
use super::picker::{self, Candidate, PickerKind};
use super::*;
use crate::file_watch::FileWatches;

struct Workspace {
    id: EntityId,
    root: PathBuf,
    tabs: Vec<Entity<Tab>>,
    active: usize,
}

#[derive(Clone, Copy)]
enum CloseTarget {
    Window,
    Workspace(EntityId),
    Tab(EntityId),
}

pub(crate) struct WorkspaceWindow {
    workspaces: Vec<Workspace>,
    active: usize,
    connect: AgentConnector,
    watches: Entity<FileWatches>,
    opening_diffs: Vec<(EntityId, DiffRequest)>,
}

impl WorkspaceWindow {
    pub(crate) fn new(
        root: PathBuf,
        connect: AgentConnector,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let watches = cx.new(|cx| FileWatches::new(window, cx));
        let mut this = Self {
            workspaces: Vec::new(),
            active: 0,
            connect,
            watches,
            opening_diffs: Vec::new(),
        };
        let root = root.canonicalize().unwrap_or(root);
        let tab = this.create_tab(root.clone(), window, cx);
        tab.update(cx, |tab, _| tab.visible = true);
        this.workspaces.push(Workspace {
            id: tab.entity_id(),
            root,
            tabs: vec![tab],
            active: 0,
        });
        this
    }

    fn create_tab(
        &self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Tab> {
        let tab =
            cx.new(|cx| Tab::new(root, self.connect.clone(), self.watches.clone(), window, cx));
        cx.observe(&tab, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(&tab, window, |this, tab, action: &TabAction, window, cx| {
            if this.active_tab() != *tab {
                return;
            }
            match action {
                TabAction::Workspaces => this.open_picker(PickerKind::Workspaces, window, cx),
                TabAction::Tabs => this.open_picker(PickerKind::Tabs, window, cx),
                TabAction::CloseWorkspace => this.request_close_workspace(window, cx),
                TabAction::CloseTab => this.request_close_tab(window, cx),
                TabAction::OpenDiff(request) => {
                    if let Err(error) = this.open_diff(*request, window, cx) {
                        window.push_notification(Notification::error(format!("{error:#}")), cx);
                    }
                }
            }
        })
        .detach();
        tab
    }

    pub(crate) fn active_tab(&self) -> Entity<Tab> {
        let workspace = &self.workspaces[self.active];
        workspace.tabs[workspace.active].clone()
    }

    pub(crate) fn active_root(&self) -> &Path {
        &self.workspaces[self.active].root
    }

    pub(crate) fn focus_active(&self, window: &mut Window, cx: &mut App) {
        self.active_tab()
            .update(cx, |tab, cx| tab.focus_active(window, cx));
    }

    pub(crate) fn focus_for_mode(&self, window: &mut Window, cx: &mut App) {
        self.active_tab()
            .update(cx, |tab, cx| tab.focus_for_mode(window, cx));
    }

    pub(crate) fn key(
        &mut self,
        key: &Keystroke,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.active_tab()
            .update(cx, |tab, cx| tab.key(key, window, cx))
    }

    pub(crate) fn open_finder(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab()
            .update(cx, |tab, cx| tab.open_finder(window, cx));
    }

    fn leave_tab(&self, cx: &mut App) {
        self.active_tab()
            .update(cx, |tab, cx| tab.set_visible(false, cx));
    }

    fn enter_tab(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.active_tab()
            .update(cx, |tab, cx| tab.set_visible(true, cx));
        cx.set_global(Mode::Normal);
        self.focus_active(window, cx);
        cx.notify();
    }

    pub(crate) fn open_workspace(
        &mut self,
        argument: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        let root = resolve_root(argument, &self.workspaces[self.active].root)?;
        self.leave_tab(cx);
        if let Some(index) = self
            .workspaces
            .iter()
            .position(|workspace| workspace.root == root)
        {
            self.active = index;
        } else {
            let tab = self.create_tab(root.clone(), window, cx);
            self.workspaces.push(Workspace {
                id: tab.entity_id(),
                root,
                tabs: vec![tab],
                active: 0,
            });
            self.active = self.workspaces.len() - 1;
        }
        self.enter_tab(window, cx);
        Ok(())
    }

    pub(crate) fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.leave_tab(cx);
        let tab = self.create_tab(self.workspaces[self.active].root.clone(), window, cx);
        let workspace = &mut self.workspaces[self.active];
        workspace.tabs.push(tab);
        workspace.active = workspace.tabs.len() - 1;
        self.enter_tab(window, cx);
    }

    pub(crate) fn open_diff(&mut self, request: DiffRequest, window: &mut Window, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let workspace = &self.workspaces[self.active];
        let owner = workspace.id;
        let origin = self.active_tab().entity_id();
        let root = workspace.root.clone();
        let key = (owner, request);
        anyhow::ensure!(!self.opening_diffs.contains(&key), "This diff is already loading");
        self.opening_diffs.push(key);
        window.push_notification(Notification::info("Loading diff…"), cx);
        let loading = cx.background_spawn(async move { crate::diff::load(&root, request) });
        cx.spawn_in(window, async move |this, cx| {
            let result = loading.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.opening_diffs.retain(|pending| *pending != key);
                let Some(index) = this.workspaces.iter().position(|workspace| workspace.id == owner
                    && workspace.tabs.iter().any(|tab| tab.entity_id() == origin)) else { return; };
                let snapshot = match result {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        window.push_notification(Notification::error(format!("Diff: {error:#}")).autohide(false), cx);
                        return;
                    }
                };
                // A slow PR fetch must not take focus from another tab or a dialog.
                let activate = this.active == index && this.active_tab().entity_id() == origin
                    && !cx.global::<Mode>().is_overlay();
                if activate { this.leave_tab(cx); }
                let tab = this.create_tab(this.workspaces[index].root.clone(), window, cx);
                tab.update(cx, |tab, cx| {
                    let session = cx.new(|_| crate::diff::DiffSession::new(snapshot));
                    let id = tab.register_item(OpenItem::Diff(session), cx);
                    tab.show_item(tab.panes[tab.active].entity_id(), id, window, cx);
                });
                let workspace = &mut this.workspaces[index];
                workspace.tabs.push(tab);
                let number = workspace.tabs.len();
                if activate {
                    workspace.active = number - 1;
                    this.enter_tab(window, cx);
                } else {
                    window.push_notification(Notification::info(format!("Diff ready in {} · Tab {number}", workspace.root.display())), cx);
                }
                cx.notify();
            });
        }).detach();
        Ok(())
    }

    fn open_picker(&self, kind: PickerKind, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = &self.workspaces[self.active];
        let candidates = match kind {
            PickerKind::Workspaces => self
                .workspaces
                .iter()
                .enumerate()
                .map(|(index, workspace)| Candidate {
                    id: workspace.id,
                    label: workspace.root.display().to_string(),
                    current: index == self.active,
                })
                .collect(),
            PickerKind::Tabs => workspace
                .tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| {
                    let tab = tab.read(cx);
                    let current = tab.panes[tab.active].read(cx).item_id(cx);
                    let label = tab
                        .items
                        .iter()
                        .find(|item| Some(item.id()) == current)
                        .map(|item| item.label(&tab.root, cx))
                        .unwrap_or_else(|| "Plain".into());
                    Candidate {
                        id: workspace.tabs[index].entity_id(),
                        label: format!("Tab {} — {label}", index + 1),
                        current: index == workspace.active,
                    }
                })
                .collect(),
        };
        cx.set_global(Mode::Switcher);
        picker::open(cx.entity().downgrade(), kind, candidates, window, cx);
    }

    pub(super) fn select(
        &mut self,
        kind: PickerKind,
        id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match kind {
            PickerKind::Workspaces => {
                let Some(index) = self
                    .workspaces
                    .iter()
                    .position(|workspace| workspace.id == id)
                else {
                    return;
                };
                self.leave_tab(cx);
                self.active = index;
            }
            PickerKind::Tabs => {
                let Some(index) = self.workspaces[self.active]
                    .tabs
                    .iter()
                    .position(|tab| tab.entity_id() == id)
                else {
                    return;
                };
                self.leave_tab(cx);
                self.workspaces[self.active].active = index;
            }
        }
        self.enter_tab(window, cx);
    }

    fn target_tabs(&self, target: CloseTarget) -> Vec<Entity<Tab>> {
        self.workspaces
            .iter()
            .filter(|workspace| match target {
                CloseTarget::Workspace(id) => workspace.id == id,
                _ => true,
            })
            .flat_map(|workspace| workspace.tabs.iter())
            .filter(|tab| match target {
                CloseTarget::Tab(id) => tab.entity_id() == id,
                _ => true,
            })
            .cloned()
            .collect()
    }

    fn check_close(&self, target: CloseTarget, cx: &App) -> anyhow::Result<()> {
        match target {
            CloseTarget::Workspace(_) if self.workspaces.len() == 1 => {
                anyhow::bail!("Cannot close the last workspace.")
            }
            CloseTarget::Tab(id)
                if self
                    .workspaces
                    .iter()
                    .any(|w| w.tabs.len() == 1 && w.tabs[0].entity_id() == id) =>
            {
                anyhow::bail!("Cannot close the last tab.")
            }
            _ => {}
        }
        if self
            .target_tabs(target)
            .iter()
            .any(|tab| tab.read(cx).buffers().any(|buffer| buffer.read(cx).saving))
        {
            anyhow::bail!("Saving files. Please wait before closing.");
        }
        Ok(())
    }

    pub(crate) fn request_close_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_target(CloseTarget::Tab(self.active_tab().entity_id()), window, cx);
    }

    pub(crate) fn request_close_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_target(
            CloseTarget::Workspace(self.workspaces[self.active].id),
            window,
            cx,
        );
    }

    pub(crate) fn request_close(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Err(error) = self.check_close(CloseTarget::Window, cx) {
            window.push_notification(Notification::info(error.to_string()), cx);
            return false;
        }
        if self
            .target_tabs(CloseTarget::Window)
            .iter()
            .all(|tab| tab.read(cx).buffers().all(|b| !b.read(cx).dirty()))
        {
            return true;
        }
        self.confirm_close(CloseTarget::Window, window, cx);
        false
    }

    fn confirm_close(&mut self, target: CloseTarget, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.check_close(target, cx) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        if *cx.global::<Mode>() == Mode::Confirm {
            return;
        }
        let title = match target {
            CloseTarget::Window => "Unsaved files",
            CloseTarget::Workspace(_) => "Close workspace",
            CloseTarget::Tab(_) => "Close tab",
        };
        let mut details = Vec::new();
        let targets = self.target_tabs(target);
        for workspace in &self.workspaces {
            for (index, tab) in workspace.tabs.iter().enumerate() {
                if !targets.contains(tab) {
                    continue;
                }
                let tab = tab.read(cx);
                details.push(format!("{} · Tab {}", workspace.root.display(), index + 1));
                for item in &tab.items {
                    match item {
                        OpenItem::File(buffer) if buffer.read(cx).dirty() => {
                            details.push(format!("Unsaved: {}", item.label(&workspace.root, cx)))
                        }
                        OpenItem::Chat(_) | OpenItem::Terminal(_) | OpenItem::Media(_) => {
                            details.push(format!("Will stop: {}", item.label(&workspace.root, cx)))
                        }
                        _ => {}
                    }
                }
            }
        }
        window.close_dialog(cx);
        cx.set_global(Mode::Confirm);
        let owner = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            dialog.centered().w(px(560.)).p_4().entrance(DialogEntrance::None).title(title)
                .on_ok(|_, _, _| false)
                .on_close(|_, _, cx| cx.set_global(Mode::Normal))
                .child(div().id("unsaved-files").role(Role::Group).aria_label(title)
                    .child("Close this work and discard any unsaved changes? Saved files will not be deleted.")
                    .child(div().id("close-details").max_h(px(320.)).overflow_y_scroll()
                        .children(details.iter().map(|detail| div().child(detail.clone()))))
                    .child(div().flex().gap_2().mt_3()
                        .child(Button::new("cancel-close").label("Keep editing").on_click(|_, window, cx| {
                            window.close_dialog(cx);
                            cx.set_global(Mode::Normal);
                        }))
                        .child(Button::new("discard-close").label("Discard and close").on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |this, cx| this.close_target(target, window, cx));
                        }))))
        });
    }

    fn close_target(
        &mut self,
        target: CloseTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(error) = self.check_close(target, cx) {
            window.push_notification(Notification::error(error.to_string()), cx);
            return;
        }
        if matches!(target, CloseTarget::Window) {
            window.remove_window();
            return;
        }
        self.leave_tab(cx);
        for tab in self.target_tabs(target) {
            tab.update(cx, |tab, cx| tab.shutdown(cx));
        }
        match target {
            CloseTarget::Workspace(id) => {
                self.workspaces.retain(|workspace| workspace.id != id);
                self.active = self.active.min(self.workspaces.len() - 1);
            }
            CloseTarget::Tab(id) => {
                for workspace in &mut self.workspaces {
                    workspace.tabs.retain(|tab| tab.entity_id() != id);
                    workspace.active = workspace.active.min(workspace.tabs.len() - 1);
                }
            }
            CloseTarget::Window => unreachable!(),
        }
        window.close_dialog(cx);
        self.enter_tab(window, cx);
    }
}

fn resolve_root(argument: &str, current: &Path) -> anyhow::Result<PathBuf> {
    let argument = argument.trim();
    anyhow::ensure!(!argument.is_empty(), "Usage: wsnew <directory>");
    let argument = argument
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .unwrap_or(argument);
    anyhow::ensure!(!argument.is_empty(), "Usage: wsnew <directory>");
    let path = if argument == "~" || argument.starts_with("~/") {
        PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME is not set"))?)
            .join(argument.strip_prefix("~/").unwrap_or(""))
    } else {
        current.join(argument)
    };
    let path = path.canonicalize()?;
    anyhow::ensure!(path.is_dir(), "Not a directory: {}", path.display());
    Ok(path)
}

impl Render for WorkspaceWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = &self.workspaces[self.active];
        let label = format!(
            "{} · Tab {}",
            workspace.root.display(),
            workspace.active + 1
        );
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .id("workspace-location")
                    .role(Role::Group)
                    .aria_label(label.clone())
                    .h(px(24.))
                    .flex_shrink_0()
                    .px_3()
                    .text_sm()
                    .flex()
                    .items_center()
                    .gap_2()
                    .overflow_hidden()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(workspace.root.display().to_string()),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .child(format!("Tab {}", workspace.active + 1)),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.active_tab()))
    }
}
