use iced::Color;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed, Ordering::SeqCst};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime};

use rayon::prelude::*;

pub const NO_EXT: u32 = u32::MAX;

#[derive(Default)]
pub struct Progress {
    pub files: AtomicU64,
    pub dirs: AtomicU64,
    pub bytes: AtomicU64,
    pub errors: AtomicU64,
    pub cancel: AtomicBool,
    pub current: Mutex<String>,
    /// Finished directory listings waiting for the UI to merge them into the tree.
    inbox: Mutex<Vec<Listing>>,
    /// Set once every queued directory has been read.
    pub done: AtomicBool,
    next_id: AtomicUsize,
    pending: AtomicUsize,
}

/// One directory's entries, with node ids pre-assigned by the worker.
struct Listing {
    parent: usize,
    first_id: usize,
    entries: Vec<RawEntry>,
}

struct RawEntry {
    name: String,
    is_dir: bool,
    size: u64,
    mtime: Option<SystemTime>,
}

#[derive(Debug)]
pub struct Node {
    pub name: String,
    pub parent: Option<usize>,
    pub is_dir: bool,
    /// Allocated size in bytes (recursive for directories).
    pub size: u64,
    pub files: u64,
    pub mtime: Option<SystemTime>,
    /// Sorted by size, descending. Deleted children are removed.
    pub children: Vec<usize>,
    pub ext: u32,
    pub deleted: bool,
    /// False for folders beyond the depth limit: their contents were never read.
    pub scanned: bool,
    /// True if this folder (or anything below it) was not scanned, so `size` is a lower bound.
    pub partial: bool,
}

#[derive(Debug)]
pub struct Tree {
    pub root: PathBuf,
    pub nodes: Vec<Node>,
    pub errors: u64,
    /// Folders in the tree whose contents have not been read.
    pub unscanned: u64,
    /// True while a scan is still filling the tree in.
    pub live: bool,
    pub ext_names: Vec<String>,
    ext_map: HashMap<String, u32>,
    pub ext_stats: Vec<(u64, u64)>, // (bytes, files)
    pub ext_color: Vec<Color>,
}

const PALETTE: [(u8, u8, u8); 12] = [
    (66, 133, 244),
    (234, 67, 53),
    (251, 188, 5),
    (52, 168, 83),
    (171, 71, 188),
    (0, 172, 193),
    (255, 112, 67),
    (141, 110, 99),
    (156, 204, 101),
    (236, 64, 122),
    (121, 134, 203),
    (38, 166, 154),
];
const OTHER: (u8, u8, u8) = (120, 124, 130);
pub const DIR_COLOR: Color = Color::from_rgb(0.35, 0.39, 0.47);

impl Tree {
    pub fn path(&self, id: usize) -> PathBuf {
        let mut parts = Vec::new();
        let mut cur = id;
        while let Some(p) = self.nodes[cur].parent {
            parts.push(&self.nodes[cur].name);
            cur = p;
        }
        let mut pb = self.root.clone();
        for n in parts.iter().rev() {
            pb.push(n);
        }
        pb
    }

    pub fn is_alive(&self, mut id: usize) -> bool {
        loop {
            if self.nodes[id].deleted {
                return false;
            }
            match self.nodes[id].parent {
                Some(p) => id = p,
                None => return true,
            }
        }
    }

    /// Detaches a node after it was deleted on disk and fixes ancestor totals.
    pub fn remove(&mut self, id: usize) {
        if id == 0 || !self.is_alive(id) {
            return;
        }
        let (size, files) = (self.nodes[id].size, self.nodes[id].files);
        self.nodes[id].deleted = true;
        let mut cur = self.nodes[id].parent;
        if let Some(p) = cur {
            self.nodes[p].children.retain(|&c| c != id);
        }
        while let Some(p) = cur {
            let n = &mut self.nodes[p];
            n.size = n.size.saturating_sub(size);
            n.files = n.files.saturating_sub(files);
            cur = n.parent;
        }
    }

    pub fn color_of(&self, id: usize) -> Color {
        let n = &self.nodes[id];
        if n.ext == NO_EXT {
            DIR_COLOR
        } else {
            self.ext_color
                .get(n.ext as usize)
                .copied()
                .unwrap_or_else(|| rgb(OTHER))
        }
    }

