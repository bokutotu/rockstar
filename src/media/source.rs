use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use url::Url;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MediaKind {
    Image,
    Audio,
    Video,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) enum MediaSource {
    File(PathBuf),
    Http(Url),
}

impl MediaSource {
    pub(crate) fn label(&self) -> String {
        match self {
            Self::File(path) => path.display().to_string(),
            Self::Http(url) => url.to_string(),
        }
    }

    pub(super) fn resource(&self) -> gpui_kit::Resource {
        match self {
            Self::File(path) => path.clone().into(),
            Self::Http(url) => gpui_kit::Resource::Uri(url.as_str().to_string().into()),
        }
    }

    /// Called on the loading thread, never during layout.
    pub(super) fn validate_file(&self) -> Result<()> {
        if let Self::File(path) = self {
            let metadata = path
                .metadata()
                .with_context(|| format!("Cannot open {}", path.display()))?;
            if !metadata.is_file() {
                bail!("Not a regular file: {}", path.display());
            }
        }
        Ok(())
    }
}

pub(crate) fn classify_file(path: &Path) -> Option<MediaKind> {
    Some(
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => MediaKind::Image,
            "mp3" | "wav" | "ogg" | "oga" | "flac" => MediaKind::Audio,
            "mp4" | "m4v" | "webm" => MediaKind::Video,
            _ => return None,
        },
    )
}

/// Resolve URI spelling (including percent escapes) without performing I/O.
/// Files explicitly selected by the user already have a canonical PathBuf and
/// do not pass through URI parsing, so literal '%' and '#' filenames survive.
pub(crate) fn resolve_source(src: &str, base: &Path) -> Result<MediaSource> {
    if src.is_empty() || src.contains('\0') || src.starts_with("//") {
        bail!("Invalid media reference");
    }
    let base = Url::from_directory_path(base)
        .map_err(|_| anyhow::anyhow!("Media base directory must be absolute"))?;
    let url = base.join(src).context("Invalid media URL")?;
    match url.scheme() {
        "http" | "https" if url.host_str().is_some() => Ok(MediaSource::Http(url)),
        "file" => {
            let path = url
                .to_file_path()
                .map_err(|_| anyhow::anyhow!("Invalid local media URL"))?;
            Ok(MediaSource::File(path))
        }
        _ => bail!("Unsupported media URL scheme: {}", url.scheme()),
    }
}
