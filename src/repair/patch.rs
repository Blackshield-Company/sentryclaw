//! Extracting and vetting unified diffs produced by the fixer model.

/// Pull a unified diff out of a model reply (fenced ```diff block, or the
/// first line that looks like a diff header onwards).
pub fn extract_diff(reply: &str) -> Option<String> {
    let mut diff = None;
    let mut lines = reply.lines();
    while let Some(line) = lines.next() {
        let t = line.trim_start();
        if t.starts_with("```diff") || t.starts_with("```patch") {
            let body: Vec<&str> = lines
                .by_ref()
                .take_while(|l| !l.trim_start().starts_with("```"))
                .collect();
            diff = Some(body.join("\n"));
            break;
        }
    }
    if diff.is_none() {
        let start = reply
            .find("diff --git ")
            .or_else(|| reply.find("--- a/"))
            .or_else(|| reply.find("--- "))?;
        let body = &reply[start..];
        let end = body.find("\n```").unwrap_or(body.len());
        diff = Some(body[..end].to_string());
    }
    let mut d = diff?;
    if !d.contains("@@") {
        return None;
    }
    if !d.ends_with('\n') {
        d.push('\n');
    }
    Some(d)
}

/// Paths touched by a diff (from `+++`/`---` headers, prefix stripped).
pub fn touched_paths(diff: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in diff.lines() {
        let rest = line
            .strip_prefix("+++ ")
            .or_else(|| line.strip_prefix("--- "));
        if let Some(rest) = rest {
            let p = rest.split('\t').next().unwrap_or(rest).trim();
            if p == "/dev/null" {
                continue;
            }
            let p = p
                .strip_prefix("a/")
                .or_else(|| p.strip_prefix("b/"))
                .unwrap_or(p);
            if !out.iter().any(|x| x == p) {
                out.push(p.to_string());
            }
        }
    }
    out
}

fn is_test_path(p: &str) -> bool {
    let file = p.rsplit('/').next().unwrap_or(p);
    p.starts_with("tests/")
        || p.contains("/tests/")
        || p.starts_with("test/")
        || p.contains("/test/")
        || file.starts_with("test_")
        || file.ends_with("_test.go")
        || file.ends_with("_test.py")
        || file.contains(".test.")
        || file.contains(".spec.")
}

/// Reject diffs that escape the repo, touch VCS metadata, or weaken tests.
pub fn vet_diff(diff: &str, protect_tests: bool) -> Result<(), String> {
    let paths = touched_paths(diff);
    if paths.is_empty() {
        return Err("diff names no files".into());
    }
    for p in &paths {
        if p.starts_with('/') || p.split('/').any(|c| c == "..") {
            return Err(format!("path escapes the repo: {}", p));
        }
        if p == ".git" || p.starts_with(".git/") {
            return Err(format!("diff touches VCS metadata: {}", p));
        }
        if protect_tests && is_test_path(p) {
            return Err(format!("diff edits a test file: {}", p));
        }
    }
    if protect_tests {
        for line in diff.lines() {
            if line.starts_with("---") {
                continue;
            }
            if let Some(removed) = line.strip_prefix('-') {
                let r = removed.trim();
                if r.contains("#[test]")
                    || r.contains("assert")
                    || r.starts_with("def test_")
                    || r.starts_with("func Test")
                {
                    return Err(format!("diff removes or rewrites test code: `{}`", r));
                }
            }
        }
        for line in diff.lines() {
            if line.starts_with("+++") {
                continue;
            }
            if let Some(added) = line.strip_prefix('+') {
                let a = added.trim();
                if a.starts_with("#[ignore")
                    || a.contains("pytest.mark.skip")
                    || a.contains("t.Skip(")
                {
                    return Err(format!("diff disables a test: `{}`", a));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: &str =
        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n fn f() {\n-    1\n+    2\n }\n";

    #[test]
    fn extracts_fenced_and_bare() {
        let fenced = format!("Here you go:\n```diff\n{}```\nDone.", D);
        assert_eq!(extract_diff(&fenced).unwrap(), D);
        let bare = format!("Patch:\n{}", D);
        assert_eq!(extract_diff(&bare).unwrap(), D);
        assert!(extract_diff("no diff here").is_none());
    }

    #[test]
    fn touched_paths_strip_prefix() {
        assert_eq!(touched_paths(D), vec!["src/lib.rs".to_string()]);
    }

    #[test]
    fn vet_rejects_escapes_and_test_edits() {
        assert!(vet_diff(D, true).is_ok());
        let esc = D.replace("src/lib.rs", "../evil.rs");
        assert!(vet_diff(&esc, true).is_err());
        let git = D.replace("src/lib.rs", ".git/config");
        assert!(vet_diff(&git, true).is_err());
        let t = D.replace("src/lib.rs", "tests/it.rs");
        assert!(vet_diff(&t, true).is_err());
        assert!(vet_diff(&t, false).is_ok());
        let weaken = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,2 +1,1 @@\n-        assert_eq!(f(), 1);\n+        \n";
        assert!(vet_diff(weaken, true).is_err());
        let ignore =
            "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,1 +1,2 @@\n+    #[ignore]\n     #[test]\n";
        assert!(vet_diff(ignore, true).is_err());
    }
}
