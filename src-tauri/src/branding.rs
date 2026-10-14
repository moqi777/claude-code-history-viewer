//! Identity constants for the `AgentKB` fork.

/// Where the feedback dialog sends people. Must stay inside this fork: the
/// upstream project is not ours to file issues against.
pub const ISSUES_URL: &str = "https://github.com/moqi777/claude-code-history-viewer/issues/new";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_url_points_at_this_fork() {
        assert!(ISSUES_URL.starts_with("https://github.com/moqi777/"));
        assert!(!ISSUES_URL.contains("jhlee0409"));
    }
}
