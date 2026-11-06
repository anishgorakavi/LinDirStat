use crate::scan::Tree;
use crate::util::age_days;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Safety {
    /// Regenerated automatically; deleting is low-risk.
    Safe,
    /// Probably removable, but look first.
    Review,
}

#[derive(Clone, Debug)]
pub struct Suggestion {
    pub id: usize,
    pub title: String,
    pub reason: String,
    pub size: u64,
    pub safety: Safety,
}

const MB: u64 = 1 << 20;
const GB: u64 = 1 << 30;

/// Directories that tools recreate on demand.
fn junk_dir(tree: &Tree, id: usize) -> Option<(&'static str, Safety)> {
    use Safety::*;
    let n = &tree.nodes[id];
    let parent_name = n.parent.map(|p| tree.nodes[p].name.as_str()).unwrap_or("");
    let sibling = |name: &str| {
        n.parent.is_some_and(|p| {
            tree.nodes[p]
                .children
                .iter()
                .any(|&c| tree.nodes[c].name == name)
        })
    };
    Some(match n.name.as_str() {
        "node_modules" => ("JavaScript dependencies; `npm install` restores them", Safe),
        "target" if sibling("Cargo.toml") => {
            ("Rust build output; `cargo build` recreates it", Safe)
        }
        "__pycache__" | ".pytest_cache" | ".mypy_cache" | ".ruff_cache" => {
            ("Python tool cache; regenerated automatically", Safe)
        }
        ".tox" => ("tox environments; recreated on demand", Safe),
        ".next" | ".nuxt" | ".parcel-cache" => ("Web framework build cache", Safe),
        ".npm" => ("npm download cache", Safe),
        ".cache" => ("Application cache directory", Safe),
        "Caches" if parent_name == "Library" => ("macOS application caches", Safe),
        "DerivedData" => ("Xcode build products and indexes", Safe),
        "registry" if parent_name == ".cargo" => ("Cargo crate download cache", Safe),
        ".gradle" => (
            "Gradle caches and wrapper downloads; re-downloaded when needed",
            Review,
        ),
        ".Trash" | "Trash" => ("Already in the Trash; deleting it frees this space", Safe),
        ".venv" | "venv" => ("Python virtualenv; recreate from requirements", Review),
        _ => return None,
    })
}

const ARCHIVES: &[&str] = &[
    "iso", "dmg", "zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "pkg", "exe", "msi", "deb",
    "rpm",
];

pub fn compute(tree: &Tree, dupes: &[Vec<usize>]) -> Vec<Suggestion> {
    let mut out = Vec::new();
    let mut stack = vec![0usize];
    while let Some(id) = stack.pop() {
        for &c in &tree.nodes[id].children {
            let n = &tree.nodes[c];
            if n.is_dir {
                if n.size >= 10 * MB {
                    if let Some((why, safety)) = junk_dir(tree, c) {
                        out.push(Suggestion {
                            id: c,
                            title: n.name.clone(),
                            reason: why.into(),
                            size: n.size,
                            safety,
                        });
                        continue; // don't also report things inside it
                    }
                }
                stack.push(c);
                continue;
            }
            let ext = tree.ext_names[n.ext as usize].as_str();
            let age = age_days(n.mtime);
            let hit = if (n.name == "core" || n.name.starts_with("core.")) && n.size >= 10 * MB {
                Some(("Crash core dump", Safety::Safe))
            } else if ext == "log" && n.size >= 20 * MB {
                Some(("Large log file", Safety::Review))
            } else if ARCHIVES.contains(&ext) && n.size >= 50 * MB {
                Some((
                    "Archive/installer; often already extracted or installed",
                    Safety::Review,
                ))
            } else if n.size >= 200 * MB && age.is_some_and(|d| d > 180) {
                Some(("Large file untouched for over 6 months", Safety::Review))
            } else if n.size >= GB {
                Some(("Very large file", Safety::Review))
            } else {
                None
            };
            if let Some((why, safety)) = hit {
                let reason = match age {
                    Some(d) if d > 30 => format!("{why} (modified {d} days ago)"),
                    _ => why.to_string(),
                };
                out.push(Suggestion {
                    id: c,
                    title: n.name.clone(),
                    reason,
                    size: n.size,
                    safety,
                });
            }
        }
    }

    for group in dupes {
        let mut live: Vec<usize> = group
            .iter()
            .copied()
            .filter(|&i| tree.is_alive(i) && !under_junk(tree, i))
            .collect();
        if live.len() < 2 {
            continue;
        }
        // Keep the copy with the shortest path (usually the "original").
        live.sort_by_key(|&i| tree.path(i).as_os_str().len());
        let keep = tree.path(live[0]);
        for &i in &live[1..] {
            out.push(Suggestion {
                id: i,
                title: tree.nodes[i].name.clone(),
                reason: format!("Identical to {}", keep.display()),
                size: tree.nodes[i].size,
                safety: Safety::Review,
            });
        }
    }

    out.sort_by(|a, b| b.size.cmp(&a.size));
    out
}

fn under_junk(tree: &Tree, id: usize) -> bool {
    let mut cur = tree.nodes[id].parent;
    while let Some(p) = cur {
        if tree.nodes[p].is_dir && junk_dir(tree, p).is_some() {
            return true;
        }
        cur = tree.nodes[p].parent;
    }
    false
}
