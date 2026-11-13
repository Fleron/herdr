# Core-backed Herdr TUI

This fork preserves the regular Herdr terminal interface and uses `herdr-core`
for its runtime. The default `herdr` executable
connects through the installed `hc` command. macOS is the supported interactive
platform. Linux also builds and runs the real-core integration suite in CI;
Windows interactive support is not qualified.

## Runtime and saved arrangements

The familiar machine, repository and worktree sidebar, global agents panel,
tabs, splits, menus, settings and input controls remain client-owned.
Core owns terminal programs, process lifetime, terminal output and remote
connections. The TUI owns workspace, tab and pane arrangements. Closing a pane,
tab or workspace removes its saved views. Detaching leaves programs running.
The separate **Stop terminal** action asks for confirmation because it ends the
shared program for desktop and other viewers too.

New terminal programs are independent of the terminal hosting the TUI and remain
available after detach. Use **Attach existing terminal** to reopen a running
program. Native OSC 7501 reports supply agent status; programs without those
reports have unknown status.

Arrangements are saved in the `herdr-tui` core document on the machine that owns
their terminals. They do not change the desktop's arrangement document. The TUI
keeps selected workspace, tab and pane preferences in a local
`core-tui-view.json` file under its state directory. Conflicting document writes
retain pending edits for retry; the sidebar menu also offers an explicit discard
of unsaved arrangement changes.
Voluntary detach waits for pending saves, terminal placement and typing, or an
explicit discard. An approved checkout deletion finishes before detaching.

## Controls and configuration

Run `herdr --help` or `herdr --skill` for the supported interface. The default
prefix is Ctrl+B. Press prefix then `?` for the configured shortcuts. The sidebar
menu contains attach, retry, discard and confirmed Stop actions. Workspaces,
tabs, splits, focus, resize, zoom, rename,
scrollback, selection and copy are handled by the TUI. Worktree operations run
on the workspace's owning machine.

The TUI reads the existing Herdr configuration file. Run `herdr config check`
to validate it and `herdr --default-config` to print the shared configuration
template. Configured shell commands run through core on the selected machine;
configured pane commands open the usual temporary, zoomed command pane and restore
focus when it finishes. Their working directory follows the selected terminal.
Window titles, tab status, theme, mouse and copy preferences use the existing
configuration. Plugin actions, external tool popups, the external scrollback
editor, tab status commands, legacy automation API and updater are outside this
port's supported interface.

Remote clipboard images and dropped image files use private staging on their
owning machine. Unsent input waits for the transfer; changing the target holds
that input until explicit discard. Staging files are cleaned up on exit. If the
machine becomes unreachable, a cleanup failure reports any remaining paths.

Configure remotes with `hc machine add`; use `hc` for runtime commands and
machine management. The TUI uses those connections for remote terminals,
documents and worktree operations. It does not start the legacy Herdr server or
require its plugin API.

## Build

Use the Rust and Zig versions pinned by the repository's CI and vendored
terminal build. Install herdr-core 0.1.20, or a compatible generation-1 runtime,
and make `hc` available on PATH before opening the TUI.

```sh
cargo build --locked
cargo run --locked -- --help
cargo run --locked -- --default-config
cargo run --locked -- config check
cargo run --locked
```

The default Cargo binary is the core-backed TUI. The old implementation remains
an explicitly selected development example for existing regression tests:

```sh
cargo build --locked --example legacy-regression
```

That example retains the old server and CLI only for regression coverage. It is
not the default executable or a runtime dependency of the core-backed TUI.

## Core compatibility limits

Inline terminal images render live. Current core does not replay image payloads
that finished before attachment or reconnection. Kitty file and shared-memory
transfers cannot read a remote filesystem through the client; inline transfers
remain available. The port makes no changes to core.

Core 0.1.20 reports remote event ownership using aliases. Saved layouts and
terminal operations use permanent machine IDs, and Attach/Stop choices use the
profiles captured with their terminal listing. Avoid removing a remote and
reusing its alias while this TUI is open: event snapshots and separate profile
refreshes cannot prove that such a concurrent reassignment is consistent.

Initial attachment holds input until screen and keyboard modes arrive. Core
0.1.20 can also replace a backlogged stream with a fresh paint without marking
the repaint's start. Input during that replacement can briefly use intermediate
keyboard modes. A complete fix requires an explicit core repaint boundary.

## Validation

Real-core tests require an actual runtime. Set `HERDR_CORE_BIN` to its absolute
path when it is not available as `hc` on PATH. Each test creates a scratch home,
state and runtime directory and a unique `HC_SESSION`; test commands must never
stop or restart a user's existing core.

```sh
HERDR_CORE_BIN=/absolute/path/to/herdr-core just test
HERDR_CORE_BIN=/absolute/path/to/herdr-core just test-one default_tui_controls
just lint
```

`just test`, `just test-one` and `just ci-tests` build the legacy example before
running tests that still exercise its contract. The `core_client`, `core_tui`
and `core_remotes` integration targets exercise real core streams, TUI controls,
process lifetime, documents, remotes and worktrees. CI installs herdr-core 0.1.20
on macOS and Linux before running them. Windows checks continue to exercise the
retained legacy implementation and its ConPTY packaging.

The manual `core_linux_remote` test qualifies the macOS TUI against a Linux
core 0.1.20 over a custom Docker transport. It downloads Ubuntu 24.04 and the
official checksum-verified core release into a disposable container, without
mounting host files. Run it explicitly with Docker available:

```sh
DOCKER_CONTEXT=colima HERDR_CORE_BIN=/absolute/path/to/herdr-core \
  cargo test --locked --test core_linux_remote -- --ignored --nocapture
```

Omit `DOCKER_CONTEXT` to use Docker's current context. This checks remote shell
execution, saved-document ownership, detach/reopen process lifetime and Git
worktree safety. It does not qualify SSH authentication or transport behavior.

`just bench-render-scale` measures parsing, pane painting, the regular shell
composition and ANSI encoding at a fixed 5,400-cell window with 1 and 15 populated
core terminals. `just bench-bsp-layout` measures the shared layout code. The old full-render
and server CPU comparisons are explicitly named
`just bench-legacy-render-scale` and `just bench-legacy-release-smoke`. Their
results describe the legacy example, not the core-backed TUI.

Offline builds, parser tests and CLI checks do not establish runtime behavior.
Real-core integration tests need an environment that permits core's Unix sockets
and process startup. Treat an unavailable runtime or a blocked socket bind as
unavailable validation, not as a passing integration result.
