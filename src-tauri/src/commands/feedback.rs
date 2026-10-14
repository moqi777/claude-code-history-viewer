use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct SystemInfo {
    pub app_version: String,
    pub os_type: String,
    pub os_version: String,
    pub arch: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeedbackData {
    pub subject: String,
    pub body: String,
    pub include_system_info: bool,
    pub feedback_type: String, // "bug", "feature", "improvement", "other"
}

/// Kept for command-surface compatibility. `AgentKB` has no email channel (the
/// upstream address belongs to another project), so this opens the GitHub
/// issue form instead.
#[tauri::command]
pub async fn send_feedback(feedback: FeedbackData) -> Result<(), String> {
    open_github_issues(Some(feedback)).await
}

#[tauri::command]
pub async fn get_system_info() -> Result<SystemInfo, String> {
    Ok(SystemInfo {
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        os_type: std::env::consts::OS.to_string(),
        os_version: "Unknown".to_string(), // Can be obtained from OS plugin
        arch: std::env::consts::ARCH.to_string(),
    })
}

#[tauri::command]
pub async fn open_github_issues(feedback: Option<FeedbackData>) -> Result<(), String> {
    let base_url = crate::branding::ISSUES_URL;

    let github_url = match feedback {
        Some(fb) => {
            if fb.subject.len() > 200 || fb.body.len() > 2000 {
                return Err("Input too long".to_string());
            }

            let title = match fb.feedback_type.as_str() {
                "bug" => format!("[Bug Report] {}", fb.subject),
                "feature" => format!("[Feature Request] {}", fb.subject),
                "improvement" => format!("[Improvement] {}", fb.subject),
                _ => fb.subject.clone(),
            };

            let label = match fb.feedback_type.as_str() {
                "bug" => "bug",
                "feature" | "improvement" => "enhancement",
                _ => "",
            };

            let mut body = fb.body.clone();
            if fb.include_system_info {
                if let Ok(info) = get_system_info().await {
                    body.push_str("\n\n---\n**System Information**\n");
                    body.push_str(&format!("- App Version: {}\n", info.app_version));
                    body.push_str(&format!("- OS: {} {}\n", info.os_type, info.os_version));
                    body.push_str(&format!("- Architecture: {}\n", info.arch));
                }
            }

            let encoded_title = urlencoding::encode(&title);
            let encoded_body = urlencoding::encode(&body);

            if label.is_empty() {
                format!("{base_url}?title={encoded_title}&body={encoded_body}")
            } else {
                let encoded_label = urlencoding::encode(label);
                format!(
                    "{base_url}?title={encoded_title}&body={encoded_body}&labels={encoded_label}"
                )
            }
        }
        None => base_url.to_string(),
    };

    tauri_plugin_opener::open_url(github_url, None::<String>)
        .map_err(|e| format!("Failed to open GitHub: {e}"))?;

    Ok(())
}
