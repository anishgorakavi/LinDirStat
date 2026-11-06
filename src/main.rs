mod scan;
mod suggest;
mod treemap;
mod util;

use std::path::PathBuf;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::futures::channel::oneshot;
use iced::widget::{
    button, canvas, column, container, progress_bar, row, scrollable, text, text_input, Column,
};
use iced::{Color, Element, Fill, Subscription, Task, Theme};

use scan::{Progress, Tree};
use suggest::{Safety, Suggestion};
use util::{count, human};

const DEFAULT_DEPTH: u32 = 3;
const MAX_THREADS: usize = 64;
/// Sidebar rows shown per folder; more rows make every redraw slower.
const ROW_LIMIT: usize = 150;

fn main() -> iced::Result {
    iced::application(App::new, App::update, App::view)
        .title("LinDirStat")
        .subscription(App::subscription)
        .theme(|_: &App| Theme::Dark)
        .window_size((1280.0, 800.0))
        .centered()
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Folder,
    Types,
    Suggestions,
}

#[derive(Debug, Clone)]
pub enum Message {
    PathChanged(String),
    Scan,
    Browse,
    Picked(Option<PathBuf>),
    Tick,
    DupesFound(u64, Vec<Vec<usize>>),
    Select(usize),
    /// Select a node and navigate the treemap so it is visible.
    Show(usize),
    /// Zoom the treemap into a folder.
    Enter(usize),
    Up,
    SetDepth(u32),
    /// Read a not-yet-scanned folder (`scan::ALL` = everything below it).
    Expand(usize, u32),
    SetThreads(usize),
    SetTab(Tab),
    AskDelete(Vec<usize>, bool),
    ConfirmDelete,
    CancelDelete,
    Deleted(Vec<(usize, Result<(), String>)>),
    Reveal(usize),
}

struct Scanning {
    progress: Arc<Progress>,
    started: Instant,
    last_ext: Instant,
}

struct PendingDelete {
    ids: Vec<usize>,
    permanent: bool,
}

struct App {
    path_input: String,
    depth: u32,
    threads: usize,
    tree: Option<Tree>,
    view_root: usize,
    selected: Option<usize>,
    tab: Tab,
    scanning: Option<Scanning>,
    scan_gen: u64,
    rev: u64,
    suggestions: Vec<Suggestion>,
    dupe_groups: Vec<Vec<usize>>,
    finding_dupes: bool,
    pending: Option<PendingDelete>,
    deleting: bool,
    status: String,
    last_click: Option<(usize, Instant)>,
}

impl App {
    fn new() -> (Self, Task<Message>) {
        // `lindirstat [folder]`; defaults to the home directory, which works
        // on machines where `/` is not readable.
        let mut start = None;
        let mut depth = DEFAULT_DEPTH;
        let mut threads = std::thread::available_parallelism().map_or(4, |n| n.get());
        let mut args = std::env::args().skip(1);
        while let Some(a) = args.next() {
            match a.as_str() {
                "-d" | "--depth" => {
                    depth = match args.next().as_deref() {
                        Some("all") => scan::ALL,
                        Some(n) => n.parse().unwrap_or(DEFAULT_DEPTH).max(1),
                        None => depth,
                    }
                }
                "-j" | "--threads" => {
                    if let Some(n) = args.next().and_then(|n| n.parse::<usize>().ok()) {
                        threads = n.clamp(1, MAX_THREADS);
                    }
                }
                _ => start = Some(PathBuf::from(a)),
            }
        }
        let start = start.unwrap_or_else(util::home_dir);
        let app = App {
            path_input: start.display().to_string(),
            depth,
            threads,
            tree: None,
            view_root: 0,
            selected: None,
            tab: Tab::Folder,
            scanning: None,
            scan_gen: 0,
            rev: 0,
            suggestions: Vec::new(),
            dupe_groups: Vec::new(),
            finding_dupes: false,
            pending: None,
            deleting: false,
            status: String::new(),
            last_click: None,
        };
        (app, Task::done(Message::Scan))
    }

