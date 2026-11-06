//! Media extensions of the existing Markdown AST, not a text substitution.
//! Fenced/inline code and escaped HTML never reach this parser as HTML nodes.
use super::{LoadState, MediaKind, MediaSession, MediaSource, MediaView, resolve_source};
use gpui_kit::{
    App, AppContext as _, Entity, Image, ImageCacheError, ImageFormat, ImageSource, IntoElement,
    ParentElement as _, SharedUri, Styled as _, WeakEntity, Window,
    base::{
        MarkdownNode, MarkdownParseContext, MarkdownPlugin, TextView, TextViewState,
        markdown_ast::Node,
    },
    div, px,
};
use html5ever::{
    parse_document,
    serialize::{SerializeOpts, TraversalScope, serialize},
    tendril::TendrilSink,
};
use markup5ever_rcdom::{Handle, NodeData, RcDom, SerializableHandle};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    start: usize,
    index: usize,
    kind: MediaKind,
    src: String,
}
#[derive(Clone, Debug)]
enum Part {
    Html(String),
    Media(MediaKind, String),
}
#[derive(Default)]
struct Cache {
    views: HashMap<Key, Entity<MediaView>>,
    images: HashMap<String, Entity<MediaSession>>,
    data_images: HashMap<String, Arc<Image>>,
    visible: bool,
    closed: bool,
    suppressed: HashSet<Key>,
}

#[derive(Clone)]
pub(crate) struct MediaPlugin {
    cache: Arc<Mutex<Cache>>,
    base: PathBuf,
    document: WeakEntity<TextViewState>,
    block: bool,
}
impl MediaPlugin {
    pub fn new(document: &Entity<TextViewState>, base: &Path) -> Self {
        Self {
            cache: Arc::new(Mutex::new(Cache {
                visible: true,
                ..Default::default()
            })),
            base: base.into(),
            document: document.downgrade(),
            block: false,
        }
    }
    pub fn block(&self) -> Self {
        Self {
            block: true,
            ..self.clone()
        }
    }

    pub fn set_visible(&self, visible: bool, cx: &mut App) {
        let views = {
            let mut cache = self.cache.lock().unwrap();
            cache.visible = visible;
            cache.views.values().cloned().collect::<Vec<_>>()
        };
        for view in views {
            view.update(cx, |view, cx| view.set_visible(visible, cx));
        }
    }

    pub fn shutdown(&self, cx: &mut App) {
        let (views, images) = {
            let mut cache = self.cache.lock().unwrap();
            cache.visible = false;
            cache.closed = true;
            cache.data_images.clear();
            (
                std::mem::take(&mut cache.views),
                std::mem::take(&mut cache.images),
            )
        };
        for view in views.values() {
            let session = view.read(cx).session.clone();
            session.update(cx, |session, cx| session.shutdown(cx));
        }
        for session in images.values() {
            session.update(cx, |session, cx| session.shutdown(cx));
        }
    }

    /// Only full replacements need reconciliation. Appending text cannot
    /// change an already complete tag or the source offset of an earlier one.
    pub fn reconcile(&self, source: &str, cx: &mut App) {
        let mut live = HashSet::new();
        if let Ok(root) = ::markdown::to_mdast(source, &::markdown::ParseOptions::gfm()) {
            collect_keys(&root, source, &mut live);
        }
        let removed = {
            let mut cache = self.cache.lock().unwrap();
            let keys = cache
                .views
                .keys()
                .filter(|key| !live.contains(*key))
                .cloned()
                .collect::<Vec<_>>();
            cache.suppressed.retain(|key| !live.contains(key));
            cache.suppressed.extend(keys.iter().cloned());
            let removed = keys
                .into_iter()
                .filter_map(|key| cache.views.remove(&key))
                .collect::<Vec<_>>();
            cache.images.retain(|src, _| source.contains(src));
            cache.data_images.retain(|src, _| source.contains(src));
            removed
        };
        for view in removed {
            let session = view.read(cx).session.clone();
            session.update(cx, |session, cx| session.shutdown(cx));
        }
    }

    pub fn appended(&self, source: &str, cx: &mut App) {
        let suppressed = !self.cache.lock().unwrap().suppressed.is_empty();
        if suppressed {
            self.reconcile(source, cx);
        }
    }

