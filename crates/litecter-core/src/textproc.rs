use regex::Regex;

/// Compile per-URL ignore patterns, silently dropping invalid ones
/// (an invalid regex must never break checking).
pub fn compile_ignores(patterns: &[String]) -> Vec<Regex> {
    patterns.iter().filter_map(|p| Regex::new(p).ok()).collect()
}

/// Normalize rendered page text so cosmetic churn doesn't read as a change:
/// trim line ends, drop lines matching any ignore pattern, collapse runs of
/// blank lines, trim leading/trailing blanks.
pub fn normalize(raw: &str, ignores: &[Regex]) -> String {
    let mut out: Vec<&str> = Vec::new();
    let mut blank_run = 0usize;
    for line in raw.lines() {
        let line = line.trim_end();
        if !line.is_empty() && ignores.iter().any(|re| re.is_match(line)) {
            continue;
        }
        if line.trim_start().is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue;
            }
            out.push("");
        } else {
            blank_run = 0;
            out.push(line);
        }
    }
    out.join("\n").trim_matches('\n').to_string()
}

/// Content fingerprint for the fast "did anything change" comparison.
pub fn hash(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_collapses_blanks_and_applies_ignores() {
        let ignores = compile_ignores(&["(?i)last updated".to_string()]);
        let raw = "Title  \n\n\n\nBody line\nLast updated: 2026-08-12\n\n";
        assert_eq!(normalize(raw, &ignores), "Title\n\nBody line");
    }

    #[test]
    fn hash_is_stable() {
        assert_eq!(hash("abc"), hash("abc"));
        assert_ne!(hash("abc"), hash("abd"));
    }
}
