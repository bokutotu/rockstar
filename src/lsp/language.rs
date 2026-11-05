//! Language selection and the boundary between source bytes and LSP positions.
use anyhow::{Result, anyhow, bail};
use lsp_types::{Position, TextEdit, Uri};
use serde_json::{Value, json};
use std::{ops::Range, path::Path};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum ServerKind {
    Rust,
    Haskell,
    TypeScript,
    Terraform,
    Ccls,
    Nix,
}

impl ServerKind {
    pub(crate) fn command(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Rust => ("rust-analyzer", &[]),
            Self::Haskell => ("haskell-language-server-wrapper", &["--lsp"]),
            Self::TypeScript => ("typescript-language-server", &["--stdio"]),
            Self::Terraform => ("terraform-ls", &["serve"]),
            Self::Ccls => ("ccls", &[]),
            Self::Nix => ("nixd", &[]),
        }
    }
}

pub(crate) fn language(path: &Path) -> Option<(&'static str, ServerKind)> {
    use ServerKind::*;
    let name = path.file_name()?.to_str()?;
    if name.ends_with(".tf.json") {
        return Some(("terraform", Terraform));
    }
    if name.ends_with(".tfvars.json") {
        return Some(("terraform-vars", Terraform));
    }
    Some(match path.extension()?.to_str()? {
        "rs" => ("rust", Rust),
        "hs" | "hsc" => ("haskell", Haskell),
        "lhs" => ("literate haskell", Haskell),
        "cabal" => ("cabal", Haskell),
        "ts" | "mts" | "cts" => ("typescript", TypeScript),
        "tsx" => ("typescriptreact", TypeScript),
        "tf" => ("terraform", Terraform),
        "tfvars" => ("terraform-vars", Terraform),
        "c" | "h" => ("c", Ccls),
        "cc" | "cpp" | "cxx" | "c++" | "C" | "hh" | "hpp" | "hxx" | "h++" | "H" => ("cpp", Ccls),
        "cu" | "cuh" => ("cuda", Ccls),
        "nix" => ("nix", Nix),
        _ => return None,
    })
}

