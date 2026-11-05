#!/usr/bin/env python3
"""Exercise real Fcitx5/Mozc candidate placement on a private X11 display.

Requires Xvfb, dbus-run-session, xdotool, and Fcitx5 with Mozc. No API calls are
made: a stub pi records prompts. See tests/README.md for usage.
"""

import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time


PROFILE = """[Groups/0]
Name=Default
Default Layout=us
DefaultIM=mozc

[Groups/0/Items/0]
Name=keyboard-us
Layout=

[Groups/0/Items/1]
Name=mozc
Layout=

[GroupOrder]
0=Default
"""


def wait_for(probe, description, timeout=10):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(f"Timed out waiting for {description}")


def check(binary, scale, preedit, lsp=False):
    with tempfile.TemporaryDirectory(prefix="rockstar-ime-") as directory:
        home = Path(directory)
        config = home / ".config/fcitx5"
        (config / "conf").mkdir(parents=True)
        (config / "profile").write_text(PROFILE)
        (config / "conf/xim.conf").write_text(
            f"UseOnTheSpot={'True' if preedit == 'on' else 'False'}\n"
        )
        (home / "runtime").mkdir(mode=0o700)
        (home / "bin").mkdir()
        stub = home / "bin/pi"
        stub.write_text(
            "#!/usr/bin/env python3\n"
            "import os, sys\n"
            "for line in sys.stdin:\n"
            "    with open(os.environ['ROCKSTAR_IME_PROMPTS'], 'a') as output:\n"
            "        output.write(line)\n"
        )
        stub.chmod(0o755)
        prompts = home / "prompts.jsonl"
        workspace = home / "workspace"
        workspace.mkdir()
        source = workspace / "unicode.rs"
        source.write_text("// ")
        if lsp:
            # Same production LSP process transport, deterministic annotations.
            mock = Path(__file__).resolve().with_name("mock_lsp.py")
            server = home / "bin/rust-analyzer"
            server.write_text(f"#!/usr/bin/env python3\nimport runpy\nrunpy.run_path({str(mock)!r}, run_name='__main__')\n")
            server.chmod(0o755)
            (workspace / "mock-options.json").write_text(json.dumps({
                "annotations": True, "unsupported": ["completionProvider"],
            }))
            for name in ("plain.txt", "annotated.rs"):
                (workspace / name).write_text("let answer = 42;")
        processes = []
        logs = []
        env = dict(os.environ)

        def start(args, name, cwd=None):
            log = (home / f"{name}.log").open("w+")
            logs.append(log)
            process = subprocess.Popen(args, env=env, cwd=cwd, stdout=log, stderr=log)
            processes.append(process)
            return process

        def run(*args, required=True):
            return subprocess.run(
                args, env=env, capture_output=True, text=True, check=required, timeout=10
            ).stdout.strip()

        def xdo(*args, required=True):
            return run("xdotool", *map(str, args), required=required)

        def geometry(window):
            return {
                key: int(value)
                for key, value in (
                    line.split("=", 1)
                    for line in xdo("getwindowgeometry", "--shell", window).splitlines()
                )
            }

        def candidate():
            windows = xdo(
                "search", "--onlyvisible", "--name", "^Fcitx5 Input Window$", required=False
            ).split()
            return geometry(windows[0]) if windows else None

        def popup():
            wait_for(candidate, "the Mozc candidate window")
            # Allow the asynchronous preedit -> paint -> XIM update to settle.
            time.sleep(0.3)
            bounds = candidate()
            assert bounds is not None, "Candidate window unexpectedly disappeared"
            return bounds

        def key(*keys):
            xdo("key", "--clearmodifiers", *keys)
            time.sleep(0.1)

        def type_text(text):
            # Xdotool emits Linefeed (Ctrl-J) for embedded newlines; the editor's
            # newline gesture is Return. Exercise that actual keyboard operation.
            for index, line in enumerate(text.split("\n")):
                if index:
                    key("Return")
                xdo("type", "--clearmodifiers", "--delay", "15", "--", line)

        def compose():
            run("fcitx5-remote", "-o")
            type_text("nihongo")
            key("space", "space")
            return popup()

        def commit():
            # Explicitly choose hiragana; dictionary learning can reorder kanji candidates.
            key("F6", "Return")
            wait_for(lambda: not candidate(), "composition to finish")

        def replace_draft(text):
            run("fcitx5-remote", "-c")
            key("ctrl+Home", "ctrl+shift+End")
            type_text(text)
            time.sleep(0.2)

        def near(actual, expected, description):
            assert abs(actual - expected) <= 3 * scale, (
                f"{description}: got {actual}, expected {expected}"
            )

        def visible_anchor(bounds, window):
            app = geometry(window)
            assert app["X"] + 5 * scale < bounds["X"] < app["X"] + app["WIDTH"], (
                f"IME is not anchored inside the input: {bounds}, app={app}"
            )
            assert app["Y"] + app["HEIGHT"] - 100 * scale < bounds["Y"] < app["Y"] + app["HEIGHT"], (
                f"IME is not below a visible input row: {bounds}, app={app}"
            )

        try:
            # Xvfb allocates an unused display; never send events to the user's display.
            display_file = home / "display"
            with display_file.open("w") as display:
                process = subprocess.Popen(
                    ["Xvfb", "-displayfd", str(display.fileno()), "-screen", "0", "3200x2400x24", "-nolisten", "tcp"],
                    pass_fds=(display.fileno(),), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                )
                processes.append(process)
            number = wait_for(lambda: display_file.read_text().strip(), "Xvfb")
            env.update({
                "DISPLAY": f":{number}",
                "HOME": directory,
                "XDG_CONFIG_HOME": str(home / ".config"),
                "XDG_DATA_HOME": str(home / ".local/share"),
                "XDG_CACHE_HOME": str(home / ".cache"),
                "XDG_RUNTIME_DIR": str(home / "runtime"),
                "XMODIFIERS": "@im=fcitx",
                "GPUI_X11_SCALE_FACTOR": str(scale),
                "PATH": str(home / "bin") + os.pathsep + os.environ["PATH"],
                "ROCKSTAR_IME_PROMPTS": str(prompts),
                "ROCKSTAR_IME_TEST_HOME": directory,
            })
            env.pop("WAYLAND_DISPLAY", None)
            start(["fcitx5", "-D", "--disable", "wayland,waylandim"], "fcitx")
            wait_for(
                lambda: subprocess.run(
                    ["fcitx5-remote", "--check"], env=env,
                    stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=5,
                ).returncode == 0,
                "Fcitx5",
            )
            app = start([str(binary)], "rockstar", cwd=workspace)
            windows = wait_for(
                lambda: xdo("search", "--onlyvisible", "--pid", app.pid, required=False).split(),
                "Rockstar",
            )
            window = windows[0]
            xdo("windowmove", window, 100, 100)
            xdo("windowsize", window, 960 * scale, 720 * scale)
            xdo("windowfocus", "--sync", window)
            time.sleep(0.5)
            run("fcitx5-remote", "-c")
            key("colon")
            type_text("chat")
            key("Return")
            time.sleep(0.5)
            key("i")
            xdo("mousemove", "--window", window, 50 * scale, 670 * scale, "click", 1)
            run("fcitx5-remote", "-s", "mozc")
            wait_for(lambda: run("fcitx5-remote", "-n") == "mozc", "Mozc")

            first = compose()
            visible_anchor(first, window)
            commit()
            key("Return")
            second = compose()
            assert second["Y"] > first["Y"] + 5 * scale, (first, second)
            near(second["X"], first["X"], "new line X")

            xdo("windowsize", window, 720 * scale, 540 * scale)
            resized = popup()
            near(resized["Y"], second["Y"] - 180 * scale, "resize during composition")
            visible_anchor(resized, window)
            commit()

            # Moving the caret without changing the draft must also update XIM.
            key("Home", "Right")
            moved = compose()
            assert moved["X"] > resized["X"] + 3 * scale, (resized, moved)
            near(moved["Y"], resized["Y"], "horizontal caret movement")
            commit()

            replace_draft("x" * 80)
            wide = compose()
            xdo("windowsize", window, 360 * scale, 540 * scale)
            # Window resize, repaint, XIM, and Fcitx updates are asynchronous.
            # Wait for the required wrapped-row position, not a fixed sleep.
            wrapped = wait_for(
                lambda: bounds if (bounds := candidate()) and bounds["Y"] > wide["Y"] + 5 * scale else None,
                "the candidate window below the wrapped input row",
            )
            visible_anchor(wrapped, window)
            commit()

            # More rows than fit: coordinates must include the textarea's scroll offset.
            replace_draft("\n".join(f"row {n}" for n in range(9)))
            scrolled = compose()
            visible_anchor(scrolled, window)
            commit()
            key("ctrl+Home")
            top = compose()
            assert top["Y"] < scrolled["Y"] - 5 * scale, (scrolled, top)
            near(top["X"], first["X"], "scroll back to first column")
            visible_anchor(top, window)
            commit()

            assert not prompts.exists(), "Ordinary conversion/Enter unexpectedly sent a prompt"
            replace_draft("send")
            run("fcitx5-remote", "-o")
            type_text("nihongo")
            key("space", "F6", "Return")
            key("ctrl+Return")
            records = wait_for(
                lambda: prompts.read_text().splitlines() if prompts.exists() else None,
                "the explicitly submitted prompt",
            )
            assert len(records) == 1, records
            assert json.loads(records[0])["message"] == "sendにほんご", records

            # Exercise the file editor through the same native keyboard/IME stack.
            run("fcitx5-remote", "-c")
            key("Escape")
            xdo("windowsize", window, 960 * scale, 720 * scale)
            key("backslash", "f", "f")
            type_text("unicode.rs")
            time.sleep(0.5)
            key("Return")
            time.sleep(0.5)
            key("A")
            first_source = compose()
            app_bounds = geometry(window)
            assert app_bounds["Y"] < first_source["Y"] < app_bounds["Y"] + 200 * scale, first_source
            commit()
            key("Return")
            second_source = compose()
            assert second_source["Y"] > first_source["Y"] + 5 * scale, (first_source, second_source)
            commit()
            run("fcitx5-remote", "-c")
            key("Escape", "space", "w")
            wait_for(lambda: source.read_text() == "// にほんご\nにほんご", "the UTF-8 file saved from the editor")
            assert len(prompts.read_text().splitlines()) == 1
            if lsp:
                anchors = []
                for name in ("plain.txt", "annotated.rs"):
                    run("fcitx5-remote", "-c")
                    key("Escape", "backslash", "f", "f")
                    type_text(name)
                    time.sleep(0.3)
                    key("Return")
                    time.sleep(0.6)  # analysis + annotation layout
                    key("A")
                    anchors.append(compose())
                    commit()
                    run("fcitx5-remote", "-c")
                    key("Escape", "space", "w")
                    wait_for(lambda: (workspace / name).read_text() == "let answer = 42;にほんご", "annotated source saved without virtual text")
                plain, annotated = anchors
                if preedit == "off":
                    assert annotated["X"] > plain["X"] + 15 * scale, (plain, annotated)
                    assert annotated["Y"] > plain["Y"] + 10 * scale, (plain, annotated)
                else:
                    # Preedit changes invalidate annotations. XIM must track their removal.
                    near(annotated["X"], plain["X"], "preedit removes the inline hint")
                    near(annotated["Y"], plain["Y"], "preedit removes the lens row")
            print(f"PASS: chat + fileview, scale={scale}, on-the-spot={preedit}, lsp={lsp}", flush=True)
        except Exception:
            for log in logs:
                log.flush()
                log.seek(0)
                print(f"--- {log.name} ---\n{log.read()[-8000:]}", file=sys.stderr)
            raise
        finally:
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
            for process in reversed(processes):
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            # Mozc may daemonize. Only terminate processes bearing this test's unique marker.
            marker = f"ROCKSTAR_IME_TEST_HOME={directory}".encode()
            for process in Path("/proc").glob("[0-9]*"):
                try:
                    if marker in (process / "environ").read_bytes().split(b"\0"):
                        os.kill(int(process.name), signal.SIGTERM)
                except (OSError, ProcessLookupError):
                    pass
            for log in logs:
                log.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, default=Path("target/debug/rockstar"))
    parser.add_argument("--scale", type=int, choices=(1, 2), nargs="+", default=(1, 2))
    parser.add_argument("--preedit", choices=("off", "on"), nargs="+", default=("off", "on"))
    parser.add_argument("--lsp", action="store_true", help="also verify inlay/lens IME coordinates through a stdio LSP peer")
    args = parser.parse_args()
    for tool in ("Xvfb", "dbus-run-session", "xdotool", "fcitx5", "fcitx5-remote"):
        if not shutil.which(tool):
            parser.error(f"Required tool not on PATH: {tool}")
    binary = args.binary.resolve(strict=True)
    if os.environ.get("ROCKSTAR_IME_PRIVATE_BUS") != "1":
        os.execvpe(
            "dbus-run-session",
            ["dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), *sys.argv[1:]],
            {**os.environ, "ROCKSTAR_IME_PRIVATE_BUS": "1"},
        )
    for scale in args.scale:
        for preedit in args.preedit:
            check(binary, scale, preedit, args.lsp)


if __name__ == "__main__":
    main()
