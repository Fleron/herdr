herdr — terminal UI for herdr-core

Usage: herdr
       herdr --help | --version | --skill | --default-config
       herdr config check

Requires hc (herdr-core 0.1.20 or a compatible generation-1 core) on PATH.
The TUI shares running core terminals and saves its own workspace and pane layouts.
Use hc for runtime commands and machine management. This executable does not run
the legacy Herdr server, plugin API, or updater.

The configured prefix defaults to Ctrl+B. Press prefix then ? for current shortcuts.
Closing views or detaching leaves programs running. Stop terminal explicitly to
terminate a program shared with other viewers.
