"""Offline Windows release CLI check. All data and executables live in a temp home.

Usage: python scripts/p7_windows_smoke.py path/to/AgentKB.exe [--scheduled]
Prints the exact rendered skill commands and complete UTF-8 JSON responses.
The optional check registers, runs and deletes a unique temporary scheduled task.
Set AGENTKB_TEST_POWERSHELL to test another PowerShell executable (e.g. 5.1).
"""

from contextlib import closing
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
import uuid
from xml.sax.saxutils import escape


def scheduled_check(root, exe, config, env, powershell):
    """Run real scheduled sync with a slow, offline fake AI; never the user's CLI."""
    repository = Path(__file__).resolve().parent.parent
    fake = root / "fake-ai"
    fake.mkdir()
    subprocess.run(["rustc", str(repository / "src-tauri/tests/fake_ai/fake-ai.rs"),
                    "-o", str(fake / "fake-ai.exe")], check=True, capture_output=True)
    (fake / "mode").write_text("slow", encoding="utf-8")
    settings = json.loads(config.read_text(encoding="utf-8"))
    settings["digest"] = {"enabled": True, "engine": "claude", "cli_path": str(fake / "fake-ai.exe"),
                          "time": "00:00", "quiet_minutes": 0, "min_messages": 1,
                          "max_sessions_per_round": 1, "recent_days": 365}
    settings["semantic"] = {"enabled": True}
    config.write_text(json.dumps(settings, ensure_ascii=False), encoding="utf-8")
    task_env = {key: env[key] for key in ["CCHV_TEST_HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA"]}
    task_env["FAKE_AI_DIR"] = str(fake)
    literal = lambda value: json.dumps(str(value), ensure_ascii=False)
    env_calls = "".join(f".env({literal(k)}, {literal(v)})" for k, v in task_env.items())
    # Task Scheduler does not inherit this Python process's environment. The
    # GUI wrapper explicitly supplies the sandbox then waits for actual sync.
    wrapper = root / "scheduled-sync.rs"
    wrapper.write_text(f'''#![windows_subsystem = "windows"]
fn main() {{
    let log = std::fs::File::create({literal(root / "sync-output.json")}).unwrap();
    let mut command = std::process::Command::new({literal(exe)});
    command.args(["--kb-sync", "--scheduled"]){env_calls}
        .stdin(std::process::Stdio::null()).stdout(log.try_clone().unwrap()).stderr(log);
    let status = command.status().unwrap();
    std::fs::write({literal(root / "sync-exited")}, status.code().unwrap_or(-1).to_string()).unwrap();
}}
''', encoding="utf-8")
    runner = root / "scheduled-sync.exe"
    subprocess.run(["rustc", "--crate-name", "scheduled_sync", str(wrapper), "-o", str(runner)],
                   check=True, capture_output=True)
    user = subprocess.check_output([powershell, "-NoProfile", "-NonInteractive", "-Command",
                                    "[System.Security.Principal.WindowsIdentity]::GetCurrent().Name"]).decode("utf-8").strip()
    name = "AgentKB-P7-real-" + str(uuid.uuid4())
    source = (repository / "src-tauri/src/kb/schedule.rs").read_text(encoding="utf-8")
    xml = re.search(r'r#"(<\?xml.*?</Task>\n)"#', source, re.S).group(1)
    for value in [name, "600", escape(str(runner)), escape(str(root))]:
        xml = xml.replace("{}", value, 1)
    xml = xml.replace("{user}", escape(user)).replace("--kb-sync --scheduled", "")
    xml = xml.replace("<Enabled>true</Enabled>", "<Enabled>false</Enabled>")
    definition = root / "task.xml"
    definition.write_text(xml, encoding="utf-16")

    def task(*args):
        result = subprocess.run(["schtasks.exe", *args], capture_output=True)
        assert result.returncode == 0, repr(result.stderr)

    try:
        task("/Create", "/TN", name, "/XML", str(definition), "/F")
        task("/Run", "/TN", name)
        deadline = time.monotonic() + 90
        while not (root / "sync-exited").exists() and time.monotonic() < deadline:
            time.sleep(0.2)
        assert (root / "sync-exited").read_text() == "0"
        print("\nSCHEDULED SYNC: " + (root / "sync-output.json").read_text(encoding="utf-8").strip())
        with closing(sqlite3.connect(root / "AgentKB/kb.db")) as db:
            assert db.execute("SELECT count(*) FROM notes").fetchone()[0] == 0
        print("Scheduled sync exited successfully before slow fake AI wrote a note")
        count = 0
        while time.monotonic() < deadline:
            with closing(sqlite3.connect(root / "AgentKB/kb.db")) as db:
                count = db.execute("SELECT count(*) FROM notes").fetchone()[0]
            if count:
                break
            time.sleep(0.5)
        assert count == 1, "background digest did not finish after scheduled sync exited"
        print(f"Background digest completed after sync exit: notes={count}; fake AI only")
        for path in sorted((root / "AgentKB/logs").glob("embed-*.log")):
            print("EMBED LOG: " + path.read_text(encoding="utf-8").strip())
        # Let normal child shutdown close SQLite/log handles before temp cleanup.
        time.sleep(1)
    finally:
        subprocess.run(["schtasks.exe", "/End", "/TN", name], capture_output=True)
        task("/Delete", "/TN", name, "/F")
        print("Deleted temporary task: " + name)


