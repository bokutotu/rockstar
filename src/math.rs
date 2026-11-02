use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, LazyLock, Mutex},
};

use gpui_kit::{
    AnyElement, App, Entity, Image, ImageFormat, IntoElement, ObjectFit, ParentElement as _,
    Styled as _, StyledImage as _, WeakEntity, Window,
    base::{
        InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
        TextViewState, markdown_ast::Node,
    },
    div, img, px, rgb,
};
use latex_rust::{Color, Dim, MathFont, MathStyle, SvgOptions};

static MATH_FONT: LazyLock<Result<MathFont, String>> =
    LazyLock::new(|| MathFont::stix_two_math().map_err(|error| error.to_string()));

#[derive(Clone, Copy)]
enum MathMode {
    Inline,
    Block,
}

#[derive(Clone)]
struct MathImage {
    image: Arc<Image>,
    width: f32,
    height: f32,
    baseline: f32,
}

#[derive(Clone)]
enum RenderState {
    Pending,
    Ready(MathImage),
    Failed(String),
}

#[derive(Default)]
struct RenderCache {
    entries: HashMap<String, RenderState>,
    queue: VecDeque<String>,
    active: bool,
}

#[derive(Clone)]
pub struct MathPlugin {
    mode: MathMode,
    cache: Arc<Mutex<RenderCache>>,
    document: WeakEntity<TextViewState>,
}

impl MathPlugin {
    pub fn inline(document: &Entity<TextViewState>) -> Self {
        Self::new(MathMode::Inline, document)
    }

    pub fn block(document: &Entity<TextViewState>) -> Self {
        Self::new(MathMode::Block, document)
    }

    fn new(mode: MathMode, document: &Entity<TextViewState>) -> Self {
        Self {
            mode,
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
        let mode = self.mode;
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
                        .spawn(async move { render_math(&input, mode) })
                        .await;
                    let state = match result {
                        Ok(image) => RenderState::Ready(image),
                        Err(error) => RenderState::Failed(format!("latex-rust: {error}")),
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

impl MarkdownPlugin for MathPlugin {
    fn name(&self) -> &str {
        match self.mode {
            MathMode::Inline => "rockstar-inline-math",
            MathMode::Block => "rockstar-block-math",
        }
    }

    fn is_block(&self) -> bool {
        matches!(self.mode, MathMode::Block)
    }

    fn parse(&self, node: &Node, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let source = match (self.mode, node) {
            (MathMode::Inline, Node::InlineMath(math)) => &math.value,
            (MathMode::Block, Node::Math(math)) => &math.value,
            _ => return None,
        };
        let original = cx.node_source(node).unwrap_or(source).to_string();
        Some(
            MarkdownNode::new(self.name(), source.clone())
                .text(original.clone())
                .markdown(original),
        )
    }

    fn render(&self, node: &MarkdownNode, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let source = node.data::<String>().expect("math source");
        match self.request_render(source, cx) {
            RenderState::Ready(math) => {
                let original = node.as_text().to_string();
                div()
                    .w_full()
                    .flex()
                    .justify_center()
                    .py_3()
                    .child(
                        img(math.image)
                            .w_full()
                            .max_w(px(math.width))
                            .aspect_ratio(math.width / math.height)
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

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let source = node.data::<String>()?;
        match self.request_render(source, cx) {
            RenderState::Ready(math) => {
                let scale = f32::from(context.font_size()) / 16.0;
                let original = node.as_text().to_string();
                Some(
                    InlineElement::new(
                        img(math.image)
                            .w(px(math.width * scale))
                            .h(px(math.height * scale))
                            .object_fit(ObjectFit::Contain)
                            .with_fallback(move || {
                                fallback(&original, Some("image decoding failed"))
                            }),
                    )
                    .with_baseline(px(math.baseline * scale)),
                )
            }
            RenderState::Pending => None,
            RenderState::Failed(error) => {
                Some(InlineElement::new(fallback(node.as_text(), Some(&error))))
            }
        }
    }
}

fn render_math(source: &str, mode: MathMode) -> Result<MathImage, String> {
    let font = MATH_FONT.as_ref().map_err(Clone::clone)?;
    let options = SvgOptions {
        color: Color::rgb(0x18, 0x18, 0x1b),
        display: matches!(mode, MathMode::Block),
        ..Default::default()
    };
    let style = match mode {
        MathMode::Inline => MathStyle::Text,
        MathMode::Block => MathStyle::Display,
    };
    let ast = latex_rust::parse(source).map_err(|error| error.to_string())?;
    let layout = latex_rust::layout_with_em_size_pt(&ast, font, style, &options.font_size_pt)
        .map_err(|error| error.to_string())?;
    // SVG uses points; GPUI uses logical pixels at 96 dpi (12 pt = 16 px).
    let em_px = &options.font_size_pt * &Dim::ratio(4, 3);
    let width = f32::from_bits((&layout.width * &em_px).to_ieee32_bits());
    let height = f32::from_bits(((&layout.height + &layout.depth) * &em_px).to_ieee32_bits());
    let baseline = f32::from_bits((&layout.height * &em_px).to_ieee32_bits());
    if !width.is_finite()
        || !height.is_finite()
        || !baseline.is_finite()
        || width <= 0.0
        || height <= 0.0
    {
        return Err("invalid image dimensions".into());
    }
    let svg = latex_rust::render_svg(&layout, font, &options).map_err(|error| error.to_string())?;
    Ok(MathImage {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width,
        height,
        baseline: baseline.clamp(0.0, height),
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
