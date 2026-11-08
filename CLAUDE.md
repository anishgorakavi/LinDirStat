@AGENTS.md

## Claude Code notes

- Read `AGENTS.md` above first; the scanner invariants and performance rules there are the parts
  most likely to be broken by a well-meaning change.
- Use the `personal-code-hygiene` skill when writing or reviewing code here (it enforces the
  `TREE.md` upkeep and comment rules listed in `AGENTS.md`).
- Do not try to look at the GUI: screen capture is blocked in this environment. Verify with
  `cargo test` and the `timeout 5` launch check, and tell the user visual behaviour is unchecked.
- Avoid long full-home scans in a foreground command; use `run_in_background` if you must time one.
- The repo is private on GitHub (`anishgorakavi/LinDirStat`, branch `main`). Commit or push only
  when asked.