    pub fn image_resolver(&self) -> impl Fn(&SharedUri) -> ImageSource + Send + Sync + 'static {
        let plugin = self.clone();
        move |url| plugin.image_source(url.as_ref())
    }

    fn image_source(&self, raw: &str) -> ImageSource {
        // Preserve Base's existing embedded data-image support. This is not a
        // route for arbitrary data URLs into the native media decoder.
        if raw.starts_with("data:") {
            let mut cache = self.cache.lock().unwrap();
            if let Some(image) = cache.data_images.get(raw) {
                return image.clone().into();
            }
            if let Ok(data) = data_url::DataUrl::process(raw) {
                let mime = data.mime_type();
                if let Some(format) =
                    ImageFormat::from_mime_type(&format!("{}/{}", mime.type_, mime.subtype))
                    && let Ok((bytes, _)) = data.decode_to_vec()
                {
                    let image = Arc::new(Image::from_bytes(format, bytes));
                    cache.data_images.insert(raw.into(), image.clone());
                    return image.into();
                }
            }
            return failed_image("Invalid embedded image");
        }
        let source = match resolve_source(raw, &self.base) {
            Ok(source) => source,
            Err(error) => return failed_image(&error.to_string()),
        };
        let plugin = self.clone();
        let raw = raw.to_owned();
        ImageSource::Custom(Arc::new(move |_, cx| {
            let session = {
                let mut cache = plugin.cache.lock().unwrap();
                if cache.closed {
                    return None;
                }
                cache
                    .images
                    .entry(raw.clone())
                    .or_insert_with(|| plugin.session(MediaKind::Image, source.clone(), cx))
                    .clone()
            };
            let session = session.read(cx);
            match &session.load_state {
                LoadState::Loading => None,
                LoadState::Ready => session.frame.clone().map(Ok),
                LoadState::Failed(error) => Some(Err(ImageCacheError::Other(Arc::new(
                    anyhow::anyhow!(error.clone()),
                )))),
            }
        }))
    }

    fn session(&self, kind: MediaKind, source: MediaSource, cx: &mut App) -> Entity<MediaSession> {
        let session = cx.new(|cx| MediaSession::new(kind, source, cx));
        let document = self.document.clone();
        // The document lays out images/custom inline objects, so a first frame
        // changing intrinsic dimensions must invalidate its measured layout.
        let mut dimensions = None;
        cx.observe(&session, move |session, cx| {
            let next = session.read(cx).frame.as_ref().map(|frame| frame.size(0));
            let resized = dimensions != next;
            dimensions = next;
            let _ = document.update(cx, |document, cx| {
                if resized {
                    document.invalidate_inline_layout(cx);
                } else {
                    cx.notify();
                }
            });
        })
        .detach();
        session
    }

    fn view(&self, key: Key, window: &mut Window, cx: &mut App) -> gpui_kit::AnyElement {
        let source = match resolve_source(&key.src, &self.base) {
            Ok(source) => source,
            Err(error) => {
                return div()
                    .child(format!("Cannot open media: {error}"))
                    .into_any_element();
            }
        };
        let (existing, visible) = {
            let cache = self.cache.lock().unwrap();
            if cache.closed || cache.suppressed.contains(&key) {
                return div().into_any_element();
            }
            (cache.views.get(&key).cloned(), cache.visible)
        };
        let view = existing.unwrap_or_else(|| {
            let session = self.session(key.kind, source, cx);
            let view = cx.new(|cx| MediaView::new(session, true, window, cx));
            view.update(cx, |view, cx| view.set_visible(visible, cx));
            self.cache.lock().unwrap().views.insert(key, view.clone());
            view
        });
        view.into_any_element()
    }
}

impl MarkdownPlugin for MediaPlugin {
    fn name(&self) -> &str {
        if self.block {
            "rockstar-media-block"
        } else {
            "rockstar-media-inline"
        }
    }
    fn is_block(&self) -> bool {
        self.block
    }
    fn parse(&self, node: &Node, context: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let Node::Html(html) = node else { return None };
        let parts = parse_html(&html.value)?;
        let start = context.offset() + node.position()?.start.offset;
        Some(
            MarkdownNode::new(self.name(), (start, parts))
                .text(html.value.clone())
                .markdown(html.value.clone()),
        )
    }
    fn render(&self, node: &MarkdownNode, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let (start, parts) = node.data::<(usize, Vec<Part>)>().expect("media HTML node");
        let mut content = div().flex().flex_col().gap_2();
        // Block media fills the text width; inline media uses a bounded box in
        // the existing inline flow and does not disturb adjacent text selection.
        if self.block {
            content = content.w_full();
        } else {
            let width = self
                .document
                .upgrade()
                .map(|document| document.read(cx).bounds().size.width)
                .filter(|width| *width > px(0.))
                .unwrap_or(px(300.));
            content = content.w(width.min(px(300.))).min_w_0();
        }
        for (index, part) in parts.iter().enumerate() {
            content = content.child(match part {
                Part::Html(html) => TextView::html(("media-html", index), html.clone())
                    .image_source(self.image_resolver())
                    .into_any_element(),
                Part::Media(kind, src) => self.view(
                    Key {
                        start: *start,
                        index,
                        kind: *kind,
                        src: src.clone(),
                    },
                    window,
                    cx,
                ),
            });
        }
        content
    }
}

fn failed_image(error: &str) -> ImageSource {
    let error = Arc::new(anyhow::anyhow!(error.to_string()));
    ImageSource::Custom(Arc::new(move |_, _| {
        Some(Err(ImageCacheError::Other(error.clone())))
    }))
}

