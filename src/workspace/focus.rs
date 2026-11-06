//! Spatial focus navigation across panes and their independently focusable regions.
use super::Tab;
use gpui_kit::{
    App, Bounds, Context, EntityId, Focusable as _, Pixels, Window,
    base::Placement,
    component::dock::{DockPlacement, PaneRef, PanelId},
    px,
};

use crate::mode::Mode;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum FocusTarget {
    FileTree,
    Pane(EntityId),
    DiffTree(EntityId),
    DiffBody(EntityId),
}

struct FocusRegion {
    target: FocusTarget,
    bounds: Bounds<Pixels>,
}

fn adjacent_target(
    regions: &[FocusRegion],
    from: FocusTarget,
    direction: Placement,
) -> Option<FocusTarget> {
    let source = regions.iter().find(|region| region.target == from)?.bounds;
    regions
        .iter()
        .filter(|region| region.target != from)
        .filter_map(|region| {
            let to = region.bounds;
            let (gap, overlap, distance) = match direction {
                Placement::Left => (
                    source.left() - to.right(),
                    source.bottom().min(to.bottom()) - source.top().max(to.top()),
                    (source.center().y - to.center().y).abs(),
                ),
                Placement::Right => (
                    to.left() - source.right(),
                    source.bottom().min(to.bottom()) - source.top().max(to.top()),
                    (source.center().y - to.center().y).abs(),
                ),
                Placement::Top => (
                    source.top() - to.bottom(),
                    source.right().min(to.right()) - source.left().max(to.left()),
                    (source.center().x - to.center().x).abs(),
                ),
                Placement::Bottom => (
                    to.top() - source.bottom(),
                    source.right().min(to.right()) - source.left().max(to.left()),
                    (source.center().x - to.center().x).abs(),
                ),
            };
            // Allow subpixel rounding at a shared edge, but never diagonal jumps.
            (f32::from(gap) >= -0.5 && overlap > px(0.)).then_some((
                region.target,
                f32::from(gap),
                f32::from(distance),
            ))
        })
        .min_by(|a, b| a.1.total_cmp(&b.1).then_with(|| a.2.total_cmp(&b.2)))
        .map(|(target, _, _)| target)
}

impl Tab {
    fn focus_regions(&self, cx: &App) -> Vec<FocusRegion> {
        let mut regions = Vec::new();
        if !self.visible {
            return regions;
        }
        if self.tree_visible
            && let Some(tree) = &self.tree
        {
            regions.push(FocusRegion {
                target: FocusTarget::FileTree,
                bounds: tree.read(cx).bounds.get(),
            });
        }
        let dock = self.dock.read(cx);
        if let Some(layout) = dock.layout(DockPlacement::Center) {
            for pane in &self.panes {
                let id = pane.entity_id();
                let panel = PanelId::from(id);
                let Some(node) = layout
                    .find_panel_node(panel)
                    .and_then(|node| layout.find_node(node))
                else {
                    continue;
                };
                if dock
                    .zoomed_group()
                    .is_some_and(|zoomed| zoomed != node.id())
                {
                    continue;
                }
                let PaneRef::Tabs { panels, active_ix } = node.kind() else {
                    continue;
                };
                // Cached background tabs retain their bounds, but are not destinations.
                if panels.get(active_ix) != Some(&panel) {
                    continue;
                }
                let pane = pane.read(cx);
                if let Some(diff) = pane.diff() {
                    let diff = diff.read(cx);
                    regions.extend([
                        FocusRegion {
                            target: FocusTarget::DiffTree(id),
                            bounds: diff.tree_bounds.get(),
                        },
                        FocusRegion {
                            target: FocusTarget::DiffBody(id),
                            bounds: diff.body_bounds.get(),
                        },
                    ]);
                } else {
                    regions.push(FocusRegion {
                        target: FocusTarget::Pane(id),
                        bounds: pane.bounds.get(),
                    });
                }
            }
        }
        regions.retain(|region| {
            region.bounds.size.width > px(0.) && region.bounds.size.height > px(0.)
        });
        regions
    }

    pub(super) fn focused_target(&self, window: &Window, cx: &App) -> Option<FocusTarget> {
        if !self.visible {
            return None;
        }
        if self.tree_visible
            && let Some(tree) = &self.tree
            && tree.focus_handle(cx).contains_focused(window, cx)
        {
            return Some(FocusTarget::FileTree);
        }
        self.panes.iter().find_map(|pane| {
            if !pane.focus_handle(cx).contains_focused(window, cx) {
                return None;
            }
            let id = pane.entity_id();
            Some(if let Some(diff) = pane.read(cx).diff() {
                if diff.read(cx).body_focus.contains_focused(window, cx) {
                    FocusTarget::DiffBody(id)
                } else {
                    FocusTarget::DiffTree(id)
                }
            } else {
                FocusTarget::Pane(id)
            })
        })
    }

    fn focus_target(&mut self, target: FocusTarget, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            FocusTarget::FileTree => {
                if self.tree_visible
                    && let Some(tree) = &self.tree
                {
                    tree.update(cx, |tree, cx| tree.focus(window, cx));
                }
            }
            FocusTarget::Pane(id) | FocusTarget::DiffTree(id) | FocusTarget::DiffBody(id) => {
                let Some(index) = self.panes.iter().position(|pane| pane.entity_id() == id) else {
                    return;
                };
                self.active = index;
                if *cx.global::<Mode>() != Mode::Normal {
                    cx.set_global(Mode::Normal);
                }
                self.reveal_active_item(cx);
                match target {
                    FocusTarget::DiffTree(_) | FocusTarget::DiffBody(_) => {
                        if let Some(diff) = self.panes[index].read(cx).diff() {
                            diff.update(cx, |diff, cx| match target {
                                FocusTarget::DiffTree(_) => diff.focus_tree(window, cx),
                                _ => diff.focus_body(window, cx),
                            });
                        }
                    }
                    _ => self.focus_active(window, cx),
                }
            }
        }
        self.pending.clear();
        cx.notify();
    }

    pub(super) fn move_focus(
        &mut self,
        direction: Placement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(from) = self.focused_target(window, cx) else {
            return;
        };
        if let Some(target) = adjacent_target(&self.focus_regions(cx), from, direction) {
            self.focus_target(target, window, cx);
        }
    }

    /// Preserve a tree/body focus change even if its pane activation event is still queued.
    pub(crate) fn focus_for_mode(&self, window: &mut Window, cx: &mut App) {
        if *cx.global::<Mode>() == Mode::Normal
            && matches!(
                self.focused_target(window, cx),
                Some(FocusTarget::FileTree | FocusTarget::DiffTree(_) | FocusTarget::DiffBody(_))
            )
        {
            return;
        }
        self.focus_active(window, cx);
    }

    pub(super) fn focused_bounds(&self, window: &Window, cx: &App) -> Bounds<Pixels> {
        self.focused_target(window, cx)
            .and_then(|target| {
                self.focus_regions(cx)
                    .into_iter()
                    .find(|region| region.target == target)
            })
            .map(|region| region.bounds)
            // Keep the active pane outlined while an overlay temporarily owns focus.
            .unwrap_or_else(|| self.panes[self.active].read(cx).bounds.get())
    }
}
