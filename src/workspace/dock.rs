//! Keep the standard dock appearance while measuring the entire chat frame,
//! including its title bar, for the workspace's active-pane outline.
use std::{rc::Rc, sync::Arc};

use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Axis, Div, Entity, Stateful, Styled as _, Window,
    base::{ElementExt as _, ResizeHandleContext},
    component::dock::{
        BasePanelView, DockArea, DockAreaRenderer, DockContext, DockSkin, DropIndicator, NodeId,
        PanelState, TabGroupContext, TabGroupRenderer,
    },
};

use crate::chat::ChatPane;

pub(super) fn new(window: &mut Window, cx: &mut App) -> Entity<DockArea> {
    cx.new(|cx| {
        let skin = Rc::new(WorkspaceDockSkin(DockSkin::new(cx)));
        DockArea::new("workspace", None, window, cx).with_renderer(skin)
    })
}

struct WorkspaceDockSkin(Rc<DockSkin>);

impl DockAreaRenderer for WorkspaceDockSkin {
    fn frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.0.frame(window, cx)
    }

    fn center_frame(&self, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        self.0.center_frame(window, cx)
    }

    fn split_frame(
        &self,
        node: NodeId,
        axis: Axis,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.0.split_frame(node, axis, window, cx)
    }

    fn render_split_handle(
        &self,
        handle: &ResizeHandleContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.0.render_split_handle(handle, window, cx)
    }

    fn render_dock(
        &self,
        dock: &DockContext,
        content: AnyElement,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.0.render_dock(dock, content, window, cx)
    }

    fn build_placeholder(
        &self,
        state: &PanelState,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Arc<dyn BasePanelView>> {
        self.0.build_placeholder(state, window, cx)
    }

    fn tab_group_renderer(&self) -> Rc<dyn TabGroupRenderer> {
        Rc::new(ChatFrameSkin(self.0.tab_group_renderer()))
    }
}

struct ChatFrameSkin(Rc<dyn TabGroupRenderer>);

impl TabGroupRenderer for ChatFrameSkin {
    fn frame(&self, group: &TabGroupContext, window: &mut Window, cx: &mut App) -> Stateful<Div> {
        let frame = self.0.frame(group, window, cx);
        let pane = group
            .active_panel()
            .and_then(|panel| panel.view().downcast::<ChatPane>().ok());
        if let Some(pane) = pane {
            let bounds = pane.read(cx).bounds.clone();
            frame
                .relative()
                .on_prepaint(move |rect, _, _| bounds.set(rect))
        } else {
            frame
        }
    }

    fn content_frame(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Stateful<Div> {
        self.0.content_frame(group, window, cx)
    }

    fn render_tab_bar(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.0.render_tab_bar(group, window, cx)
    }

    fn render_active_panel(
        &self,
        panel: AnyView,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        self.0.render_active_panel(panel, group, window, cx)
    }

    fn render_drop_indicator(
        &self,
        indicator: DropIndicator,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.0.render_drop_indicator(indicator, window, cx)
    }

    fn render_empty(
        &self,
        group: &TabGroupContext,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<AnyElement> {
        self.0.render_empty(group, window, cx)
    }
}
