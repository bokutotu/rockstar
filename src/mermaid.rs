use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};

use gpui_kit::{
    AnyElement, App, Entity, Image, ImageFormat, IntoElement, ObjectFit, ParentElement as _,
    Styled as _, StyledImage as _, WeakEntity, Window,
    base::{MarkdownNode, MarkdownParseContext, MarkdownPlugin, TextViewState, markdown_ast::Node},
    div, img, px, rgb,
};
use merman::{
    Engine, MermaidConfig, OperationControl, RenderOutput, RenderRequest, Renderer, SvgRequest,
    svg::{SvgPipeline, SvgRenderOptions},
};

const TIMEOUT: Duration = Duration::from_secs(30);

static MERMAID_RENDERER: LazyLock<Renderer> = LazyLock::new(|| {
    let config = MermaidConfig::from_value(serde_json::json!({
        "securityLevel": "strict",
        "theme": "default",
        "fontFamily": "Noto Sans CJK JP",
        "htmlLabels": false,
    }));
    Renderer::new()
        .with_engine(Engine::new().with_site_config(config))
        .with_parse_options(merman::ParseOptions::strict())
});

#[derive(Clone)]
struct DiagramImage {
    image: Arc<Image>,
    width: f32,
    height: f32,
}

#[derive(Clone)]
enum RenderState {
    Pending,
    Ready(DiagramImage),
    Failed(String),
}

#[derive(Default)]
struct RenderCache {
    entries: HashMap<String, RenderState>,
    queue: VecDeque<String>,
    active: bool,
}

#[derive(Clone)]
pub struct MermaidPlugin {
    cache: Arc<Mutex<RenderCache>>,
    document: WeakEntity<TextViewState>,
}

impl MermaidPlugin {
    pub fn new(document: &Entity<TextViewState>) -> Self {
        Self {
            cache: Arc::default(),
            document: document.downgrade(),
        }
    }

    fn request_render(&self, source: &str, cx: &mut App) -> RenderState {
        {
            let mut cache = self.cache.lock().unwrap();
            if let Some(state) = cache.entries.get(source) {
                return state.clone();
            }
            cache
                .entries
                .insert(source.to_string(), RenderState::Pending);
            cache.queue.push_back(source.to_string());
            if cache.active {
                return RenderState::Pending;
            }
            cache.active = true;
        }

        let cache = Arc::downgrade(&self.cache);
        let document = self.document.clone();
        // Start outside layout, with one background conversion at a time per plugin.
        cx.defer(move |cx| {
            cx.spawn(async move |cx| {
                loop {
                    if document.upgrade().is_none() {
                        return;
                    }
                    let source = {
                        let Some(cache) = cache.upgrade() else { return };
                        let mut cache = cache.lock().unwrap();
                        let Some(source) = cache.queue.pop_front() else {
                            cache.active = false;
                            return;
                        };
                        source
                    };
                    let input = source.clone();
                    let result = cx
                        .background_executor()
                        .spawn(async move { render_diagram(&input) })
                        .await;
                    let state = match result {
                        Ok(image) => RenderState::Ready(image),
                        Err(error) => RenderState::Failed(format!("merman: {error}")),
                    };
                    let Some(cache) = cache.upgrade() else { return };
                    cache.lock().unwrap().entries.insert(source, state);
                    if document
                        .update(cx, |document, cx| document.invalidate_inline_layout(cx))
                        .is_err()
                    {
                        return;
                    }
                }
            })
            .detach();
        });
        RenderState::Pending
    }
}

impl MarkdownPlugin for MermaidPlugin {
    fn name(&self) -> &str {
        "rockstar-mermaid"
    }

    fn is_block(&self) -> bool {
        true
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Code(code) = node else { return None };
        if !code
            .lang
            .as_deref()
            .is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid"))
        {
            return None;
        }
        let original = cx.node_source(node).unwrap_or(&code.value).to_string();
        Some(
            MarkdownNode::new(self.name(), code.value.clone())
                .text(original.clone())
                .markdown(original),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let source = node.data::<String>().expect("Mermaid source");
        match self.request_render(source, cx) {
            RenderState::Ready(diagram) => {
                let original = node.as_text().to_string();
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .py_3()
                    .child(
                        img(diagram.image)
                            .w_full()
                            .max_w(px(diagram.width))
                            .aspect_ratio(diagram.width / diagram.height)
                            .object_fit(ObjectFit::Contain)
                            .with_fallback(move || {
                                fallback(&original, Some("image decoding failed"))
                            }),
                    )
                    .into_any_element()
            }
            RenderState::Pending => fallback(node.as_text(), None),
            RenderState::Failed(error) => fallback(node.as_text(), Some(&error)),
        }
    }
}

fn render_diagram(source: &str) -> Result<DiagramImage, String> {
    let request = SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        options: SvgRenderOptions {
            diagram_id: Some("rockstar".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let output = MERMAID_RENDERER
        .render(RenderRequest::svg(
            source,
            OperationControl::new().with_deadline(TIMEOUT),
            request,
        ))
        .map_err(|error| error.to_string())?;
    let RenderOutput::Svg(Some(svg)) = output else {
        return Err("no Mermaid diagram detected".into());
    };
    decode_diagram(svg.into_parts().0)
}

fn decode_diagram(svg: String) -> Result<DiagramImage, String> {
    let invalid = || "invalid SVG dimensions".to_string();
    let document = roxmltree::Document::parse(&svg).map_err(|error| error.to_string())?;
    let root = document.root_element();
    if !root.has_tag_name(("http://www.w3.org/2000/svg", "svg")) {
        return Err("invalid SVG root".into());
    }
    let coordinates: Option<Vec<f32>> = root
        .attribute("viewBox")
        .ok_or_else(invalid)?
        .split(|ch: char| ch.is_ascii_whitespace() || ch == ',')
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().ok().filter(|value: &f32| value.is_finite()))
        .collect();
    let coordinates = coordinates.ok_or_else(invalid)?;
    let [_, _, width, height] = coordinates.as_slice() else {
        return Err(invalid());
    };
    let (width, height) = (*width, *height);
    if width <= 0.0 || height <= 0.0 {
        return Err(invalid());
    }
    Ok(DiagramImage {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width,
        height,
    })
}

fn fallback(source: &str, error: Option<&str>) -> AnyElement {
    let mut element = div()
        .font_family("DejaVu Sans Mono")
        .text_size(px(14.0))
        .child(source.to_string());
    if let Some(error) = error {
        element = element.child(div().text_color(rgb(0xb42318)).child(error.to_string()));
    }
    element.into_any_element()
}
