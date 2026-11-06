# TREE

Iced desktop app that scans a folder, draws a disk-usage treemap, suggests what to delete, and deletes from inside the app.

Cargo.toml        Dependencies and the size-optimised release profile.
src/              Application code.
  main.rs         Entry point, CLI flags, app state, update loop (scan pumping, selection, deletion) and all widgets.
  scan.rs         Multithreaded breadth-first scanner that fills the tree live; the tree/node model, per-extension colours, duplicate finder, and scanner tests.
  treemap.rs      Squarified treemap layout and the canvas widget that draws it and handles hover and clicks.
  suggest.rs      Rules that turn the scanned tree into SAFE/REVIEW deletion suggestions.
  util.rs         Size/age formatting, `~` expansion, reveal-in-file-manager, trash/permanent delete.