    /// Recomputes per-extension totals and colour ranking over live nodes.
    pub fn recompute_exts(&mut self) {
        let mut stats = vec![(0u64, 0u64); self.ext_names.len()];
        let mut unscanned = 0;
        let mut stack = vec![0usize];
        while let Some(id) = stack.pop() {
            for &c in &self.nodes[id].children {
                let n = &self.nodes[c];
                if n.is_dir {
                    if !n.scanned {
                        unscanned += 1;
                    }
                    stack.push(c);
                } else {
                    stats[n.ext as usize].0 += n.size;
                    stats[n.ext as usize].1 += 1;
                }
            }
        }
        let mut order: Vec<usize> = (0..stats.len()).collect();
        order.sort_by(|&a, &b| stats[b].0.cmp(&stats[a].0));
        let mut colors = vec![rgb(OTHER); stats.len()];
        for (rank, &i) in order.iter().enumerate().take(PALETTE.len()) {
            colors[i] = rgb(PALETTE[rank]);
        }
        self.unscanned = unscanned;
        self.ext_stats = stats;
        self.ext_color = colors;
    }
}

fn rgb((r, g, b): (u8, u8, u8)) -> Color {
    Color::from_rgb8(r, g, b)
}

/// Pass as `depth` to read everything.
pub const ALL: u32 = u32::MAX;

impl Tree {
    pub fn new(root: &Path) -> Result<Tree, String> {
        let md = fs::metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
        if !md.is_dir() {
            return Err(format!("{} is not a directory", root.display()));
        }
        Ok(Tree {
            root: root.to_path_buf(),
            nodes: vec![Node {
                name: root.display().to_string(),
                parent: None,
                is_dir: true,
                size: alloc_size(&md),
                files: 0,
                mtime: md.modified().ok(),
                children: Vec::new(),
                ext: NO_EXT,
                deleted: false,
                scanned: false,
                partial: true,
            }],
            errors: 0,
            unscanned: 0,
            live: true,
            ext_names: Vec::new(),
            ext_map: HashMap::new(),
            ext_stats: Vec::new(),
            ext_color: Vec::new(),
        })
    }

    fn intern(&mut self, file_name: &str) -> u32 {
        let ext = match file_name.rfind('.') {
            Some(i) if i > 0 && i + 1 < file_name.len() => file_name[i + 1..].to_lowercase(),
            _ => "(none)".to_string(),
        };
        if let Some(&i) = self.ext_map.get(&ext) {
            return i;
        }
        let i = self.ext_names.len() as u32;
        self.ext_names.push(ext.clone());
        self.ext_map.insert(ext, i);
        i
    }

    fn apply(&mut self, l: Listing) {
        let need = l.first_id + l.entries.len();
        if self.nodes.len() < need {
            // Ids come from several threads, so slots can arrive out of order.
            self.nodes.resize_with(need, || Node {
                name: String::new(),
                parent: None,
                is_dir: false,
                size: 0,
                files: 0,
                mtime: None,
                children: Vec::new(),
                ext: NO_EXT,
                deleted: true,
                scanned: true,
                partial: false,
            });
        }
        let (mut total, mut files) = (0u64, 0u64);
        for (i, e) in l.entries.into_iter().enumerate() {
            let id = l.first_id + i;
            let ext = if e.is_dir {
                NO_EXT
            } else {
                self.intern(&e.name)
            };
            total += e.size;
            files += !e.is_dir as u64;
            self.nodes[id] = Node {
                name: e.name,
                parent: Some(l.parent),
                is_dir: e.is_dir,
                size: e.size,
                files: !e.is_dir as u64,
                mtime: e.mtime,
                children: Vec::new(),
                ext,
                deleted: false,
                scanned: !e.is_dir,
                partial: false,
            };
            self.nodes[l.parent].children.push(id);
        }
        self.nodes[l.parent].scanned = true;
        let mut cur = Some(l.parent);
        while let Some(p) = cur {
            let n = &mut self.nodes[p];
            n.size += total;
            n.files += files;
            cur = n.parent;
        }
    }

    /// Forgets what was read below `id` so it can be scanned again.
    pub fn detach_children(&mut self, id: usize) {
        let kids = std::mem::take(&mut self.nodes[id].children);
        let removed: u64 = kids.iter().map(|&c| self.nodes[c].size).sum();
        let files = self.nodes[id].files;
        for c in kids {
            self.nodes[c].deleted = true;
        }
        self.nodes[id].scanned = false;
        let mut cur = Some(id);
        while let Some(p) = cur {
            let n = &mut self.nodes[p];
            n.size = n.size.saturating_sub(removed);
            n.files = n.files.saturating_sub(files);
            cur = n.parent;
        }
    }

