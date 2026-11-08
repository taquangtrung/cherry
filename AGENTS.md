# Cherry: Agent Guide

## Source

Cherry is built from scratch, slice by slice and test first. `~/Workspace/apps/snow-editor` (the editing core) is a reference only: read it to learn what a behavior should be, never copy its code.

## Library rules

- Cherry never depends on a window toolkit. A host converts its own key events into `cherry::keys` keys and paints the rows Cherry produces. Toolkit adapters live in examples or in the host, never in `cherry` itself.
- No async runtime and no global state in library crates. Long work is the host's business; Cherry exposes what to do, not a thread to do it on.
- Cherry never calls into the OS. What it needs answered on the spot (the clipboard, completions, highlighting) is a small trait the host implements; what it asks the host to do (save, open a file, quit, show a notice) is a `HostRequest` the host takes from the queue.
- Library crates use `thiserror` for errors; classify errors by variant, never by message text.
- Every public item is documented (`missing_docs` is denied); docs are written for someone who has never seen the apps this came from.
- Keep the public API small: an item is public only when a consumer needs it.

## Checks

Decide what to run by the risk of the change:

- Cosmetic edit (docs, comments, renames inside one function): run nothing.
- Structural edit (moving modules, changing signatures): `cargo check` right away.
- Behavioral edit: `cargo test -p <crate>` for the crate touched.
- Before calling a task done or committing: `make check` and `make test`.

## Commits

Semantic one-line messages: `<type>(<scope>): <description>` (`feat`, `fix`, `refactor`, `docs`, `test`, `build`, `ci`, `chore`).
