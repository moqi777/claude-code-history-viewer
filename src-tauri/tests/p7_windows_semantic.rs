//! Opt-in Windows acceptance with the real downloader/model and release EXEs.
//! Downloads about 586 MB into a temporary directory (set TEMP/TMP to D:).
//! `AGENTKB_ACCEPTANCE_EXE` must point at the release AgentKB.exe.
#![cfg(windows)]

use std::path::Path;
use std::process::Command;
use std::sync::atomic::AtomicBool;

use claude_code_history_viewer_lib::kb::{paths::DataPaths, semantic::model};
use serde_json::{json, Value};

fn run(exe: &Path, home: &Path, args: &[&str]) -> Value {
    let output = Command::new(exe)
        .args(args)
        .env("CCHV_TEST_HOME", home)
        .env("AGENTKB_DATA_DIR", home.join("AgentKB"))
        .env("USERPROFILE", home)
        .env("APPDATA", home.join("AppData"))
        .env("LOCALAPPDATA", home.join("Local AppData"))
        .output()
        .expect("start release executable");
    assert!(
        output.status.success(),
        "{args:?}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).expect("complete UTF-8 JSON");
    println!("{args:?}: {result}");
    result
}

#[test]
#[ignore = "downloads 586 MB; requires Windows release EXEs and network"]
fn real_download_and_release_semantic_search() {
    let exe = std::env::var_os("AGENTKB_ACCEPTANCE_EXE")
        .map(std::path::PathBuf::from)
        .expect("AGENTKB_ACCEPTANCE_EXE");
    assert!(exe.is_file());
    let console = exe.with_file_name("agentkb-cli.exe");
    assert!(console.is_file());
    let tmp = tempfile::Builder::new()
        .prefix("agentkb-p7-semantic-用户 ")
        .tempdir()
        .unwrap();
    let home = tmp.path();
    println!("Temporary home: {}", home.display());
    let data = DataPaths::new(home.join("AgentKB"));
    let sources = home.join(".claude/projects/fixture");
    std::fs::create_dir_all(&sources).unwrap();
    let mut originals = Vec::new();
    for (number, text) in [
        (1, "手机电池耗电异常，关闭后台定位后续航恢复正常。"),
        (2, "番茄炒蛋的做法：先把鸡蛋炒熟，再加西红柿。"),
        (3, "数据库主键冲突时使用唯一索引和事务回滚。"),
    ] {
        let sid = format!("11111111-2222-3333-4444-{number:012}");
        let path = sources.join(format!("{sid}.jsonl"));
        let row = json!({
            "type": "user", "uuid": format!("u{number}"), "sessionId": sid,
            "timestamp": "2026-10-17T01:00:00Z", "cwd": home.join("中文项目"),
            "message": {"role": "user", "content": text}
        });
        let bytes = format!("{row}\n").into_bytes();
        std::fs::write(&path, &bytes).unwrap();
        originals.push((path, bytes));
    }
    let config = home.join(".config/agent-kb/config.json");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        json!({
            "version": 1, "data_dir": data.root(), "schedule_enabled": false,
            "sources": {
                "claude": {"enabled": true, "root": home.join(".claude")},
                "codex": {"enabled": false, "root": home.join(".codex")}
            },
            "digest": {"enabled": false},
            "semantic": {"enabled": true, "source": "modelscope"}
        })
        .to_string(),
    )
    .unwrap();
    assert_eq!(run(&exe, home, &["--kb-sync"])["status"], "ok");
    assert_eq!(
        model::check(&data, &model::SPEC),
        model::ModelStatus::Missing
    );
    let mut last_bucket = 0;
    model::download(
        &data,
        &model::SPEC,
        model::Source::Modelscope,
        &AtomicBool::new(false),
        &mut |progress| {
            let bucket = progress.done_bytes / (50 * 1024 * 1024);
            if bucket > last_bucket {
                println!(
                    "Downloaded {}/{} bytes",
                    progress.done_bytes, progress.total_bytes
                );
                last_bucket = bucket;
            }
        },
    )
    .expect("real model download through the settings-page implementation");
    assert_eq!(model::check(&data, &model::SPEC), model::ModelStatus::Ready);
    let test = run(&exe, home, &["--kb-embed", "--test"]);
    assert_eq!(test["ok"], true);
    assert_eq!(test["dim"], 1024);
    assert_eq!(run(&exe, home, &["--kb-embed", "--all"])["outcome"], "Ran");
    // No literal query overlap: success must come from the vector index.
    let query = "为什么充满了没多久就得接电源";
    let result = run(&console, home, &["--kb-search", query, "--verbose"]);
    assert_eq!(result["semantic"]["state"], "used");
    let first = &result["results"][0];
    assert_eq!(
        first["session"]["session_id"],
        "11111111-2222-3333-4444-000000000001"
    );
    assert_eq!(first["match"], "semantic");
    assert_eq!(first["hit_messages"], 0);
    for (path, bytes) in originals {
        assert_eq!(
            std::fs::read(path).unwrap(),
            bytes,
            "source fixture changed"
        );
    }
    println!("PASS: downloaded/verified model, release CPU inference, natural query ranked battery first; source fixtures unchanged");
    tmp.close()
        .expect("remove only this test's temporary directory");
}
