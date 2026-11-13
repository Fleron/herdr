# Herdr core TUI

Run `herdr` to open the TUI. It uses the installed `hc` runtime and does not run
the legacy Herdr server. `hc --skill` documents runtime commands.

The configured prefix defaults to Ctrl+B. Open help with prefix then ?.
Workspaces, tabs and pane arrangements belong to this client; terminal programs
belong to core and can also be viewed in the desktop app. Closing a view or
detaching leaves its program running. Stop terminal is a separate confirmed action.

Use `hc machine add` to configure remote machines. The TUI uses those machines
for remote terminals and Git worktree operations. Programs without OSC 7501
reporting have unknown agent status.

Configuration uses the existing Herdr config file. `herdr config check` validates
it. Plugin actions, the legacy RPC/CLI environment and live legacy-session imports
are outside this fork's supported interface.

Tests must use a unique HC_SESSION and scratch HOME/XDG paths in child processes.
Never stop or restart the user's real core or agents during testing.
