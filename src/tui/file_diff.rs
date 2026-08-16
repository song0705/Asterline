//! Line-oriented `+/-` hunks for expanded file-change cards.

use std::cell::RefCell;
use std::collections::HashMap;

const MAX_DIFF_LINES: usize = 400;
const HUNK_CACHE_CAP: usize = 64;

thread_local! {
    static HUNK_CACHE: RefCell<HashMap<(u64, u64), Vec<DiffLine>>> =
        RefCell::new(HashMap::new());
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DiffLine {
    pub kind: char,
    pub text: String,
    /// 1-based source line number for the displayed side of the change.
    pub number: Option<usize>,
}

/// Parses the hunk body of a unified diff. The patch protocol already carries
/// exact source line numbers, so prefer this over reconstructing a diff from
/// incomplete file snapshots.
pub(crate) fn unified_hunks(patch: &str) -> Vec<DiffLine> {
    let mut old_line = 0;
    let mut new_line = 0;
    let mut in_hunk = false;
    let mut out = Vec::new();

    for raw_line in patch.lines() {
        if let Some((old_start, new_start)) = unified_hunk_starts(raw_line) {
            old_line = old_start;
            new_line = new_start;
            in_hunk = true;
            continue;
        }
        if !in_hunk || raw_line.starts_with("\\ No newline at end of file") {
            continue;
        }
        let Some((kind, text, number)) = (match raw_line.as_bytes().first() {
            Some(b' ') => {
                let number = Some(new_line);
                old_line += 1;
                new_line += 1;
                Some((' ', &raw_line[1..], number))
            }
            Some(b'-') => {
                let number = Some(old_line);
                old_line += 1;
                Some(('-', &raw_line[1..], number))
            }
            Some(b'+') => {
                let number = Some(new_line);
                new_line += 1;
                Some(('+', &raw_line[1..], number))
            }
            _ => None,
        }) else {
            continue;
        };
        out.push(DiffLine {
            kind,
            text: text.to_string(),
            number,
        });
    }

    out
}

fn unified_hunk_starts(line: &str) -> Option<(usize, usize)> {
    let line = line.strip_prefix("@@ -")?;
    let (old_range, line) = line.split_once(" +")?;
    let (new_range, _) = line.split_once(" @@")?;
    Some((
        unified_range_start(old_range)?,
        unified_range_start(new_range)?,
    ))
}

fn unified_range_start(range: &str) -> Option<usize> {
    range
        .split_once(',')
        .map_or(range, |(start, _)| start)
        .parse()
        .ok()
}

/// Codex-style line diff. Equal lines stay unmarked; additions are `+`,
/// removals are `-`. Huge files fall back to a truncated all-delete/all-add.
pub(crate) fn line_hunks(old: &str, new: &str) -> Vec<DiffLine> {
    let key = (fnv1a64(old.as_bytes()), fnv1a64(new.as_bytes()));
    HUNK_CACHE.with(|cell| {
        let mut cache = cell.borrow_mut();
        if let Some(hit) = cache.get(&key) {
            return hit.clone();
        }
        let hunks = line_hunks_uncached(old, new);
        if cache.len() >= HUNK_CACHE_CAP {
            cache.clear();
        }
        cache.insert(key, hunks.clone());
        hunks
    })
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn line_hunks_uncached(old: &str, new: &str) -> Vec<DiffLine> {
    let old_lines = old.lines().collect::<Vec<_>>();
    let new_lines = new.lines().collect::<Vec<_>>();
    if old_lines.is_empty() {
        return new_lines
            .into_iter()
            .enumerate()
            .map(|(index, text)| DiffLine {
                kind: '+',
                text: text.to_string(),
                number: Some(index + 1),
            })
            .collect();
    }
    if new_lines.is_empty() {
        return old_lines
            .into_iter()
            .enumerate()
            .map(|(index, text)| DiffLine {
                kind: '-',
                text: text.to_string(),
                number: Some(index + 1),
            })
            .collect();
    }
    if old_lines.len() > MAX_DIFF_LINES || new_lines.len() > MAX_DIFF_LINES {
        let mut out = take_signed(&old_lines, '-', 40);
        if old_lines.len() > 40 || new_lines.len() > 40 {
            out.push(DiffLine {
                kind: ' ',
                text: "…".to_string(),
                number: None,
            });
        }
        out.extend(take_signed(&new_lines, '+', 40));
        return out;
    }
    lcs_hunks(&old_lines, &new_lines)
}

fn take_signed(lines: &[&str], kind: char, limit: usize) -> Vec<DiffLine> {
    lines
        .iter()
        .take(limit)
        .enumerate()
        .map(|(index, text)| DiffLine {
            kind,
            text: (*text).to_string(),
            number: Some(index + 1),
        })
        .collect()
}

fn lcs_hunks(old: &[&str], new: &[&str]) -> Vec<DiffLine> {
    let n = old.len();
    let m = new.len();
    let mut dp = vec![vec![0u16; m + 1]; n + 1];
    for i in 0..n {
        for j in 0..m {
            dp[i + 1][j + 1] = if old[i] == new[j] {
                dp[i][j] + 1
            } else {
                dp[i][j + 1].max(dp[i + 1][j])
            };
        }
    }
    let mut out = Vec::new();
    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && old[i - 1] == new[j - 1] {
            out.push(DiffLine {
                kind: ' ',
                text: old[i - 1].to_string(),
                number: Some(i),
            });
            i -= 1;
            j -= 1;
        } else if j > 0 && (i == 0 || dp[i][j] == dp[i][j - 1]) {
            out.push(DiffLine {
                kind: '+',
                text: new[j - 1].to_string(),
                number: Some(j),
            });
            j -= 1;
        } else {
            out.push(DiffLine {
                kind: '-',
                text: old[i - 1].to_string(),
                number: Some(i),
            });
            i -= 1;
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_one_line_is_minus_then_plus() {
        let hunks = line_hunks("alpha\nbeta\n", "alpha\ngamma\n");
        assert_eq!(
            hunks,
            vec![
                DiffLine {
                    kind: ' ',
                    text: "alpha".to_string(),
                    number: Some(1),
                },
                DiffLine {
                    kind: '-',
                    text: "beta".to_string(),
                    number: Some(2),
                },
                DiffLine {
                    kind: '+',
                    text: "gamma".to_string(),
                    number: Some(2),
                },
            ]
        );
    }

    #[test]
    fn add_file_is_all_plus() {
        let hunks = line_hunks("", "one\ntwo\n");
        assert!(hunks.iter().all(|line| line.kind == '+'));
        assert_eq!(hunks.len(), 2);
    }

    #[test]
    fn unified_patch_preserves_hunk_line_numbers_and_signs() {
        let hunks = unified_hunks(
            "diff --git a/src/lib.rs b/src/lib.rs\n@@ -10,2 +10,3 @@ fn f() {\n unchanged\n-old\n+new\n+extra\n",
        );
        assert_eq!(
            hunks,
            vec![
                DiffLine {
                    kind: ' ',
                    text: "unchanged".to_string(),
                    number: Some(10)
                },
                DiffLine {
                    kind: '-',
                    text: "old".to_string(),
                    number: Some(11)
                },
                DiffLine {
                    kind: '+',
                    text: "new".to_string(),
                    number: Some(11)
                },
                DiffLine {
                    kind: '+',
                    text: "extra".to_string(),
                    number: Some(12)
                },
            ]
        );
    }
}
