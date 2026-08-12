use similar::{ChangeTag, TextDiff};

pub struct DiffStats {
    pub added: usize,
    pub removed: usize,
    /// First changed non-empty line, trimmed to ~120 chars — the inbox preview.
    pub snippet: Option<String>,
}

pub fn stats(old: &str, new: &str) -> DiffStats {
    let diff = TextDiff::from_lines(old, new);
    let mut added = 0usize;
    let mut removed = 0usize;
    let mut snippet: Option<String> = None;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => added += 1,
            ChangeTag::Delete => removed += 1,
            ChangeTag::Equal => continue,
        }
        if snippet.is_none() {
            let line = change.value().trim();
            if !line.is_empty() {
                let mut s: String = line.chars().take(120).collect();
                if line.chars().count() > 120 {
                    s.push('…');
                }
                snippet = Some(s);
            }
        }
    }
    DiffStats { added, removed, snippet }
}

pub fn unified(old: &str, new: &str, context: usize) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(context)
        .header("last seen", "latest")
        .to_string()
}