    fn subscription(&self) -> Subscription<Message> {
        if self.scanning.is_some() {
            iced::time::every(Duration::from_millis(250)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        }
    }

    fn refresh_suggestions(&mut self) {
        if let Some(t) = &self.tree {
            self.suggestions = suggest::compute(t, &self.dupe_groups);
        }
    }

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::PathChanged(s) => self.path_input = s,
            Message::Tick => return self.pump(),
            Message::Browse => {
                let start = util::expand_path(&self.path_input);
                return Task::perform(
                    async move {
                        rfd::AsyncFileDialog::new()
                            .set_directory(start)
                            .pick_folder()
                            .await
                            .map(|h| h.path().to_path_buf())
                    },
                    Message::Picked,
                );
            }
            Message::Picked(Some(p)) => {
                self.path_input = p.display().to_string();
                return self.update(Message::Scan);
            }
            Message::Picked(None) => {}
            Message::Scan => {
                if let Some(s) = &self.scanning {
                    s.progress.cancel.store(true, Relaxed);
                }
                let root = util::expand_path(&self.path_input);
                let root = std::fs::canonicalize(&root).unwrap_or(root);
                self.scan_gen += 1;
                self.scanning = None;
                self.finding_dupes = false;
                self.pending = None;
                self.status.clear();
                self.view_root = 0;
                self.selected = None;
                self.dupe_groups.clear();
                self.suggestions.clear();
                self.rev += 1;
                match Tree::new(&root) {
                    Ok(tree) => {
                        self.tree = Some(tree);
                        return self.begin_scan(0, self.depth);
                    }
                    Err(e) => {
                        self.tree = None;
                        self.status = format!("Scan failed: {e}");
                    }
                }
            }
            Message::SetThreads(n) => self.threads = n.clamp(1, MAX_THREADS),
            Message::DupesFound(gen, groups) => {
                if gen == self.scan_gen {
                    self.finding_dupes = false;
                    self.dupe_groups = groups;
                    self.refresh_suggestions();
                }
            }
            Message::Select(id) => {
                let Some(tree) = &self.tree else {
                    return Task::none();
                };
                let dbl = matches!(self.last_click,
                    Some((i, t)) if i == id && t.elapsed() < Duration::from_millis(400));
                self.last_click = Some((id, Instant::now()));
                self.selected = Some(id);
                self.pending = None;
                if dbl {
                    let target = if tree.nodes[id].is_dir {
                        Some(id)
                    } else {
                        tree.nodes[id].parent
                    };
                    if let Some(t) = target {
                        self.view_root = t;
                        if !tree.nodes[t].scanned {
                            return self.update(Message::Expand(t, self.depth));
                        }
                    }
                }
            }
            Message::SetDepth(d) => {
                if d != self.depth {
                    self.depth = d;
                    return self.update(Message::Scan);
                }
            }
            Message::Expand(id, depth) => {
                if self.scanning.is_some() {
                    return Task::none();
                }
                let Some(tree) = &mut self.tree else {
                    return Task::none();
                };
                tree.detach_children(id);
                tree.live = true;
                self.rev += 1;
                return self.begin_scan(id, depth);
            }
            Message::Show(id) => {
                let Some(tree) = &self.tree else {
                    return Task::none();
                };
                self.selected = Some(id);
                self.pending = None;
                if let Some(p) = tree.nodes[id].parent {
                    self.view_root = p;
                }
            }
            Message::Enter(id) => {
                self.view_root = id;
                if self.tree.as_ref().is_some_and(|t| !t.nodes[id].scanned) {
                    return self.update(Message::Expand(id, self.depth));
                }
            }
            Message::Up => {
                if let Some(p) = self
                    .tree
                    .as_ref()
                    .and_then(|t| t.nodes[self.view_root].parent)
                {
                    self.view_root = p;
                }
            }
            Message::SetTab(t) => self.tab = t,
            Message::Reveal(id) => {
                if let Some(t) = &self.tree {
                    util::reveal(&t.path(id));
                }
            }
            Message::AskDelete(ids, permanent) => {
                let ids: Vec<usize> = ids.into_iter().filter(|&i| i != 0).collect();
                if !ids.is_empty() {
                    self.pending = Some(PendingDelete { ids, permanent });
                }
            }
            Message::CancelDelete => self.pending = None,
            Message::ConfirmDelete => {
                let (Some(p), Some(tree)) = (self.pending.take(), &self.tree) else {
                    return Task::none();
                };
                let jobs: Vec<(usize, PathBuf)> =
                    p.ids.iter().map(|&i| (i, tree.path(i))).collect();
                let permanent = p.permanent;
                self.deleting = true;
                self.status = "Deleting…".into();
                let (tx, rx) = oneshot::channel();
                std::thread::spawn(move || {
                    let res = jobs
                        .into_iter()
                        .map(|(i, path)| (i, util::delete_path(&path, permanent)))
                        .collect();
                    let _ = tx.send(res);
                });
                return Task::perform(
                    async move { rx.await.unwrap_or_default() },
                    Message::Deleted,
                );
            }
            Message::Deleted(results) => {
                self.deleting = false;
                let Some(tree) = &mut self.tree else {
                    return Task::none();
                };
                let (mut freed, mut ok, mut failed) = (0u64, 0usize, Vec::new());
                for (id, r) in results {
                    match r {
                        Ok(()) => {
                            if tree.is_alive(id) {
                                freed += tree.nodes[id].size;
                                tree.remove(id);
                            }
                            ok += 1;
                        }
                        Err(e) => failed.push(format!("{}: {e}", tree.nodes[id].name)),
                    }
                }
                tree.recompute_exts();
                if !tree.is_alive(self.view_root) {
                    self.view_root = 0;
                }
                if self.selected.is_some_and(|s| !tree.is_alive(s)) {
                    self.selected = None;
                }
                self.rev += 1;
                self.status = format!("Deleted {ok} item(s), freed {}", human(freed));
                if !failed.is_empty() {
                    self.status += &format!(" — failed: {}", failed.join("; "));
                }
                self.refresh_suggestions();
            }
        }
        Task::none()
    }

    /// Starts the worker threads on folder `id`; the tree fills in as they report.
    fn begin_scan(&mut self, id: usize, depth: u32) -> Task<Message> {
        let Some(tree) = &self.tree else {
            return Task::none();
        };
        let progress = Arc::new(Progress::default());
        if let Err(e) = scan::start(tree, id, depth, self.threads, progress.clone()) {
            self.status = e;
            return Task::none();
        }
        let now = Instant::now();
        self.scanning = Some(Scanning {
            progress,
            started: now,
            last_ext: now,
        });
        Task::none()
    }

    /// Merges finished directories into the tree; wraps up when the scan is complete.
    fn pump(&mut self) -> Task<Message> {
        let (Some(sc), Some(tree)) = (&mut self.scanning, &mut self.tree) else {
            return Task::none();
        };
        // Read `done` first: if it was already set, nothing new can arrive after the drain.
        let done = sc.progress.done.load(Relaxed);
        if scan::drain(tree, &sc.progress) {
            self.rev += 1;
        }
        if !done {
            if sc.last_ext.elapsed() > Duration::from_secs(2) {
                tree.recompute_exts();
                sc.last_ext = Instant::now();
            }
            return Task::none();
        }
        let errors = sc.progress.errors.load(Relaxed);
        self.scanning = None;
        tree.finish(errors);
        self.rev += 1;

        let mut candidates = Vec::new();
        let mut stack = vec![0usize];
        while let Some(id) = stack.pop() {
            for &c in &tree.nodes[id].children {
                let n = &tree.nodes[c];
                if n.is_dir {
                    stack.push(c);
                } else if n.size >= 1 << 20 {
                    candidates.push((c, tree.path(c), n.size));
                }
            }
        }
        self.refresh_suggestions();
        if candidates.len() < 2 {
            return Task::none();
        }
        let gen = self.scan_gen;
        self.finding_dupes = true;
        let (tx, rx) = oneshot::channel();
        std::thread::spawn(move || {
            let _ = tx.send(scan::find_duplicates(candidates));
        });
        Task::perform(async move { rx.await.unwrap_or_default() }, move |g| {
            Message::DupesFound(gen, g)
        })
    }

    // ---------- views ----------

    fn view(&self) -> Element<'_, Message> {
        let can_up = self
            .tree
            .as_ref()
            .is_some_and(|t| t.nodes[self.view_root].parent.is_some());
        let top = row![
            button("↑ Up").on_press_maybe(can_up.then_some(Message::Up)),
            text_input("Folder to scan, e.g. ~ or /home/me", &self.path_input)
                .on_input(Message::PathChanged)
                .on_submit(Message::Scan),
            button("Browse…").on_press(Message::Browse),
            text("Threads"),
            button("−").on_press(Message::SetThreads(self.threads.saturating_sub(1))),
            text(self.threads.to_string())
                .width(28)
                .align_x(iced::alignment::Horizontal::Center),
            button("+").on_press(Message::SetThreads(self.threads + 1)),
            text("Depth"),
            button("−").on_press(Message::SetDepth(if self.depth == scan::ALL {
                8
            } else {
                self.depth.saturating_sub(1).max(1)
            })),
            text(if self.depth == scan::ALL {
                "all".to_string()
            } else {
                self.depth.to_string()
            })
            .width(28)
            .align_x(iced::alignment::Horizontal::Center),
            button("+").on_press_maybe((self.depth != scan::ALL).then(|| Message::SetDepth(
                if self.depth >= 12 {
                    scan::ALL
                } else {
                    self.depth + 1
                }
            ))),
            button(if self.tree.is_some() || self.scanning.is_some() {
                "Rescan"
            } else {
                "Scan"
            })
            .on_press(Message::Scan),
        ]
        .spacing(8)
        .align_y(iced::Center);

        let body: Element<'_, Message> = match &self.tree {
            Some(tree) => {
                let right = column![
                    canvas(treemap::Treemap {
                        tree,
                        root: self.view_root,
                        rev: self.rev,
                        selected: self.selected,
                    })
                    .width(Fill)
                    .height(Fill),
                    self.details(tree),
                ]
                .spacing(8);
                row![self.sidebar(tree), right.width(Fill)]
                    .spacing(10)
                    .height(Fill)
                    .into()
            }
            None => container(text(if self.scanning.is_some() {
                "Scanning…"
            } else {
                "Choose a folder and press Scan."
            }))
            .center(Fill)
            .into(),
        };

        container(column![top, body, self.status_bar()].spacing(8))
            .padding(10)
            .into()
    }

    fn status_bar(&self) -> Element<'_, Message> {
        let s = if let Some(sc) = &self.scanning {
            let p = &sc.progress;
            let cur = p.current.lock().map(|g| g.clone()).unwrap_or_default();
            format!(
                "Scanning… {} files · {} · {} unreadable · {:.0}s — {cur}",
                count(p.files.load(Relaxed)),
                human(p.bytes.load(Relaxed)),
                p.errors.load(Relaxed),
                sc.started.elapsed().as_secs_f32(),
            )
        } else if !self.status.is_empty() {
            self.status.clone()
        } else if let Some(t) = &self.tree {
            let r = &t.nodes[0];
            format!(
                "{}: {}{} in {} files{} — showing {}",
                t.root.display(),
                if r.partial { "≥ " } else { "" },
                human(r.size),
                count(r.files),
                format!(
                    "{}{}",
                    if t.errors > 0 {
                        format!(" ({} folders unreadable)", t.errors)
                    } else {
                        String::new()
                    },
                    if t.unscanned > 0 {
                        format!(" · {} folders not scanned, so sizes marked ≥ are lower bounds: double-click a folder, raise Depth, or use Scan fully", t.unscanned)
                    } else {
                        String::new()
                    },
                ),
                t.path(self.view_root).display(),
            )
        } else {
            String::new()
        };
        text(s).size(13).wrapping(text::Wrapping::None).into()
    }

    fn sidebar<'a>(&'a self, tree: &'a Tree) -> Element<'a, Message> {
        let tab_btn = |label: String, tab: Tab| {
            button(text(label).size(14))
                .on_press(Message::SetTab(tab))
                .style(if self.tab == tab {
                    button::primary
                } else {
                    button::secondary
                })
        };
        let tabs = row![
            tab_btn("Folder".into(), Tab::Folder),
            tab_btn("File types".into(), Tab::Types),
            tab_btn(
                format!("Suggestions ({})", self.suggestions.len()),
                Tab::Suggestions
            ),
        ]
        .spacing(4);

        let content = match self.tab {
            Tab::Folder => self.folder_list(tree),
            Tab::Types => self.type_list(tree),
            Tab::Suggestions => self.suggestion_list(tree),
        };
        column![tabs, scrollable(content).height(Fill)]
            .spacing(8)
            .width(420)
            .into()
    }

    fn folder_list<'a>(&'a self, tree: &'a Tree) -> Element<'a, Message> {
        let root = &tree.nodes[self.view_root];
        let mut col = Column::new().spacing(2).padding([0, 12]);
        let mut kids = root.children.clone();
        kids.sort_by(|&a, &b| tree.nodes[b].size.cmp(&tree.nodes[a].size));
        for &c in kids.iter().take(ROW_LIMIT) {
            let n = &tree.nodes[c];
            let frac = if root.size > 0 {
                n.size as f32 / root.size as f32
            } else {
                0.0
            };
            col = col.push(
                button(
                    column![
                        row![
                            swatch(tree.color_of(c)),
                            text(format!("{}{}", if n.is_dir { "▸ " } else { "" }, n.name))
                                .size(13)
                                .wrapping(text::Wrapping::None)
                                .width(Fill),
                            text(if n.is_dir && !n.scanned {
                                String::new()
                            } else {
                                format!("{:.1}%", frac * 100.0)
                            })
                            .size(12),
                            text(tree.size_label(c))
                                .size(13)
                                .width(80)
                                .align_x(iced::alignment::Horizontal::Right),
                        ]
                        .spacing(6)
                        .align_y(iced::Center),
                        progress_bar(0.0..=1.0, if n.is_dir && !n.scanned { 0.0 } else { frac })
                            .girth(4),
                    ]
                    .spacing(3),
                )
                .on_press(Message::Select(c))
                .width(Fill)
                .padding(5)
                .style(if self.selected == Some(c) {
                    button::primary
                } else {
                    button::text
                }),
            );
        }
        if root.children.is_empty() {
            col = col.push(text("(empty)"));
        } else if kids.len() > ROW_LIMIT {
            col = col.push(
                text(format!(
                    "… {} more, see the treemap",
                    kids.len() - ROW_LIMIT
                ))
                .size(12),
            );
        }
        col.into()
    }

    fn type_list<'a>(&'a self, tree: &'a Tree) -> Element<'a, Message> {
        let mut order: Vec<usize> = (0..tree.ext_stats.len())
            .filter(|&i| tree.ext_stats[i].1 > 0)
            .collect();
        order.sort_by(|&a, &b| tree.ext_stats[b].0.cmp(&tree.ext_stats[a].0));
        let total = tree.nodes[0].size.max(1) as f32;
        let mut col = Column::new().spacing(6).padding([0, 12]);
        for i in order.into_iter().take(60) {
            let (bytes, files) = tree.ext_stats[i];
            col = col.push(
                column![
                    row![
                        swatch(tree.ext_color[i]),
                        text(format!(".{}", tree.ext_names[i])).size(13).width(Fill),
                        text(format!("{} files", count(files))).size(12),
                        text(human(bytes))
                            .size(13)
                            .width(70)
                            .align_x(iced::alignment::Horizontal::Right),
                    ]
                    .spacing(6)
                    .align_y(iced::Center),
                    progress_bar(0.0..=1.0, bytes as f32 / total).girth(4),
                ]
                .spacing(3),
            );
        }
        col.into()
    }

    fn suggestion_list<'a>(&'a self, tree: &'a Tree) -> Element<'a, Message> {
        let mut col = Column::new().spacing(8).padding([0, 12]);
        let total: u64 = self.suggestions.iter().map(|s| s.size).sum();
        let safe: Vec<&Suggestion> = self
            .suggestions
            .iter()
            .filter(|s| s.safety == Safety::Safe)
            .collect();
        let safe_total: u64 = safe.iter().map(|s| s.size).sum();
        col = col.push(
            text(format!(
                "{} suggestions · up to {} reclaimable",
                self.suggestions.len(),
                human(total)
            ))
            .size(14),
        );
        if tree.unscanned > 0 {
            col = col.push(text(format!("{} folders beyond the depth limit were not scanned, so suggestions inside them are missing.", tree.unscanned)).size(12));
        }
        if self.finding_dupes {
            col = col.push(text("Still looking for duplicate files…").size(12));
        }
        if !safe.is_empty() {
            col = col.push(
                button(text(format!("Trash all “safe” items ({})", human(safe_total))).size(13))
                    .on_press(Message::AskDelete(
                        safe.iter().map(|s| s.id).collect(),
                        false,
                    )),
            );
        }
        for s in self.suggestions.iter().take(200) {
            let (label, color) = match s.safety {
                Safety::Safe => ("SAFE", Color::from_rgb(0.3, 0.8, 0.4)),
                Safety::Review => ("REVIEW", Color::from_rgb(0.95, 0.7, 0.2)),
            };
            col = col.push(
                container(
                    column![
                        row![
                            text(label).size(11).color(color),
                            text(&s.title)
                                .size(13)
                                .wrapping(text::Wrapping::None)
                                .width(Fill),
                            text(human(s.size)).size(13),
                        ]
                        .spacing(8),
                        text(&s.reason).size(12),
                        text(tree.path(s.id).display().to_string())
                            .size(11)
                            .wrapping(text::Wrapping::None),
                        row![
                            button(text("Show").size(12)).on_press(Message::Show(s.id)),
                            button(text("Move to Trash").size(12))
                                .on_press(Message::AskDelete(vec![s.id], false)),
                        ]
                        .spacing(6),
                    ]
                    .spacing(4),
                )
                .padding(8)
                .style(container::rounded_box)
                .clip(true),
            );
        }
        col.into()
    }

    fn details<'a>(&'a self, tree: &'a Tree) -> Element<'a, Message> {
        let mut col = Column::new().spacing(6);
        if let Some(p) = &self.pending {
            let names: Vec<&str> = p
                .ids
                .iter()
                .take(3)
                .map(|&i| tree.nodes[i].name.as_str())
                .collect();
            let size: u64 = p.ids.iter().map(|&i| tree.nodes[i].size).sum();
            let what = if p.ids.len() > 3 {
                format!("{}, … ({} items)", names.join(", "), p.ids.len())
            } else {
                names.join(", ")
            };
            col = col.push(text(format!(
                "{} {} ({})? {}",
                if p.permanent {
                    "PERMANENTLY delete"
                } else {
                    "Move to Trash:"
                },
                what,
                human(size),
                if p.permanent {
                    "This cannot be undone."
                } else {
                    ""
                }
            )));
            col = col.push(
                row![
                    button("Confirm")
                        .on_press_maybe((!self.deleting).then_some(Message::ConfirmDelete))
                        .style(button::danger),
                    button("Cancel")
                        .on_press(Message::CancelDelete)
                        .style(button::secondary),
                ]
                .spacing(8),
            );
        } else if let Some(id) = self.selected {
            let n = &tree.nodes[id];
            let parent_size = n
                .parent
                .map(|p| tree.nodes[p].size)
                .unwrap_or(n.size)
                .max(1);
            col = col.push(
                text(tree.path(id).display().to_string())
                    .size(14)
                    .wrapping(text::Wrapping::None),
            );
            col = col.push(
                text(format!(
                    "{} ({} bytes allocated) · {:.1}% of parent · {}modified {}",
                    tree.size_label(id),
                    count(n.size),
                    n.size as f64 * 100.0 / parent_size as f64,
                    if n.is_dir {
                        format!("{} files · ", count(n.files))
                    } else {
                        String::new()
                    },
                    util::ago(n.mtime),
                ))
                .size(13),
            );
            let mut actions = row![].spacing(8);
            let idle = self.scanning.is_none();
            if let Some(p) = n.parent {
                actions = actions.push(
                    button("Parent folder")
                        .on_press(Message::Select(p))
                        .style(button::secondary),
                );
            }
            if n.is_dir && !n.scanned {
                actions = actions
                    .push(button("Scan this folder").on_press_maybe(
                        (self.scanning.is_none()).then(|| Message::Expand(id, self.depth)),
                    ))
                    .push(
                        button("Scan fully")
                            .on_press_maybe(
                                (self.scanning.is_none()).then(|| Message::Expand(id, scan::ALL)),
                            )
                            .style(button::secondary),
                    );
            } else if n.is_dir && id != self.view_root {
                actions = actions.push(
                    button("Open in map")
                        .on_press(Message::Enter(id))
                        .style(button::secondary),
                );
            }
            actions =
                actions
                    .push(
                        button("Reveal")
                            .on_press(Message::Reveal(id))
                            .style(button::secondary),
                    )
                    .push(button("Move to Trash").on_press_maybe(
                        (id != 0 && idle).then(|| Message::AskDelete(vec![id], false)),
                    ))
                    .push(
                        button("Delete permanently")
                            .on_press_maybe(
                                (id != 0 && idle).then(|| Message::AskDelete(vec![id], true)),
                            )
                            .style(button::danger),
                    );
            col = col.push(actions);
        } else {
            col = col.push(
                text(
                    "Click a block or list item to inspect it. Double-click to zoom into a folder.",
                )
                .size(13),
            );
        }
        container(col)
            .padding(8)
            .style(container::rounded_box)
            .width(Fill)
            .into()
    }
}

fn swatch<'a>(color: Color) -> Element<'a, Message> {
    container("")
        .width(10)
        .height(10)
        .style(move |_| container::Style {
            background: Some(color.into()),
            ..Default::default()
        })
        .into()
}
