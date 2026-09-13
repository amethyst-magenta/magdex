# Magdex

Magdex is a small terminal UI for the official Codex runtime. It starts
`codex app-server` and acts only as its JSON-RPC frontend; commands, file
changes, web access, approvals, configuration, authentication, `AGENTS.md`,
and thread storage remain Codex responsibilities.

## Build

Requirements: a recent Rust toolchain and the official `codex` executable in
`PATH`.

```bash
cargo build --release
install -Dm755 target/release/magdex ~/.local/bin/magdex
```

Run it from a project directory:

```bash
cd ~/code/my-project
magdex
```

Choose one of the recent conversations from the current directory:

```bash
magdex resume
```

## Keys

| Key | Action |
| --- | --- |
| `Enter` | Send |
| `Alt+Enter` | Insert a newline |
| `Ctrl+V` | Paste text or attach an image from the system clipboard |
| `Ctrl+C` | Interrupt; clear input; quit when idle and empty |
| `Ctrl+P` | Command palette |
| `Ctrl+R` | Resume a thread |
| `PageUp` / `PageDown` | Scroll transcript |
| `Home` / `End` | Top / follow newest output |
| Mouse wheel | Scroll transcript |
| `Ctrl+O` | Expand the latest long command output |
| `↑` / `↓` | Recall older/newer messages in the composer |
| `Esc` | Close the bottom panel (or cancel an approval) |
| `↑` / `↓` or `j` / `k` | Choose an answer when Codex asks a structured question |

Press `Ctrl+V` to attach an image from the system clipboard. Repeat to attach
multiple images; an image-only turn is also supported. If the clipboard holds
text, `Ctrl+V` inserts that text normally.

The other textual commands are `/new`, `/resume`, `/mode`, `/model`,
`/reasoning`, `/login`, and `/quit`.

Frontend-only settings may be placed in `~/.config/magdex/config.toml`:

```toml
show_reasoning = true
mouse = true
default_mode_request_user_input = true
```

- `show_reasoning` shows or hides streamed reasoning summaries.
- `mouse` enables terminal mouse capture and wheel scrolling.
- `default_mode_request_user_input` lets Codex ask structured questions in
  Default mode as well as Plan mode.

Codex model defaults, sandbox policy, approvals, MCP, skills, and web settings
belong in the normal Codex configuration, not this file.

`magdex --debug` records JSON-RPC traffic in `~/.cache/magdex/magdex.log`. App Server
stderr is always kept separately in `~/.cache/magdex/app-server.log`; neither log
is printed over the TUI.
