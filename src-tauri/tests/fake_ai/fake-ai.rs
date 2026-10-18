//! Dependency-free fake Claude/Codex CLI used by the digest tests on Windows.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::PathBuf;
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

fn text(path: PathBuf) -> String { fs::read_to_string(path).unwrap_or_default() }
fn range(input: &str) -> (u64, u64) {
    let Some(start) = input.find("<messages from=#") else { return (1, 1) };
    let rest = &input[start + 16..];
    let Some(split) = rest.find(" to=#") else { return (1, 1) };
    let from = rest[..split].parse().unwrap_or(1);
    let to = rest[split + 5..].split(|c: char| !c.is_ascii_digit()).next().and_then(|s| s.parse().ok()).unwrap_or(1);
    (from, to)
}
fn json_escape(value: &str) -> String {
    let mut out = String::from("\"");
    for c in value.chars() { match c { '"' => out.push_str("\\\""), '\\' => out.push_str("\\\\"), '\n' => out.push_str("\\n"), '\r' => out.push_str("\\r"), '\t' => out.push_str("\\t"), c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)), c => out.push(c) } }
    out.push('"'); out
}
fn entries(n: u64, from: u64, to: u64, prior: bool) -> String {
    format!(r#"{{"entries":[{{"id":null,"title":"Fake note {n} seq {from}-{to}","problem":"problem text","cause":"cause text","solution":"solution text","conclusion":"fake conclusion{}","keywords":["fake","note"],"seq_from":{from},"seq_to":{to}}}]}}"#, if prior { " after earlier notes" } else { "" })
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().is_some_and(|a| a == "--version") { println!("fake-ai 9.9.9"); return ExitCode::SUCCESS; }
    let Some(value) = env::var_os("FAKE_AI_DIR") else { return ExitCode::from(2) };
    let dir = PathBuf::from(value);
    let n = text(dir.join("count")).trim().parse::<u64>().unwrap_or(0) + 1;
    let _ = fs::write(dir.join("count"), n.to_string());
    let _ = fs::write(dir.join(format!("args-{n}")), args.iter().map(|a| format!("{a}\n")).collect::<String>());
    let _ = fs::write(dir.join(format!("argv-{n}.json")), format!("[{}]", args.iter().map(|a| json_escape(a)).collect::<Vec<_>>().join(",")));
    let _ = fs::write(dir.join(format!("cwd-{n}")), env::current_dir().unwrap_or_default().display().to_string());
    let mut input = String::new(); let _ = io::stdin().read_to_string(&mut input);
    let _ = fs::write(dir.join(format!("input-{n}")), &input);
    let selected = text(dir.join(format!("mode-{n}"))).trim().to_string();
    let mode = if selected.is_empty() { let m = text(dir.join("mode")).trim().to_string(); if m.is_empty() { "ok".to_string() } else { m } } else { selected };
    let codex = args.first().is_some_and(|a| a == "exec");
    let out = args.windows(2).find(|w| w[0] == "-o").map(|w| PathBuf::from(&w[1]));
    let (from, to) = range(&input);
    let answer = entries(n, from, to, input.contains("<previous_notes>"));
    let emit = |answer: &str| {
        if codex {
            if let Some(path) = &out { let _ = fs::write(path, answer); }
            println!("{{\"type\":\"thread.started\",\"thread_id\":\"fake\"}}");
            println!("{{\"type\":\"turn.completed\",\"usage\":{{\"input_tokens\":115,\"cached_input_tokens\":5,\"output_tokens\":20,\"reasoning_output_tokens\":0}}}}");
        } else {
            println!("{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false,\"num_turns\":1,\"result\":{},\"usage\":{{\"input_tokens\":100,\"cache_creation_input_tokens\":10,\"cache_read_input_tokens\":5,\"output_tokens\":20}},\"modelUsage\":{{\"fake-model\":{{}}}},\"total_cost_usd\":0.001}}", json_escape(answer));
        }
    };
    match mode.as_str() {
        "ok" | "update" => emit(&answer),
        "custom" => emit(&text(dir.join("answer.json"))),
        "nothing" => emit("{\"entries\":[]}"),
        "bad_json" => { if codex { if let Some(path) = &out { let _ = fs::write(path, "sorry, I cannot do that"); } println!("sorry, I cannot do that"); } else { println!("{{\"type\":\"result\",\"is_error\":false,\"result\":\"sorry, I cannot do that\"}}"); } },
        "bad_range" => emit(r#"{"entries":[{"id":null,"title":"Out of range","problem":"p","cause":"","solution":"","conclusion":"c","keywords":["k"],"seq_from":900000,"seq_to":900001}]}"#),
        "leaky" => emit(&format!(r#"{{"entries":[{{"id":null,"title":"Leaky","problem":"p","cause":"","solution":"set password=hunter2hunter2 in the file","conclusion":"c","keywords":["k"],"seq_from":{from},"seq_to":{to}}}]}}"#)),
        "quota" => { if codex { println!("{}", r#"{"type":"error","message":"You've hit your usage limit. Upgrade to Pro or try again at 11:56 PM."}"#); println!("{}", r#"{"type":"turn.failed","error":{"message":"You've hit your usage limit. Upgrade to Pro or try again at 11:56 PM."}}"#); } else { println!("{}", r#"{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"You've hit your session limit · resets 8:20pm (Asia/Shanghai)","usage":{"input_tokens":0,"output_tokens":0}}"#); } return ExitCode::from(1); },
        "auth" => { if codex { println!("{}", r#"{"type":"turn.failed","error":{"message":"401 Unauthorized: please log in again"}}"#); } else { println!("{}", r#"{"type":"result","is_error":true,"api_error_status":401,"result":"Invalid API key · Please run /login","usage":{"input_tokens":0,"output_tokens":0}}"#); } return ExitCode::from(1); },
        "fail" => { eprintln!("boom"); return ExitCode::from(1); },
        "slow" => { thread::sleep(Duration::from_secs(30)); emit(&answer); },
        _ => { eprintln!("unknown fake mode {mode}"); return ExitCode::from(3); },
    }
    ExitCode::SUCCESS
}
