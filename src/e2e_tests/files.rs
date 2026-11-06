//! Public UI scenarios: real dock, real editors, real files and Git; only pi is mocked.
use super::*;
mod items;
mod lifecycle;
mod lsp;
mod visual;
use std::{
    path::Path,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};

struct Project(PathBuf);
impl Project {
    fn new(files: &[(&str, &str)]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rockstar-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        for (path, text) in files {
            let path = root.join(path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        Self(root)
    }
    fn read(&self, path: &str) -> String {
        fs::read_to_string(self.0.join(path)).unwrap()
    }
    fn git(&self, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.0)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

impl Driver {
    fn wait_source(&mut self, path: &Path) {
        let label = format!("File {}", path.display());
        for _ in 0..200 {
            let mut ready = false;
            self.ui(|window, _| {
                ready = window.find_all_by_label(&label).iter().any(|e| e.visible())
            });
            if ready {
                return;
            }
            self.cx.advance_clock(Duration::from_millis(10));
        }
        panic!("file did not appear: {}", path.display());
    }
    fn candidates(&mut self) -> Vec<String> {
        for _ in 0..200 {
            let mut result = Vec::new();
            self.ui(|window, _| {
                result = window
                    .within("file-finder")
                    .find_all_by_role(Role::ListBoxOption)
                    .iter()
                    .map(|e| e.label().unwrap().to_string())
                    .collect();
            });
            if !result.is_empty() {
                return result;
            }
            self.cx.advance_clock(Duration::from_millis(10));
        }
        panic!("file candidates did not appear");
    }
    fn finder(&mut self, query: &str) -> Vec<String> {
        self.press("\\");
        self.press("f");
        self.press("f");
        self.type_text(query);
        self.candidates()
    }
    fn open_file(&mut self, query: &str, path: &Path) {
        let candidates = self.finder(query);
        assert_eq!(candidates[0], query);
        self.press("enter");
        self.wait_source(path);
    }
    fn assert_sources(&mut self, contents: &[&str]) {
        self.ui(|window, _| {
            let actual = window
                .find_all_by_label("Source code")
                .iter()
                .map(|e| e.value().unwrap().to_string())
                .collect::<Vec<_>>();
            assert_eq!(actual, contents);
        });
    }
    fn assert_status(&mut self, status: &str) {
        self.ui(|window, _| assert_eq!(window.find("file-status").label(), Some(status)));
    }
    fn wait_saved(&mut self, project: &Project, name: &str, expected: &str) {
        for _ in 0..200 {
            self.ui(|_, _| {});
            if project.read(name) == expected {
                return;
            }
            self.cx.advance_clock(Duration::from_millis(10));
        }
        assert_eq!(project.read(name), expected);
    }
    fn keys(&mut self, keys: &[&str]) {
        for key in keys {
            self.press(key);
        }
    }
}

#[test]
fn startup_and_splits_offer_usable_plain_panes() {
    let project = Project::new(&[("main.rs", "fn main() {}\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.ui(|window, _| {
        assert_eq!(window.find("plain").label(), Some("Plain"));
        assert_eq!(window.find("pane").focused(), Some(true));
    });
    app.keys(&["s", "v"]);
    app.assert_active_pane(1);
    app.ui(|window, _| assert_eq!(window.find_all("plain").len(), 2));
    app.command("chat");
    app.wait_for_chat(1);
    app.keys(&["i"]);
    app.type_text("Keep this draft");
    app.press("escape");
    app.keys(&["s", "s"]);
    app.assert_active_pane(2);
    app.ui(|window, _| {
        assert_eq!(window.find_all("plain").len(), 2);
        assert_eq!(
            window.find_by_label("Message to pi").value(),
            Some("Keep this draft")
        );
    });
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.assert_sources(&["fn main() {}\n"]);
    app.keys(&["s", "v"]);
    app.assert_active_pane(3);
    app.assert_sources(&["fn main() {}\n"]);
    app.ui(|window, _| assert_eq!(window.find_all("plain").len(), 2));
}

#[test]
fn git_finder_lists_tracked_and_untracked_files_and_opens_the_selected_candidate() {
    let project = Project::new(&[
        (".gitignore", "ignored.rs\ntracked.rs\n"),
        ("ignored.rs", "ignored"),
        ("tracked.rs", "tracked"),
        ("src/main.rs", "main"),
        ("src/map.rs", "map"),
    ]);
    project.git(&["init", "-q"]);
    project.git(&["add", "-f", "tracked.rs"]);
    let mut app = Driver::open_plain_in(project.0.clone());
    let candidates = app.finder("rs");
    assert_eq!(candidates, ["tracked.rs", "src/map.rs", "src/main.rs"]);
    app.press("ctrl-n");
    app.ui(|window, _| {
        let selected = window
            .within("file-finder")
            .find_all_by_role(Role::ListBoxOption)
            .into_iter()
            .filter(|e| e.selected() == Some(true))
            .map(|e| e.label().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(selected, ["src/map.rs"]);
    });
    app.press("enter");
    app.wait_source(&project.0.join("src/map.rs"));
    app.assert_sources(&["map"]);
    app.assert_status("NORMAL · 1:1 · Saved");
}

#[test]
fn tree_expands_opens_a_file_and_returns_keyboard_focus_to_the_editor() {
    let project = Project::new(&[("src/main.rs", "one\ntwo\n"), ("README.md", "read me")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.press("ctrl-b");
    for _ in 0..200 {
        let mut entries = Vec::new();
        app.ui(|window, _| entries = window.within("file-tree").find_all_by_role(Role::TreeItem));
        if entries.len() == 2 {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    app.ui(|window, _| {
        assert_eq!(window.find("file-tree").focused(), Some(true));
        assert_eq!(
            window
                .within("file-tree")
                .find_all_by_role(Role::TreeItem)
                .iter()
                .map(|e| e.label().unwrap())
                .collect::<Vec<_>>(),
            ["src/", "README.md"]
        );
    });
    app.keys(&["l", "j", "enter"]);
    app.wait_source(&project.0.join("src/main.rs"));
    app.assert_sources(&["one\ntwo\n"]);
    app.press("i");
    app.type_text("tree ");
    app.press("escape");
    app.assert_sources(&["tree one\ntwo\n"]);
    app.press("ctrl-b");
    app.keys(&["A"]);
    app.type_text("!");
    app.press("escape");
    app.assert_sources(&["tree one!\ntwo\n"]);
    app.ui(|window, _| assert_eq!(window.find_by_label("Source code").focused(), Some(true)));
}

#[test]
fn vim_insert_visual_yank_paste_undo_redo_and_write_produce_the_expected_file() {
    let project = Project::new(&[("main.rs", "alpha beta\nsecond\nthird\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.keys(&["w", "c", "w"]);
    app.type_text("日本語");
    app.press("escape");
    app.assert_sources(&["alpha 日本語\nsecond\nthird\n"]);
    app.keys(&["j", "0", "V", "y", "p"]);
    app.assert_sources(&["alpha 日本語\nsecond\nsecond\nthird\n"]);
    app.press("u");
    app.assert_sources(&["alpha 日本語\nsecond\nthird\n"]);
    app.press("ctrl-r");
    app.assert_sources(&["alpha 日本語\nsecond\nsecond\nthird\n"]);
    app.keys(&["g", "g", "0", "v", "e", "d"]);
    app.assert_sources(&[" 日本語\nsecond\nsecond\nthird\n"]);
    app.command("w");
    app.wait_saved(&project, "main.rs", " 日本語\nsecond\nsecond\nthird\n");
    app.assert_status("NORMAL · 1:1 · Saved");
}

#[test]
fn file_buffers_share_text_and_undo_but_retain_each_views_edit_position() {
    let project = Project::new(&[("main.rs", "first\nsecond\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.keys(&["j", "s", "v"]);
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("i");
    app.type_text("right ");
    app.press("escape");
    app.assert_sources(&["right first\nsecond\n", "right first\nsecond\n"]);
    app.keys(&["s", "h", "i"]);
    app.type_text("left ");
    app.press("escape");
    app.assert_sources(&["right first\nleft second\n", "right first\nleft second\n"]);
    app.keys(&["s", "l", "u"]);
    app.assert_sources(&["right first\nsecond\n", "right first\nsecond\n"]);
    app.press("u");
    app.assert_sources(&["first\nsecond\n", "first\nsecond\n"]);
}

#[test]
fn buffers_survive_switching_to_chat_and_closing_their_pane() {
    let project = Project::new(&[("a.rs", "a\n"), ("b.rs", "b\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("a.rs", &project.0.join("a.rs"));
    app.press("A");
    app.type_text(" edited");
    app.press("escape");
    app.open_file("b.rs", &project.0.join("b.rs"));
    app.press("ctrl-p");
    app.assert_sources(&["a edited\n"]);
    app.command("chat");
    app.wait_for_chat(1);
    app.press("i");
    app.type_text("chat draft");
    app.press("escape");
    app.open_file("a.rs", &project.0.join("a.rs"));
    app.assert_sources(&["a edited\n"]);
    app.keys(&["s", "v", "s", "h"]);
    app.command("close");
    app.open_file("a.rs", &project.0.join("a.rs"));
    app.assert_sources(&["a edited\n"]);
    // Plain starts at a.rs; cycling backward wraps to the retained chat.
    app.keys(&["s", "v", "ctrl-n", "ctrl-p"]);
    app.ui(|window, _| {
        assert_eq!(
            window.find_by_label("Message to pi").value(),
            Some("chat draft")
        )
    });
}

#[test]
fn insert_shortcuts_save_and_leave_normal_text_and_unicode_intact() {
    let project = Project::new(&[("main.rs", "")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("i");
    app.type_text("日本語 ");
    app.keys(&["j", "k"]);
    app.wait_saved(&project, "main.rs", "日本語 ");
    app.assert_sources(&["日本語 "]);
    app.assert_status("NORMAL · 1:4 · Saved");
    app.press("A");
    app.keys(&["j", "x", "j", "j", "j"]);
    app.assert_sources(&["日本語 jx"]);
    app.assert_status("NORMAL · 1:1 · Modified");
    app.keys(&["A", "j"]);
    app.cx.advance_clock(Duration::from_secs(2));
    app.keys(&["k", "escape", "space", "w"]);
    app.wait_saved(&project, "main.rs", "日本語 jxjk");
}

#[test]
fn search_uses_smartcase_and_escape_restores_the_original_edit_position() {
    let project = Project::new(&[("main.rs", "word Word word\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("/");
    app.type_text("Word");
    app.press("enter");
    app.assert_status("NORMAL · 1:6 · Saved");
    app.press("/");
    app.type_text("word");
    app.press("escape");
    app.assert_status("NORMAL · 1:6 · Saved");
    app.press("n");
    app.assert_status("NORMAL · 1:11 · Saved");
    app.press("N");
    app.assert_status("NORMAL · 1:6 · Saved");
    app.press("i");
    app.type_text("found ");
    app.press("escape");
    app.assert_sources(&["word found Word word\n"]);
}

#[test]
fn saving_preserves_bom_crlf_and_executable_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let project = Project::new(&[("run.sh", "\u{feff}echo one\r\necho two\r\n")]);
    fs::set_permissions(project.0.join("run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("run.sh", &project.0.join("run.sh"));
    app.keys(&["A"]);
    app.type_text("!");
    app.keys(&["escape", "space", "w"]);
    app.wait_saved(&project, "run.sh", "\u{feff}echo one!\r\necho two\r\n");
    assert_eq!(
        fs::metadata(project.0.join("run.sh"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
}

#[test]
fn save_conflict_keeps_the_edited_text_and_the_external_disk_version_available() {
    let project = Project::new(&[("main.rs", "original\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.cx.update(|cx| cx.set_reduce_motion(true));
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("A");
    app.type_text(" edited");
    app.press("escape");
    fs::write(project.0.join("main.rs"), "external\n").unwrap();
    app.command("w");
    for _ in 0..200 {
        let mut visible = false;
        app.ui(|window, _| visible = window.find_all("notification").iter().any(|e| e.visible()));
        if visible {
            break;
        }
        app.cx.advance_clock(Duration::from_millis(10));
    }
    app.assert_sources(&["original edited\n"]);
    app.assert_status("NORMAL · 1:15 · Modified");
    assert_eq!(project.read("main.rs"), "external\n");
    app.ui(|window, _| assert!(window.find("notification").visible()));
    app.press("i");
    app.type_text("!");
    app.press("escape");
    app.assert_sources(&["original edite!d\n"]);
}

#[test]
fn counted_motions_and_deletions_handle_line_and_file_edges() {
    let project = Project::new(&[("edge.txt", "one two\n日本語\nlast")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("edge.txt", &project.0.join("edge.txt"));
    app.keys(&["w", "d", "w"]);
    app.assert_sources(&["one \n日本語\nlast"]);
    app.keys(&["2", "j", "0", "d", "w"]);
    app.assert_sources(&["one \n日本語\n"]);
    app.keys(&["g", "g", "2", "d", "d"]);
    app.assert_sources(&[""]);
    app.keys(&["c", "c"]);
    app.type_text("一二三");
    app.press("escape");
    app.keys(&["0", "2", "l", "d", "l"]);
    app.assert_sources(&["一二"]);
    app.keys(&["u"]);
    app.assert_sources(&["一二三"]);
    app.keys(&["d", "d"]);
    app.assert_sources(&[""]);
    app.command("w");
    app.wait_saved(&project, "edge.txt", "");
}

#[test]
fn cancelling_finder_and_unsaved_close_returns_to_the_editable_document() {
    let project = Project::new(&[("main.rs", "text\n")]);
    let mut app = Driver::open_plain_in(project.0.clone());
    app.open_file("main.rs", &project.0.join("main.rs"));
    app.press("A");
    app.type_text(" draft");
    app.press("escape");
    app.finder("main.rs");
    app.press("escape");
    app.assert_sources(&["text draft\n"]);
    app.ui(|window, _| assert_eq!(window.find_by_label("Source code").focused(), Some(true)));
    app.command("quit");
    app.ui(|window, cx| {
        assert!(window.find("unsaved-files").visible());
        window.click("cancel-close", cx);
    });
    app.press("A");
    app.type_text(" continues");
    app.press("escape");
    app.assert_sources(&["text draft continues\n"]);
    assert_eq!(project.read("main.rs"), "text\n");
}