    /// Settles derived data once a scan has delivered everything.
    pub fn finish(&mut self, errors: u64) {
        self.live = false;
        self.errors += errors;
        for n in &mut self.nodes {
            n.partial = !n.scanned && !n.deleted;
        }
        // Children always have larger ids than their parent.
        for id in (1..self.nodes.len()).rev() {
            if self.nodes[id].partial && !self.nodes[id].deleted {
                if let Some(p) = self.nodes[id].parent {
                    self.nodes[p].partial = true;
                }
            }
        }
        self.recompute_exts();
    }

    /// Size for display: exact, a lower bound (\"≥\"), or unknown.
    pub fn size_label(&self, id: usize) -> String {
        let n = &self.nodes[id];
        if n.is_dir && !n.scanned {
            if self.live {
                "scanning…".into()
            } else {
                "not scanned".into()
            }
        } else if n.partial || (self.live && n.is_dir) {
            format!("≥ {}", crate::util::human(n.size))
        } else {
            crate::util::human(n.size)
        }
    }
}

/// Merges whatever the workers have finished so far. Returns true if the tree changed.
pub fn drain(tree: &mut Tree, p: &Progress) -> bool {
    let batch = std::mem::take(&mut *p.inbox.lock().unwrap());
    let changed = !batch.is_empty();
    for l in batch {
        tree.apply(l);
    }
    changed
}

struct Job {
    id: usize,
    path: PathBuf,
    level: u32,
}

struct Queue {
    jobs: Mutex<VecDeque<Job>>,
    cv: Condvar,
}

fn alloc_size(md: &fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        md.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        md.len()
    }
}

/// Hard links share data; only count each inode once.
fn first_seen(md: &fs::Metadata, seen: &Mutex<HashSet<(u64, u64)>>) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if md.nlink() > 1 {
            return seen.lock().unwrap().insert((md.dev(), md.ino()));
        }
    }
    let _ = (md, seen);
    true
}

fn skip_dir(path: &Path) -> bool {
    #[cfg(unix)]
    {
        const SKIP: &[&str] = &[
            "/proc",
            "/sys",
            "/dev",
            #[cfg(target_os = "macos")]
            "/Volumes",
            #[cfg(target_os = "macos")]
            "/System/Volumes",
        ];
        return SKIP.iter().any(|s| path == Path::new(s));
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        false
    }
}

/// Reads one directory, publishes its listing, returns jobs for its subfolders.
fn process(job: &Job, p: &Progress, seen: &Mutex<HashSet<(u64, u64)>>, max: u32) -> Vec<Job> {
    p.dirs.fetch_add(1, Relaxed);
    if let Ok(mut g) = p.current.try_lock() {
        *g = job.path.display().to_string();
    }
    let mut entries = Vec::new();
    let mut subdirs = Vec::new(); // (index into entries, path)
    match fs::read_dir(&job.path) {
        Err(_) => {
            p.errors.fetch_add(1, Relaxed);
        }
        Ok(rd) => {
            for de in rd {
                let Ok(de) = de else {
                    p.errors.fetch_add(1, Relaxed);
                    continue;
                };
                let Ok(md) = de.metadata() else {
                    p.errors.fetch_add(1, Relaxed);
                    continue;
                };
                let ft = md.file_type();
                let name = de.file_name().to_string_lossy().into_owned();
                if ft.is_dir() {
                    let path = de.path();
                    if skip_dir(&path) {
                        continue;
                    }
                    if job.level + 1 < max {
                        subdirs.push((entries.len(), path));
                    }
                    entries.push(RawEntry {
                        name,
                        is_dir: true,
                        size: alloc_size(&md),
                        mtime: md.modified().ok(),
                    });
                } else if ft.is_file() {
                    let size = if first_seen(&md, seen) {
                        alloc_size(&md)
                    } else {
                        0
                    };
                    p.files.fetch_add(1, Relaxed);
                    p.bytes.fetch_add(size, Relaxed);
                    entries.push(RawEntry {
                        name,
                        is_dir: false,
                        size,
                        mtime: md.modified().ok(),
                    });
                }
                // Symlinks and special files are ignored so nothing is counted twice.
            }
        }
    }
    let first_id = p.next_id.fetch_add(entries.len(), SeqCst);
    let jobs = subdirs
        .into_iter()
        .map(|(i, path)| Job {
            id: first_id + i,
            path,
            level: job.level + 1,
        })
        .collect();
    // Publish before the child jobs exist, so parents always reach the UI first.
    p.inbox.lock().unwrap().push(Listing {
        parent: job.id,
        first_id,
        entries,
    });
    jobs
}