fn collect_keys(node: &Node, source: &str, keys: &mut HashSet<Key>) {
    fn insert(html: &str, start: usize, keys: &mut HashSet<Key>) {
        if let Some(parts) = parse_html(html) {
            for (index, part) in parts.into_iter().enumerate() {
                if let Part::Media(kind, src) = part {
                    keys.insert(Key {
                        start,
                        index,
                        kind,
                        src,
                    });
                }
            }
        }
    }
    fn boundary(node: &Node) -> Option<(bool, String)> {
        let Node::Html(html) = node else { return None };
        let inner = html.value.trim().strip_prefix('<')?.strip_suffix('>')?;
        if inner.contains('>') {
            return None;
        }
        let closing = inner.starts_with('/');
        let name = inner
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()?
            .to_ascii_lowercase();
        matches!(name.as_str(), "audio" | "video").then_some((closing, name))
    }
    if let Node::Html(html) = node
        && let Some(position) = node.position()
    {
        insert(&html.value, position.start.offset, keys);
    }
    if let Some(children) = node.children() {
        let mut index = 0;
        while index < children.len() {
            // Match the complete inline element given to the plugin by Base.
            if let Some((false, name)) = boundary(&children[index]) {
                let mut depth = 0;
                let end = (index + 1..children.len()).find(|&next| {
                    match boundary(&children[next]) {
                        Some((false, tag)) if tag == name => depth += 1,
                        Some((true, tag)) if tag == name => {
                            if depth == 0 {
                                return true;
                            }
                            depth -= 1;
                        }
                        _ => {}
                    }
                    false
                });
                if let Some(end) = end
                    && let (Some(first), Some(last)) =
                        (children[index].position(), children[end].position())
                    && let Some(html) = source.get(first.start.offset..last.end.offset)
                {
                    insert(html, first.start.offset, keys);
                    index = end + 1;
                    continue;
                }
            }
            collect_keys(&children[index], source, keys);
            index += 1;
        }
    }
}

fn tag(node: &Handle) -> Option<&str> {
    match &node.data {
        NodeData::Element { name, .. } => Some(name.local.as_ref()),
        _ => None,
    }
}
fn attr(node: &Handle, key: &str) -> Option<String> {
    match &node.data {
        NodeData::Element { attrs, .. } => attrs
            .borrow()
            .iter()
            .find(|attr| attr.name.local.as_ref() == key)
            .map(|attr| attr.value.to_string()),
        _ => None,
    }
}
fn has_media(node: &Handle) -> bool {
    matches!(tag(node), Some("audio" | "video")) || node.children.borrow().iter().any(has_media)
}

/// HTML5 parsing accepts quoted/unquoted attributes, character references,
/// uppercase tags and <source> children. Non-media content is retained rather
/// than dropped when several elements share a CommonMark raw-HTML block.
fn parse_html(html: &str) -> Option<Vec<Part>> {
    if !html.contains('>') {
        return None;
    }
    let dom = parse_document(RcDom::default(), Default::default()).one(html);
    if !has_media(&dom.document) {
        return None;
    }
    fn walk(node: &Handle, parts: &mut Vec<Part>) {
        match tag(node) {
            Some("audio" | "video") => {
                let kind = if tag(node) == Some("audio") {
                    MediaKind::Audio
                } else {
                    MediaKind::Video
                };
                let src = attr(node, "src")
                    .or_else(|| {
                        node.children
                            .borrow()
                            .iter()
                            .filter(|child| tag(child) == Some("source"))
                            .find_map(|child| attr(child, "src"))
                    })
                    .unwrap_or_default();
                parts.push(Part::Media(kind, src));
            }
            Some("html" | "head" | "body") => {
                for child in node.children.borrow().iter() {
                    walk(child, parts);
                }
            }
            None if matches!(node.data, NodeData::Document) => {
                for child in node.children.borrow().iter() {
                    walk(child, parts);
                }
            }
            _ if has_media(node) => {
                for child in node.children.borrow().iter() {
                    walk(child, parts);
                }
            }
            _ => {
                let mut bytes = Vec::new();
                if serialize(
                    &mut bytes,
                    &SerializableHandle::from(node.clone()),
                    SerializeOpts {
                        traversal_scope: TraversalScope::IncludeNode,
                        ..Default::default()
                    },
                )
                .is_ok()
                {
                    let html = String::from_utf8(bytes).unwrap_or_default();
                    if !html.trim().is_empty() {
                        if let Some(Part::Html(previous)) = parts.last_mut() {
                            previous.push_str(&html);
                        } else {
                            parts.push(Part::Html(html));
                        }
                    }
                }
            }
        }
    }
    let mut parts = Vec::new();
    walk(&dom.document, &mut parts);
    Some(parts)
}