def write_jsonl(path, rows):
    path.parent.mkdir(parents=True, exist_ok=True)
    # Match the compact JSONL written by the real CLIs (the existing message
    # extractor uses compact type fields for its fast path).
    path.write_text("".join(json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n" for row in rows), encoding="utf-8")


def snapshot(roots):
    return {str(p): hashlib.sha256(p.read_bytes()).hexdigest()
            for root in roots for p in root.rglob("*") if p.is_file()}


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    sys.stderr.reconfigure(encoding="utf-8")
    binary = Path(sys.argv[1]).resolve(strict=True)
    bash = Path(os.environ.get("ProgramFiles", r"C:\Program Files")) / "Git/bin/bash.exe"
    powershell = os.environ.get("AGENTKB_TEST_POWERSHELL") or shutil.which("pwsh.exe")
    assert bash.is_file() and powershell, "Git Bash and PowerShell are required"
    with tempfile.TemporaryDirectory(prefix="agentkb-p7-用户 with space-") as directory:
        root = Path(directory)
        exe = root / "Local AppData/Programs/AgentKB/AgentKB.exe"
        exe.parent.mkdir(parents=True)
        shutil.copy2(binary, exe)
        claude, codex = root / ".claude", root / ".codex"
        project = str(root / "中文项目")
        for index in [1, 2]:
            sid = f"11111111-2222-3333-4444-{index:012}"
            write_jsonl(claude / f"projects/test/{sid}.jsonl", [
                {"type": "user", "uuid": f"u{index}", "sessionId": sid, "timestamp": "2026-10-17T01:00:00Z",
                 "cwd": project, "message": {"role": "user", "content": f"数据库中文检索验证 {index}：如何保留完整的中文输出？"}},
                {"type": "assistant", "uuid": f"a{index}", "sessionId": sid, "timestamp": "2026-10-17T01:01:00Z",
                 "message": {"role": "assistant", "content": [{"type": "text", "text": "使用 UTF-8 编码，检查数据库搜索结果。"}]}},
            ])
        sid = "019e7371-a6b9-7a50-9825-b7acd6255914"
        write_jsonl(codex / f"sessions/2026/10/17/rollout-2026-10-17T01-00-00-{sid}.jsonl", [
            {"type": "session_meta", "timestamp": "2026-10-17T01:00:00Z",
             "payload": {"id": sid, "cwd": project, "originator": "codex-tui", "source": "cli"}},
            {"type": "response_item", "timestamp": "2026-10-17T01:00:01Z",
             "payload": {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "数据库备份支持中文路径，输出必须完整。"}]}},
        ])
        config = root / ".config/agent-kb/config.json"
        config.parent.mkdir(parents=True)
        config.write_text(json.dumps({
            "version": 1, "data_dir": str(root / "AgentKB"), "schedule_enabled": False,
            "sources": {"claude": {"enabled": True, "root": str(claude)},
                        "codex": {"enabled": True, "root": str(codex)}},
            "digest": {"enabled": False}, "semantic": {"enabled": False},
        }, ensure_ascii=False), encoding="utf-8")
        env = os.environ.copy()
        env.update(CCHV_TEST_HOME=str(root), USERPROFILE=str(root), APPDATA=str(root / "AppData"),
                   LOCALAPPDATA=str(root / "Local AppData"), MSYS_NO_PATHCONV="1")
        before = snapshot([claude, codex])

        def run(args):
            result = subprocess.run(args, env=env, capture_output=True, check=True)
            output = result.stdout.decode("utf-8", errors="strict").strip()
            assert "\ufffd" not in output
            return output

        print("CCHV_TEST_HOME=" + str(root))
        assert "template 5" in run([str(exe), "--kb-skill-print", "codex"]), "use this revision's release build"
        print("SYNC " + run([str(exe), "--kb-sync"]))
        results = []
        for target in ["claude", "codex"]:
            template = run([str(exe), "--kb-skill-print", target])
            command = re.search(r"2\. 搜索：`([^`]+)`", template).group(1).replace("<关键词>", "数据库")
            shell = [str(bash), "--noprofile", "--norc", "-c"] if target == "claude" else [powershell, "-NoProfile", "-NonInteractive", "-Command"]
            print(f"\n{target} COMMAND: {command}")
            output = run([*shell, command])
            print(output)
            value = json.loads(output)
            if value["matched_sessions"] != 3:
                with closing(sqlite3.connect(root / "AgentKB/kb.db")) as db:
                    print("SESSIONS:", db.execute("SELECT source, session_id, message_count, cwd_excluded FROM sessions").fetchall())
                    print("MESSAGES:", db.execute("SELECT kind, text FROM messages").fetchall())
            assert value["matched_sessions"] == 3, value
            assert "数据库" in output and "中文" in output
            results.append(value)
        assert results[0] == results[1], "Shells must return the same complete JSON"
        assert snapshot([claude, codex]) == before, "Collection/search changed source fixtures"
        print("\nPASS: both shells returned identical complete UTF-8 JSON; 3 sessions; source hashes unchanged")
        if "--scheduled" in sys.argv[2:]:
            scheduled_check(root, exe, config, env, powershell)
            assert snapshot([claude, codex]) == before


if __name__ == "__main__":
    main()