fn worker(p: &Progress, q: &Queue, seen: &Mutex<HashSet<(u64, u64)>>, max: u32) {
    loop {
        let job = {
            let mut jobs = q.jobs.lock().unwrap();
            loop {
                if p.cancel.load(Relaxed) {
                    break None;
                }
                // Front of the queue = shallowest folder, so the top levels fill in first.
                if let Some(j) = jobs.pop_front() {
                    break Some(j);
                }
                if p.pending.load(SeqCst) == 0 {
                    break None;
                }
                jobs =
                    q.cv.wait_timeout(jobs, Duration::from_millis(50))
                        .unwrap()
                        .0;
            }
        };
        let Some(job) = job else { return };
        let children = process(&job, p, seen, max);
        if !children.is_empty() {
            p.pending.fetch_add(children.len(), SeqCst);
            q.jobs.lock().unwrap().extend(children);
            q.cv.notify_all();
        }
        if p.pending.fetch_sub(1, SeqCst) == 1 {
            p.done.store(true, SeqCst);
            q.cv.notify_all();
        }
    }
}

/// Starts scanning folder node `id` (at `path`) on `threads` worker threads, down
/// to `depth` levels (1 = just its contents). Results arrive through `drain`.
pub fn start(
    tree: &Tree,
    id: usize,
    depth: u32,
    threads: usize,
    p: Arc<Progress>,
) -> Result<(), String> {
    p.next_id.store(tree.nodes.len(), SeqCst);
    p.pending.store(1, SeqCst);
    let q = Arc::new(Queue {
        jobs: Mutex::new(VecDeque::from([Job {
            id,
            path: tree.path(id),
            level: 0,
        }])),
        cv: Condvar::new(),
    });
    let seen = Arc::new(Mutex::new(HashSet::new()));
    let depth = depth.max(1);
    for _ in 0..threads.max(1) {
        let (p, q, seen) = (p.clone(), q.clone(), seen.clone());
        std::thread::Builder::new()
            .stack_size(1 << 20)
            .spawn(move || worker(&p, &q, &seen, depth))
            .map_err(|e| format!("could not start scan thread: {e}"))?;
    }
    Ok(())
}

