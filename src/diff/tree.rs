//! A tree built only from changed paths, including paths absent from the worktree.
use super::model::{DiffEntry, DiffSection};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub(super) enum TreeItem {
    Section(DiffSection),
    Directory(DiffSection, PathBuf),
    File(DiffEntry),
}

impl TreeItem {
    fn section(&self) -> DiffSection {
        match self {
            Self::Section(section) | Self::Directory(section, _) => *section,
            Self::File(entry) => entry.section,
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Section(_) => Path::new(""),
            Self::Directory(_, path) => path,
            Self::File(entry) => entry.path(),
        }
    }

    pub(super) fn entry(&self) -> Option<&DiffEntry> {
        match self {
            Self::File(entry) => Some(entry),
            _ => None,
        }
    }

    pub(super) fn is_folder(&self) -> bool {
        self.entry().is_none()
    }

    pub(super) fn depth(&self) -> usize {
        self.path().components().count()
    }

    pub(super) fn label(&self) -> String {
        match self {
            Self::Section(section) => section.label().into(),
            _ => self
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
        }
    }

    pub(super) fn description(&self) -> String {
        match self {
            Self::Section(section) => section.label().into(),
            Self::Directory(section, path) => format!("{} {}", section.label(), path.display()),
            Self::File(entry) => format!(
                "{} {} {}",
                entry.section.label(),
                entry.kind.label(),
                entry.label()
            ),
        }
    }

    fn same_item(&self, other: &Self) -> bool {
        self.section() == other.section()
            && self.path() == other.path()
            && self.is_folder() == other.is_folder()
    }
}

fn build_tree(entries: &[DiffEntry]) -> Vec<TreeItem> {
    let mut folders = BTreeSet::new();
    let mut items = Vec::new();
    for entry in entries {
        for parent in entry.path().ancestors().skip(1) {
            folders.insert((entry.section, parent.to_path_buf()));
        }
        items.push(TreeItem::File(entry.clone()));
    }
    items.extend(folders.into_iter().map(|(section, path)| {
        if path.as_os_str().is_empty() {
            TreeItem::Section(section)
        } else {
            TreeItem::Directory(section, path)
        }
    }));
    // The same component ordering as FileTree, without converting path identity
    // to UTF-8 (Git paths can contain arbitrary bytes).
    items.sort_by_cached_key(|item| {
        let depth = item.depth();
        let components = item
            .path()
            .components()
            .enumerate()
            .map(|(i, component)| {
                (
                    i + 1 == depth && !item.is_folder(),
                    component.as_os_str().to_owned(),
                )
            })
            .collect::<Vec<_>>();
        (item.section(), components)
    });
    items
}

#[derive(Clone, Copy)]
pub(super) enum TreeAction {
    Up,
    Down,
    Open,
    Expand,
    Collapse,
}

#[derive(Default)]
pub(super) struct DiffTree {
    items: Vec<TreeItem>,
    visible: Vec<usize>,
    // New sections/folders start expanded. Each comparison has its own state.
    collapsed: BTreeSet<(DiffSection, PathBuf)>,
    pub(super) selected: usize,
}

impl DiffTree {
    pub(super) fn new(entries: &[DiffEntry]) -> Self {
        let mut tree = Self::default();
        tree.refresh(entries);
        tree.selected = (0..tree.len())
            .find(|&i| tree.item(i).entry().is_some())
            .unwrap_or(0);
        tree
    }

    pub(super) fn len(&self) -> usize {
        self.visible.len()
    }

    pub(super) fn item(&self, index: usize) -> &TreeItem {
        &self.items[self.visible[index]]
    }

    pub(super) fn selected_entry(&self) -> Option<&DiffEntry> {
        self.visible
            .get(self.selected)
            .and_then(|&i| self.items[i].entry())
    }

    pub(super) fn first_entry(&self) -> Option<&DiffEntry> {
        self.items.iter().find_map(TreeItem::entry)
    }

    pub(super) fn is_expanded(&self, item: &TreeItem) -> bool {
        item.is_folder()
            && !self
                .collapsed
                .contains(&(item.section(), item.path().to_path_buf()))
    }

    pub(super) fn refresh(&mut self, entries: &[DiffEntry]) {
        let selected = self
            .visible
            .get(self.selected)
            .map(|&i| self.items[i].clone());
        self.items = build_tree(entries);
        let folders = self
            .items
            .iter()
            .filter(|item| item.is_folder())
            .map(|item| (item.section(), item.path().to_path_buf()))
            .collect::<BTreeSet<_>>();
        self.collapsed.retain(|key| folders.contains(key));
        self.rebuild(selected.as_ref());
    }

    fn rebuild(&mut self, selected: Option<&TreeItem>) {
        self.visible = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| {
                let hidden = item.path().ancestors().skip(1).any(|parent| {
                    self.collapsed
                        .contains(&(item.section(), parent.to_path_buf()))
                });
                (!hidden).then_some(i)
            })
            .collect();
        let retained = selected.and_then(|selected| {
            self.visible
                .iter()
                .position(|&i| self.items[i].same_item(selected))
        });
        self.selected = retained.unwrap_or(self.selected.min(self.len().saturating_sub(1)));
    }

    pub(super) fn select(&mut self, index: usize) {
        self.selected = index.min(self.len().saturating_sub(1));
    }

    /// Navigation alone never changes the open document. Only opening a file
    /// returns an entry; opening a section/directory toggles that branch.
    pub(super) fn apply(&mut self, action: TreeAction) -> Option<DiffEntry> {
        let item = self
            .visible
            .get(self.selected)
            .map(|&i| self.items[i].clone())?;
        match action {
            TreeAction::Up => self.selected = self.selected.saturating_sub(1),
            TreeAction::Down => self.selected = (self.selected + 1).min(self.len() - 1),
            TreeAction::Open | TreeAction::Expand if !item.is_folder() => {
                return item.entry().cloned();
            }
            TreeAction::Open => {
                let key = (item.section(), item.path().to_path_buf());
                if !self.collapsed.remove(&key) {
                    self.collapsed.insert(key);
                }
                self.rebuild(Some(&item));
            }
            TreeAction::Expand => {
                if self
                    .collapsed
                    .remove(&(item.section(), item.path().to_path_buf()))
                {
                    self.rebuild(Some(&item));
                } else if self.selected + 1 < self.len()
                    && self.item(self.selected + 1).depth() > item.depth()
                {
                    self.selected += 1;
                }
            }
            TreeAction::Collapse => {
                if self.is_expanded(&item) {
                    self.collapsed
                        .insert((item.section(), item.path().to_path_buf()));
                    self.rebuild(Some(&item));
                } else if let Some(parent) = item.path().parent() {
                    if let Some(index) = self.visible.iter().position(|&i| {
                        let candidate = &self.items[i];
                        candidate.is_folder()
                            && candidate.section() == item.section()
                            && candidate.path() == parent
                    }) {
                        self.selected = index;
                    }
                }
            }
        }
        None
    }
}
