//! Line diff (LCS) with context, for readable transcript comparisons.

/// Unified-style diff of `a` and `b`; empty when they are equal.
pub fn diff(a: &[String], b: &[String], label_a: &str, label_b: &str) -> String {
    if a == b {
        return String::new();
    }
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            ops.push((' ', &a[i]));
            i += 1;
            j += 1;
        } else if i < n && (j == m || lcs[i + 1][j] >= lcs[i][j + 1]) {
            ops.push(('-', &a[i]));
            i += 1;
        } else {
            ops.push(('+', &b[j]));
            j += 1;
        }
    }
    const CONTEXT: usize = 3;
    let mut out = format!("--- {label_a}\n+++ {label_b}\n");
    let changed: Vec<usize> = (0..ops.len()).filter(|&k| ops[k].0 != ' ').collect();
    let mut shown_until = 0usize;
    for &k in &changed {
        let start = k.saturating_sub(CONTEXT).max(shown_until);
        if start > shown_until || (shown_until == 0 && start > 0) {
            out.push_str("@@\n");
        }
        let end = (k + CONTEXT + 1).min(ops.len());
        for (sign, line) in &ops[start.max(shown_until)..end] {
            out.push_str(&format!("{sign} {line}\n"));
        }
        shown_until = shown_until.max(end);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(str::to_owned).collect()
    }

    #[test]
    fn equal_inputs_have_no_diff() {
        assert!(diff(&lines("a\nb"), &lines("a\nb"), "A", "B").is_empty());
    }

    #[test]
    fn shows_changed_lines_with_context() {
        let a = lines("1\n2\n3\n4\n5\n6\n7\n8\n9");
        let b = lines("1\n2\n3\n4\nX\n6\n7\n8\n9");
        let out = diff(&a, &b, "A", "B");
        assert!(out.contains("- 5\n+ X\n"), "{out}");
        assert!(out.contains("  4\n"), "{out}");
        assert!(!out.contains("  1\n"), "{out}");
    }
}