/// Finds groups of identical files (>= 1 MiB). Returns node-id groups.
pub fn find_duplicates(candidates: Vec<(usize, PathBuf, u64)>) -> Vec<Vec<usize>> {
    use std::io::Read;
    let mut by_size: HashMap<u64, Vec<(usize, PathBuf)>> = HashMap::new();
    for (id, path, size) in candidates {
        by_size.entry(size).or_default().push((id, path));
    }
    let groups: Vec<Vec<(usize, PathBuf)>> =
        by_size.into_values().filter(|g| g.len() > 1).collect();

    fn hash(path: &Path, limit: Option<usize>) -> Option<[u8; 32]> {
        let mut f = fs::File::open(path).ok()?;
        let mut h = blake3::Hasher::new();
        let mut buf = vec![0u8; 1 << 16];
        let mut left = limit.unwrap_or(usize::MAX);
        while left > 0 {
            let want = buf.len().min(left);
            let n = f.read(&mut buf[..want]).ok()?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
            left -= n;
        }
        Some(*h.finalize().as_bytes())
    }

    groups
        .into_par_iter()
        .flat_map(|g| {
            // Cheap prefix hash first, full hash only for survivors.
            let mut by_prefix: HashMap<[u8; 32], Vec<(usize, PathBuf)>> = HashMap::new();
            for (id, path) in g {
                if let Some(h) = hash(&path, Some(4096)) {
                    by_prefix.entry(h).or_default().push((id, path));
                }
            }
            let mut out = Vec::new();
            for (_, cands) in by_prefix.into_iter().filter(|(_, v)| v.len() > 1) {
                let mut full: HashMap<[u8; 32], Vec<usize>> = HashMap::new();
                for (id, path) in cands {
                    if let Some(h) = hash(&path, None) {
                        full.entry(h).or_default().push(id);
                    }
                }
                out.extend(full.into_values().filter(|v| v.len() > 1));
            }
            out
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write(p: &Path, n: usize) {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::File::create(p)
            .unwrap()
            .write_all(&vec![7u8; n])
            .unwrap();
    }

    fn run(tree: &mut Tree, id: usize, depth: u32, threads: usize) {
        let p = Arc::new(Progress::default());
        start(tree, id, depth, threads, p.clone()).unwrap();
        loop {
            let done = p.done.load(SeqCst);
            drain(tree, &p);
            if done {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        tree.finish(p.errors.load(Relaxed));
    }

    fn scan(root: &Path, depth: u32, threads: usize) -> Tree {
        let mut t = Tree::new(root).unwrap();
        run(&mut t, 0, depth, threads);
        t
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("lds-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn scans_suggests_and_removes() {
        let dir = tmp("full");
        write(&dir.join("a/big.bin"), 3 << 20);
        write(&dir.join("a/copy.bin"), 3 << 20);
        write(&dir.join("proj/Cargo.toml"), 10);
        write(&dir.join("proj/target/junk.o"), 12 << 20);
        write(&dir.join("b/small.txt"), 100);

        for threads in [1, 4] {
            let mut tree = scan(&dir, ALL, threads);
            assert_eq!(tree.nodes[0].files, 5, "threads={threads}");
            assert!(tree.nodes[0].size >= (18 << 20));
            assert!(!tree.nodes[0].partial);

            let cands: Vec<_> = (0..tree.nodes.len())
                .filter(|&i| !tree.nodes[i].is_dir && tree.nodes[i].size >= 1 << 20)
                .map(|i| (i, tree.path(i), tree.nodes[i].size))
                .collect();
            let dupes = find_duplicates(cands);
            assert_eq!(dupes.len(), 1);
            assert_eq!(dupes[0].len(), 2);

            let sugg = crate::suggest::compute(&tree, &dupes);
            assert!(sugg.iter().any(|s| s.title == "target"), "{sugg:?}");
            assert!(sugg.iter().any(|s| s.reason.starts_with("Identical")));

            let before = tree.nodes[0].size;
            let t = sugg.iter().find(|s| s.title == "target").unwrap();
            tree.remove(t.id);
            assert_eq!(tree.nodes[0].size, before - t.size);
            assert_eq!(tree.nodes[0].files, 4);

            let cells = crate::treemap::build(&tree, 0, iced::Size::new(800.0, 600.0));
            assert!(cells.iter().any(|c| c.leaf));
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn many_dirs_many_threads_match_single_thread() {
        let dir = tmp("wide");
        for i in 0..40 {
            for j in 0..5 {
                write(&dir.join(format!("d{i}/s{j}/f.bin")), 1000 * (i + j + 1));
            }
        }
        let a = scan(&dir, ALL, 1);
        let b = scan(&dir, ALL, 8);
        assert_eq!(a.nodes[0].size, b.nodes[0].size);
        assert_eq!(a.nodes[0].files, 200);
        assert_eq!(b.nodes[0].files, 200);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn depth_limit_and_expand() {
        let dir = tmp("depth");
        write(&dir.join("top.bin"), 1 << 20);
        write(&dir.join("a/mid.bin"), 2 << 20);
        write(&dir.join("a/b/deep.bin"), 4 << 20);

        let mut tree = scan(&dir, 1, 2);
        assert_eq!(tree.nodes[0].files, 1, "only top-level files at depth 1");
        assert_eq!(tree.unscanned, 1);
        let a = tree.nodes[0]
            .children
            .iter()
            .copied()
            .find(|&c| tree.nodes[c].name == "a")
            .unwrap();
        assert!(!tree.nodes[a].scanned && tree.nodes[a].children.is_empty());

        tree.live = true;
        run(&mut tree, a, 1, 2);
        assert!(tree.nodes[a].scanned);
        assert_eq!(tree.nodes[0].files, 2);
        assert_eq!(tree.unscanned, 1, "a/b is now the unscanned one");
        assert!(tree.nodes[0].partial && tree.nodes[a].partial);

        // Rescanning an already-read folder must not double count.
        tree.detach_children(a);
        tree.live = true;
        run(&mut tree, a, ALL, 3);
        assert_eq!(tree.unscanned, 0);
        assert!(!tree.nodes[0].partial && !tree.nodes[a].partial);
        assert_eq!(tree.nodes[0].files, 3);
        let full = scan(&dir, ALL, 1);
        assert_eq!(tree.nodes[0].size, full.nodes[0].size);
        let _ = fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// `cargo test --release -- --ignored --nocapture bench_home`
    #[test]
    #[ignore]
    fn bench_home() {
        let root = crate::util::home_dir();
        for threads in [1, 4, 10] {
            let t0 = std::time::Instant::now();
            let mut tree = Tree::new(&root).unwrap();
            let p = Arc::new(Progress::default());
            start(&tree, 0, ALL, threads, p.clone()).unwrap();
            while !p.done.load(SeqCst) {
                drain(&mut tree, &p);
                std::thread::sleep(Duration::from_millis(5));
            }
            drain(&mut tree, &p);
            println!(
                "threads={threads:2}  {:6.1}s  {} files  {:.2} GB",
                t0.elapsed().as_secs_f32(),
                tree.nodes[0].files,
                tree.nodes[0].size as f64 / 1e9
            );
        }
    }
}
