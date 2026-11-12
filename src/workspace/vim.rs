use super::*;

impl Tab {
    pub(super) fn vim_workspace_command(
        &mut self,
        command: &str,
        count: usize,
        explicit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(command, "ctrl-^" | "ctrl-6") {
            let pane = self.panes[self.active].clone();
            if let Some(item) = pane.read(cx).alternate_file {
                self.show_item(pane.entity_id(), item, window, cx);
            } else {
                window.push_notification(Notification::error("No alternate file"), cx);
            }
            return;
        }
        if matches!(command, "gf" | "gF") {
            let Some(file) = self.panes[self.active].read(cx).file() else {
                return;
            };
            let target = file.read(cx).file_name_at_cursor(cx);
            if let Some((path, line)) = target {
                file.update(cx, |file, cx| file.remember_jump(cx));
                self.open_file_at(
                    self.panes[self.active].entity_id(),
                    path,
                    (command == "gF").then(|| {
                        (
                            lsp_types::Position::new(line.unwrap_or(0) as u32, 0),
                            crate::lsp::language::Encoding::Utf8,
                            false,
                        )
                    }),
                    true,
                    window,
                    cx,
                );
            } else {
                window.push_notification(Notification::error("No file name under cursor"), cx);
            }
            return;
        }
        let key = command
            .strip_prefix("ctrl-w")
            .unwrap_or(command)
            .trim_start_matches("ctrl-");
        match key {
            "s" | "v" => {
                let source = self.panes[self.active].clone();
                let item = source.read(cx).item_id(cx);
                let location = source
                    .read(cx)
                    .file()
                    .map(|file| file.read(cx).location(cx));
                let jumps = source.read(cx).jumps.read(cx).clone();
                self.split(
                    if key == "s" {
                        Placement::Bottom
                    } else {
                        Placement::Right
                    },
                    window,
                    cx,
                );
                let target_jumps = self.panes[self.active].read(cx).jumps.clone();
                target_jumps.update(cx, |new, _| *new = jumps);
                if let Some(item) = item {
                    self.show_item(self.panes[self.active].entity_id(), item, window, cx);
                }
                if let Some(location) = location
                    && let Some(file) = self.panes[self.active].read(cx).file()
                {
                    file.update(cx, |file, cx| {
                        file.jump_to(
                            lsp_types::Position::new(location.line as u32, location.column as u32),
                            crate::lsp::language::Encoding::Utf8,
                            false,
                            true,
                            window,
                            cx,
                        )
                    });
                }
            }
            "+" | "-" | ">" | "<" | "=" | "_" | "|" => {
                self.resize_vim(key, count, explicit, window, cx)
            }
            "r" | "R" | "x" => self.reorder_vim(key, count, explicit, window, cx),
            "H" | "J" | "K" | "L" => {
                let root = self
                    .dock
                    .read(cx)
                    .layout(DockPlacement::Center)
                    .map(|tree| tree.root().id());
                if let Some(node) = root {
                    let placement = match key {
                        "H" => Placement::Left,
                        "J" => Placement::Bottom,
                        "K" => Placement::Top,
                        _ => Placement::Right,
                    };
                    let panel = PanelId::from(self.panes[self.active].entity_id());
                    self.dock.update(cx, |dock, cx| {
                        dock.move_panel(
                            panel,
                            InsertTarget::Split {
                                node,
                                placement,
                                size: None,
                            },
                            window,
                            cx,
                        )
                    });
                    self.equalize_splits(None, window, cx);
                    self.focus_active(window, cx);
                }
            }
            "h" | "left" => self.move_focus(Placement::Left, window, cx),
            "j" | "down" => self.move_focus(Placement::Bottom, window, cx),
            "k" | "up" => self.move_focus(Placement::Top, window, cx),
            "l" | "right" => self.move_focus(Placement::Right, window, cx),
            "w" | "W" | "p" | "t" | "b" => {
                let previous = self.panes[self.active].entity_id();
                self.active = match key {
                    "p" => self
                        .previous_pane
                        .and_then(|id| self.panes.iter().position(|pane| pane.entity_id() == id))
                        .unwrap_or(self.active),
                    "w" | "W" if explicit => count.saturating_sub(1).min(self.panes.len() - 1),
                    "t" => 0,
                    "b" => self.panes.len() - 1,
                    "W" => {
                        (self.active + self.panes.len() - count % self.panes.len())
                            % self.panes.len()
                    }
                    _ => (self.active + count) % self.panes.len(),
                };
                if self.panes[self.active].entity_id() != previous {
                    self.previous_pane = Some(previous);
                }
                self.focus_active(window, cx);
            }
            "c" | "q" => {
                if let Err(error) = self.close_active_pane(window, cx) {
                    window.push_notification(Notification::error(error.to_string()), cx);
                }
            }
            "o" => {
                let keep = self.panes[self.active].entity_id();
                while self.panes.len() > 1 {
                    self.active = self
                        .panes
                        .iter()
                        .position(|pane| pane.entity_id() != keep)
                        .unwrap();
                    let _ = self.close_active_pane(window, cx);
                }
                self.active = 0;
                self.focus_active(window, cx);
            }
            _ => {
                window.push_notification(
                    Notification::error(format!("Window command not supported: Ctrl-w {key}")),
                    cx,
                );
            }
        }
        cx.notify();
    }
    fn reorder_vim(
        &mut self,
        key: &str,
        count: usize,
        explicit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tree) = self.dock.read(cx).layout(DockPlacement::Center).cloned() else {
            return;
        };
        let active = PanelId::from(self.panes[self.active].entity_id());
        let mut group = Vec::new();
        tree.root().walk(&mut |node| {
            if let PaneRef::Split { children, .. } = node.kind() {
                let panels: Option<Vec<_>> = children
                    .iter()
                    .map(|child| match child.kind() {
                        PaneRef::Tabs { panels, .. } if panels.len() == 1 => Some(panels[0]),
                        _ => None,
                    })
                    .collect();
                if let Some(panels) = panels.filter(|p| p.len() > 1 && p.contains(&active)) {
                    group = panels;
                }
            }
        });
        if group.is_empty() {
            window.push_notification(
                Notification::error("Cannot exchange nested split groups"),
                cx,
            );
            return;
        }
        let mut order = group.clone();
        if key == "x" {
            let index = group.iter().position(|p| *p == active).unwrap();
            let other = if explicit {
                count.saturating_sub(1).min(order.len() - 1)
            } else if index + 1 < order.len() {
                index + 1
            } else {
                index - 1
            };
            order.swap(index, other);
        } else {
            let amount = count % order.len();
            if key == "r" {
                order.rotate_left(amount);
            } else {
                order.rotate_right(amount);
            }
        }
        let replacements: std::collections::HashMap<_, _> = group.into_iter().zip(order).collect();
        fn layout(
            node: &PaneNode,
            replacements: &std::collections::HashMap<PanelId, PanelId>,
            panes: &[Entity<Pane>],
        ) -> Option<DockLayout> {
            match node.kind() {
                PaneRef::Tabs { panels, active_ix } => {
                    let mut result = DockLayout::tabs().active_index(active_ix);
                    for panel in panels {
                        let target = replacements.get(panel).unwrap_or(panel);
                        result = result.panel(
                            panes
                                .iter()
                                .find(|pane| PanelId::from(pane.entity_id()) == *target)?
                                .clone(),
                        );
                    }
                    Some(result)
                }
                PaneRef::Split {
                    axis,
                    children,
                    sizes,
                } => {
                    let mut result = if axis == gpui_kit::Axis::Horizontal {
                        DockLayout::h_split()
                    } else {
                        DockLayout::v_split()
                    };
                    for (child, size) in children.iter().zip(sizes) {
                        result = result.child(layout(child, replacements, panes)?, *size);
                    }
                    Some(result)
                }
            }
        }
        if let Some(layout) = layout(tree.root(), &replacements, &self.panes) {
            self.dock
                .update(cx, |dock, cx| dock.set_center(layout, window, cx));
        }
        self.focus_active(window, cx);
    }
    fn resize_vim(
        &mut self,
        key: &str,
        count: usize,
        explicit: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if key == "=" {
            self.equalize_splits(None, window, cx);
            return;
        }
        let Some(tree) = self.dock.read(cx).layout(DockPlacement::Center).cloned() else {
            return;
        };
        let panel = PanelId::from(self.panes[self.active].entity_id());
        let axis = if matches!(key, ">" | "<" | "|") {
            Axis::Horizontal
        } else {
            Axis::Vertical
        };
        let has_panel = |node: &PaneNode| {
            let mut found = false;
            node.walk(&mut |node| {
                if let PaneRef::Tabs { panels, .. } = node.kind() {
                    found |= panels.contains(&panel);
                }
            });
            found
        };
        let size = |node: &PaneNode, axis: Axis| {
            let mut start: Option<f32> = None;
            let mut end: Option<f32> = None;
            node.walk(&mut |node| {
                if let PaneRef::Tabs { panels, .. } = node.kind() {
                    for pane in &self.panes {
                        if !panels.contains(&PanelId::from(pane.entity_id())) {
                            continue;
                        }
                        let bounds = pane.read(cx).bounds.get();
                        let (a, b) = if axis == Axis::Horizontal {
                            (bounds.left(), bounds.right())
                        } else {
                            (bounds.top(), bounds.bottom())
                        };
                        start = Some(start.map_or(f32::from(a), |old| old.min(f32::from(a))));
                        end = Some(end.map_or(f32::from(b), |old| old.max(f32::from(b))));
                    }
                }
            });
            (end.unwrap_or(240.) - start.unwrap_or(0.)).max(48.)
        };
        let mut target_id = None;
        tree.root().walk(&mut |node| {
            if let PaneRef::Split {
                axis: split_axis,
                children,
                ..
            } = node.kind()
                && children.len() > 1
                && split_axis == axis
                && has_panel(node)
            {
                target_id = Some(node.id());
            }
        });
        let Some(id) = target_id else {
            return;
        };
        let Some(node) = tree.find_node(id) else {
            return;
        };
        let PaneRef::Split { axis, children, .. } = node.kind() else {
            return;
        };
        let mut sizes: Vec<_> = children.iter().map(|child| size(child, axis)).collect();
        let total: f32 = sizes.iter().sum();
        if let Some(index) = children.iter().position(&has_panel) {
            let unit = f32::from(cx.theme().mono_font_size)
                * if axis == Axis::Horizontal { 0.6 } else { 1.5 };
            let desired = match key {
                "|" | "_" if !explicit => total,
                "|" | "_" => count as f32 * unit,
                "+" | ">" => sizes[index] + count as f32 * unit,
                _ => sizes[index] - count as f32 * unit,
            }
            .clamp(48., (total - 48. * (sizes.len() - 1) as f32).max(48.));
            let rest = total - sizes[index];
            for (i, size) in sizes.iter_mut().enumerate() {
                if i != index {
                    *size = if rest > 0. {
                        *size / rest * (total - desired)
                    } else {
                        48.
                    };
                }
            }
            sizes[index] = desired;
        }
        let sizes = sizes.into_iter().map(px).collect();
        self.dock
            .update(cx, |dock, cx| dock.set_split_sizes(id, sizes, window, cx));
    }
}
