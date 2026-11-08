# AGENTS.md

LinDirStat: a WinDirStat-style disk usage visualizer in Rust + Iced 0.14 for Linux, macOS and
Windows. It scans a folder (default `~`, because `/` is often unreadable on shared machines),
draws a treemap, suggests files to delete, and deletes from the UI. User-facing docs are in
`README.md`; `TREE.md` maps the files.

## Commands

```sh
cargo run --release -- [-d N|all] [-j N] [folder]   # -d depth (default 3), -j threads (default = cores)
cargo test                                          # scanner, treemap and suggestion tests
cargo test --release -- --ignored --nocapture bench_home   # times a scan of $HOME at 1/4/10 threads
cargo fmt --check
```

There is no CI. Run `cargo fmt` and `cargo test` before finishing. `cargo clippy` currently reports
a handful of minor style warnings; do not add new ones.

## Architecture

- `src/scan.rs`: the core. `Tree` is an arena (`Vec<Node>`, ids are indices, root is `0`).
  `start()` spawns N worker threads over one shared queue; each worker reads one directory,
  pushes a `Listing` into a shared inbox, then queues that directory's subfolders. The UI calls
  `drain()` on a timer tick and merges listings into the tree, so the treemap fills in live.
  Also holds the duplicate finder (size, then 4 KB prefix hash, then full blake3).
- `src/treemap.rs`: squarified layout (`build`) plus the `canvas::Program` that draws it.
- `src/suggest.rs`: rules that turn the scanned tree into SAFE/REVIEW suggestions.
- `src/util.rs`: formatting, `~` expansion, reveal, trash/permanent delete.
- `src/main.rs`: app state, `update`, all widgets. `pump()` drives scanning; `begin_scan()` starts it.

## Invariants that are easy to break

- **Child ids are always greater than their parent's id.** `Tree::finish()` computes `partial`
  with a single reverse pass over ids and relies on this. Ids come from a shared atomic counter
  (`next_id`), assigned by the worker that reads the directory.
- **Publish a listing before queuing its child jobs** (`process()` in `scan.rs`). That ordering
  guarantees a parent reaches the UI before its children. Ids from different threads can still
  arrive out of order, which is why `apply()` grows the arena with placeholder nodes
  (`deleted: true`) and fills them in later.
- **Read `done` before `drain`**, never after (`pump()`): if `done` was already set, nothing new
  can arrive after the drain. Reading it afterwards can drop the last listings.
- **Directory `size` = own allocation + everything scanned below it.** Folders past the depth
  limit (`scanned == false`) contribute only their own allocation, so every ancestor total is a
  lower bound. `Node::partial` marks this; always display sizes through `Tree::size_label()` so
  lower bounds show as `≥ X`. Never present an unscanned total as exact.
- **Rescanning a folder that already has children must call `Tree::detach_children(id)` first**,
  otherwise sizes and file counts are double counted.
- **Sizes are allocated bytes** (`blocks * 512` on Unix, `len` elsewhere), matching `du`.
  Symlinks are skipped; hard links are counted once via a shared `(dev, ino)` set.
- **Deleting:** never allow deleting the scan root (id `0`); delete buttons are disabled while a
  scan runs; every delete goes through a confirm step. After deleting call `Tree::remove`,
  `recompute_exts`, bump `rev`, and refresh suggestions.
- **`rev` must be bumped whenever the tree changes**; the treemap layout cache is keyed on it.

## Performance rules (the UI was once very slow; these are why it is not)

- Iced rebuilds the entire view after every message. Do not send an app message for high-frequency
  events like mouse movement. Hover state lives inside the canvas (`treemap::State`) and only
  requests a redraw; the tooltip is drawn by the canvas.
- Keep the sidebar capped (`ROW_LIMIT` in `main.rs`). Hundreds of rows with progress bars make
  every redraw expensive.
- Keep the treemap to one fill per block plus outlines for folders; avoid per-block strokes,
  bevels or extra primitives. Text labels only on blocks big enough to hold them.
- During a scan the tick is 250 ms and the extension totals refresh at most every 2 s because
  `recompute_exts` walks the whole tree.
- Do not drop the default GPU renderer (wgpu with tiny-skia fallback). A tiny-skia-only build was
  tried to shrink the binary and made the UI slow. The binary is about 4-5 MB; the budget is 50 MB.

## Platform notes

- Developed and tested only on macOS. Anything Unix-specific must sit behind `cfg(unix)` /
  `cfg(target_os = ...)` with a fallback, as `alloc_size`, `first_seen`, `skip_dir` and `reveal` do.
- Iced 0.14 API: `iced::application(new, update, view)`, `canvas::Program::update` returns
  `Option<canvas::Action<_>>` (`publish`, `request_redraw`, `and_capture`). Older Iced examples
  online use different signatures.

## Verifying changes

- The test suite covers the scanner (including 1 vs many threads giving identical results, depth
  limits, expand/rescan), treemap geometry and suggestions. Add a test when you change scanning,
  sizing or suggestion rules.
- The GUI cannot be screenshotted from the agent sandbox. To check the app still starts, run
  `timeout 5 ./target/release/lindirstat -d 2 ~/Desktop`; exit code 124 means it ran until the
  timeout without crashing. Say plainly that visual behaviour was not checked.
- Do not scan `/` or large folders repeatedly while testing; a cold scan of a home directory can
  take minutes.

## Conventions

- Match the surrounding code. Spaces, not tabs; `cargo fmt` is the formatter.
- Comments explain why, not what. No commented-out code or leftover debug output.
- Keep `TREE.md` in sync in the same change that adds, removes, renames or repurposes a file.
- Minimal changes: no new dependencies or abstractions without a reason.

## Known gaps

- Linux and Windows are untested.
- Deleting a folder that was never scanned under-reports the freed space (its size is only its own
  allocation).
- Percentage bars and treemap weights for partially scanned folders are approximate; unscanned
  folders get a small fixed share of their parent so they stay clickable.
- No license file yet.
