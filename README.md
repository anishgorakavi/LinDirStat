# LinDirStat

A [WinDirStat](https://windirstat.net/)-style disk usage visualizer for Linux, macOS and Windows,
written in Rust with [Iced](https://iced.rs). It shows which files take the most space as a
treemap, suggests what is safe to delete, and lets you delete from inside the app.

It starts from any folder, so it works on shared machines where `/` is not readable: point it at
`~` and go.

## Features

- **Treemap:** every block is a file, sized by disk space and coloured by file type. Hover for the
  full path, size and modified date; click to select; double-click to zoom into a folder.
- **Sidebar:** folder contents ranked by size, per-extension totals, and deletion suggestions.
- **Suggestions:** flags regenerable folders (`node_modules`, Rust `target`, `__pycache__`,
  caches, Trash), core dumps, large logs, big installers and archives, large files untouched for
  six months, and exact duplicate files. Each is marked SAFE or REVIEW.
- **Delete from the app:** move to the Trash or delete permanently, always behind a confirmation.
  The scan root cannot be deleted.
- **Live, multithreaded scan:** a configurable number of worker threads read folders shallowest
  first, and the treemap fills in as folders finish.
- **Adjustable depth:** scan only the top N levels instead of walking everything, then expand
  individual folders on demand.

## Build and run

Requires a recent stable Rust toolchain.

```sh
cargo run --release                  # scan your home folder
cargo run --release -- ~/projects    # scan a specific folder
cargo run --release -- -d all -j 16 ~
```

| Flag | Meaning | Default |
|------|---------|---------|
| `-d`, `--depth N` | Levels to read (`1` = just the folder's contents), or `all` | `3` |
| `-j`, `--threads N` | Scanner threads (1-64) | number of CPU cores |

Depth and thread count can also be changed in the top bar. Changing depth rescans; changing
threads applies to the next scan. More threads help most on network filesystems.

## How sizes work

Sizes are allocated disk space (like `du`), not apparent file size. Symlinks are ignored and
hard links are counted once.

With a depth limit, folders past the limit are listed but never opened, so their size is unknown.
Every parent total that includes one is shown as a lower bound (`≥ 10.5 GB`). Double-click a
folder or use **Scan fully** to read it; once nothing below a folder is unread, its size is exact.
Suggestions and duplicate detection only cover folders that were read.

## Platform notes

- Developed and tested on macOS. Linux and Windows have not been tested yet; the code uses
  platform-specific paths only for allocated size, "Reveal in file manager" and skipping
  `/proc`, `/sys` and `/dev`.
- Moving to the Trash needs a desktop trash. If there is none, use **Delete permanently**.
- The Browse dialog on Linux needs a desktop portal; otherwise type the path.

## Tests

```sh
cargo test
cargo test --release -- --ignored --nocapture bench_home   # time a scan of your home folder
```

See [TREE.md](TREE.md) for a map of the source files.
