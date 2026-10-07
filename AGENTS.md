# Working on account-export

This is an independent Windows x64 Rust DLL project. Keep required build inputs
inside this directory and preserve existing work. For runtime changes, read
[docs/development.md](docs/development.md).

- Run `./check-source.ps1` and `./build.ps1` for code or packaging changes.
  Cargo.lock is source; Cargo builds use `--locked`.
- Resolve runtime symbols and field offsets by name. Verify callback signatures,
  inlining and identity transitions against native code before changing hooks.
- Export all server-confirmed tables, including unknown and empty tables. Success
  requires complete coverage, payment balance, final version confirmation and
  durable file readback. Retain failure visibility; close only after success.
- Keep export requests limited to authenticated confirm/pull and balance reads.
  Preserve the real sync queues and DB; pending pushes are never replayed.
- Store detours before enabling them; fence Rust panics at FFI boundaries and
  root managed objects across allocation and asynchronous work.
- Derive paths from the executable, repository or OS APIs. Keep project links
  local; builds and runtime must work without sibling checkouts.
- Keep binaries, keys, captures, generated data and build outputs untracked.
  Preserve path remapping, symbol exclusion and packaged-path scans. Package
  third-party notices with the DLL. Keep README minimal.
- Report automated checks separately from real-game login, export, shutdown and
  popup validation. An offline capture test does not certify a live export.
