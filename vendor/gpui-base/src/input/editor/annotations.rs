//! Display-only source annotations. They never enter the rope or undo history.
use crate::input::{EditorState, RopeExt};
use gpui::{Context, SharedString};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnnotationPlacement {
    Inline,
    AboveLine,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorAnnotation {
    /// UTF-8 byte offset in the document. AboveLine annotations use its containing line.
    pub offset: usize,
    pub label: SharedString,
    pub placement: AnnotationPlacement,
    /// Returned by InputEvent::AnnotationClick; the host owns the action.
    pub action: Option<usize>,
}
impl EditorState {
    pub fn set_annotations(
        &mut self,
        mut annotations: Vec<EditorAnnotation>,
        cx: &mut Context<Self>,
    ) {
        annotations.retain(|a| {
            a.offset <= self.text.len()
                && self.text.is_char_boundary(a.offset)
                && !a.label.contains('\n')
        });
        annotations.sort_by_key(|a| a.offset);
        if self.extras.annotations == annotations {
            return;
        }
        let rows = annotations
            .iter()
            .filter(|a| a.placement == AnnotationPlacement::AboveLine)
            .map(|a| self.text.offset_to_point(a.offset).row)
            .collect();
        self.extras.annotations = annotations;
        self.display_map.set_annotation_rows(rows);
        cx.notify();
    }
}
