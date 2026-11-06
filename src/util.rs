use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

pub fn count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn age_days(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs() / 86_400)
}

pub fn ago(t: Option<SystemTime>) -> String {
    match age_days(t) {
        None => "unknown".into(),
        Some(0) => "today".into(),
        Some(1) => "yesterday".into(),
        Some(d) if d < 60 => format!("{d} days ago"),
        Some(d) if d < 730 => format!("{} months ago", d / 30),
        Some(d) => format!("{} years ago", d / 365),
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Expands a leading `~` so users can type `~` or `~/projects` in the path box.
pub fn expand_path(s: &str) -> PathBuf {
    let s = s.trim();
    if s == "~" {
        home_dir()
    } else if let Some(rest) = s.strip_prefix("~/").or_else(|| s.strip_prefix("~\\")) {
        home_dir().join(rest)
    } else {
        PathBuf::from(s)
    }
}

pub fn reveal(path: &Path) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open")
        .arg(path.parent().unwrap_or(path))
        .spawn();
}

pub fn delete_path(path: &Path, permanent: bool) -> Result<(), String> {
    if permanent {
        let md = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if md.is_dir() {
            std::fs::remove_dir_all(path).map_err(|e| e.to_string())
        } else {
            std::fs::remove_file(path).map_err(|e| e.to_string())
        }
    } else {
        trash::delete(path)
            .map_err(|e| format!("{e} (try \"Delete permanently\" if there is no trash)"))
    }
}