pub(crate) fn uri(path: &Path) -> Result<Uri> {
    let url = url::Url::from_file_path(path)
        .map_err(|_| anyhow!("Not an absolute file path: {}", path.display()))?;
    Ok(url.as_str().parse()?)
}
pub(crate) fn path(uri: &Uri) -> Result<std::path::PathBuf> {
    url::Url::parse(uri.as_str())?
        .to_file_path()
        .map_err(|_| anyhow!("Not a local file URI: {}", uri.as_str()))
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum Encoding {
    Utf8,
    #[default]
    Utf16,
    Utf32,
}
impl Encoding {
    pub(crate) fn from_initialize(result: &Value) -> Result<Self> {
        match result
            .pointer("/capabilities/positionEncoding")
            .or_else(|| result.get("offsetEncoding"))
            .and_then(Value::as_str)
            .unwrap_or("utf-16")
        {
            "utf-8" => Ok(Self::Utf8),
            "utf-16" => Ok(Self::Utf16),
            "utf-32" => Ok(Self::Utf32),
            other => bail!("Unsupported LSP position encoding: {other}"),
        }
    }
    fn width(self, c: char) -> usize {
        match self {
            Self::Utf8 => c.len_utf8(),
            Self::Utf16 => c.len_utf16(),
            Self::Utf32 => 1,
        }
    }
    pub(crate) fn position(self, text: &str, offset: usize) -> Result<Position> {
        if !text.is_char_boundary(offset) {
            bail!("Invalid source offset {offset}");
        }
        let prefix = &text[..offset];
        let line = prefix.bytes().filter(|&c| c == b'\n').count();
        let start = prefix.rfind('\n').map_or(0, |at| at + 1);
        Ok(Position::new(
            line.try_into()?,
            prefix[start..]
                .chars()
                .map(|c| self.width(c))
                .sum::<usize>()
                .try_into()?,
        ))
    }
    pub(crate) fn offset(self, text: &str, pos: Position) -> Result<usize> {
        let start = if pos.line == 0 {
            0
        } else {
            text.match_indices('\n')
                .nth(pos.line as usize - 1)
                .map(|(i, _)| i + 1)
                .ok_or_else(|| anyhow!("LSP line {} is outside the document", pos.line))?
        };
        let line = text[start..]
            .split('\n')
            .next()
            .unwrap_or("")
            .trim_end_matches('\r');
        let mut units = 0;
        for (i, c) in line.char_indices() {
            if units == pos.character as usize {
                return Ok(start + i);
            }
            units += self.width(c);
        }
        if units == pos.character as usize {
            Ok(start + line.len())
        } else {
            bail!("Invalid LSP character position: {pos:?}")
        }
    }
    pub(crate) fn range(self, text: &str, range: lsp_types::Range) -> Result<Range<usize>> {
        let start = self.offset(text, range.start)?;
        let end = self.offset(text, range.end)?;
        if start > end {
            bail!("Reversed LSP range");
        }
        Ok(start..end)
    }
    /// GPUI Kit's Position is a Unicode-scalar column, not UTF-16.
    pub(crate) fn editor_range(
        self,
        text: &str,
        range: lsp_types::Range,
    ) -> Result<lsp_types::Range> {
        let bytes = self.range(text, range)?;
        Ok(lsp_types::Range::new(
            Self::Utf32.position(text, bytes.start)?,
            Self::Utf32.position(text, bytes.end)?,
        ))
    }
}

pub(crate) fn change(old: &str, new: &str, encoding: Encoding, incremental: bool) -> Result<Value> {
    if !incremental {
        return Ok(json!({"text":new}));
    }
    let prefix = old
        .chars()
        .zip(new.chars())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    let suffix = old[prefix..]
        .chars()
        .rev()
        .zip(new[prefix..].chars().rev())
        .take_while(|(a, b)| a == b)
        .map(|(c, _)| c.len_utf8())
        .sum::<usize>();
    Ok(
        json!({"range":{"start":encoding.position(old,prefix)?,"end":encoding.position(old,old.len()-suffix)?},"text":&new[prefix..new.len()-suffix]}),
    )
}

/// Validate every edit before changing anything. Positions refer to the same snapshot.
pub(crate) fn apply_edits(text: &str, edits: &[TextEdit], encoding: Encoding) -> Result<String> {
    let mut edits = edits
        .iter()
        .map(|e| Ok((encoding.range(text, e.range)?, &e.new_text)))
        .collect::<Result<Vec<_>>>()?;
    edits.sort_by_key(|(r, _)| (r.start, r.end));
    for pair in edits.windows(2) {
        if pair[0].0.end > pair[1].0.start || pair[0].0.start == pair[1].0.start {
            bail!("Overlapping LSP edits");
        }
    }
    let mut result = text.to_owned();
    for (range, replacement) in edits.into_iter().rev() {
        result.replace_range(range, replacement);
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positions_round_trip_without_splitting_unicode() {
        let text = "a日本🎸z\n次\n";
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for offset in (0..=text.len()).filter(|&p| text.is_char_boundary(p)) {
                assert_eq!(
                    encoding
                        .offset(text, encoding.position(text, offset).unwrap())
                        .unwrap(),
                    offset
                );
            }
        }
        assert!(Encoding::Utf16.offset(text, Position::new(0, 4)).is_err());
        assert!(Encoding::Utf8.offset(text, Position::new(0, 2)).is_err());
    }
    #[test]
    fn mapping_and_file_uri() {
        for (file, server) in [
            ("a.rs", ServerKind::Rust),
            ("a.hs", ServerKind::Haskell),
            ("a.tsx", ServerKind::TypeScript),
            ("a.tf", ServerKind::Terraform),
            ("a.c", ServerKind::Ccls),
            ("a.cc", ServerKind::Ccls),
            ("a.cu", ServerKind::Ccls),
            ("a.nix", ServerKind::Nix),
        ] {
            assert_eq!(language(Path::new(file)).unwrap().1, server);
        }
        assert_eq!(
            language(Path::new("a.tfvars.json")).unwrap().0,
            "terraform-vars"
        );
        let file = Path::new("/tmp/日本 #%.rs");
        assert_eq!(path(&uri(file).unwrap()).unwrap(), file);
    }
    #[test]
    fn changes_and_edits_use_the_original_snapshot() {
        let old = "日本🎸 abc\nxyz";
        let new = "日本! abc\nxyz";
        let value = change(old, new, Encoding::Utf16, true).unwrap();
        let edit = TextEdit {
            range: serde_json::from_value(value["range"].clone()).unwrap(),
            new_text: value["text"].as_str().unwrap().into(),
        };
        assert_eq!(apply_edits(old, &[edit], Encoding::Utf16).unwrap(), new);
        let edits = vec![
            TextEdit::new(
                lsp_types::Range::new(Position::new(0, 0), Position::new(0, 1)),
                "x".into(),
            ),
            TextEdit::new(
                lsp_types::Range::new(Position::new(0, 0), Position::new(0, 2)),
                "y".into(),
            ),
        ];
        assert!(apply_edits(old, &edits, Encoding::Utf16).is_err());
    }
}
