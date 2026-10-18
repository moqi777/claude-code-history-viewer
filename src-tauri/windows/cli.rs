//! Console entry point for Windows skills. No GUI, shell wrapper or duplicated
//! search implementation: both binaries dispatch into the same KB library.
#![cfg_attr(windows, windows_subsystem = "console")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let code = if let Some("--kb-search" | "--kb-show" | "--kb-skill-print") =
        args.get(1).map(String::as_str)
    {
        claude_code_history_viewer_lib::kb::cli::try_run(&args).unwrap_or(2)
    } else {
        eprintln!("usage: agentkb-cli --kb-search <words> [options] | --kb-show <source:session-id> [options] | --kb-skill-print claude|codex");
        2
    };
    std::process::exit(code);
}
