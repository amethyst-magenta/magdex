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

Choose one of the recent conversations from the current directory. Press `Tab` in
the picker to include conversations from every working directory:

```bash
magdex resume
```

## Keys

| Key | Action |
| --- | --- |
| `Enter` | Send |
| `Shift+Enter` | Insert a newline |
| `Ctrl+V` | Paste text or attach an image from the system clipboard |
| `Ctrl+Y` | Open Copy mode at the latest assistant response |
| `Ctrl+C` | Clear input; interrupt when empty; quit when idle and empty |
| `Ctrl+P` / `Ctrl+N` | Recall older/newer messages in the composer |
| `↑` / `↓` | Move between composer lines |
| `Ctrl+K` / `Ctrl+J` | Smoothly scroll transcript up/down |
| `Ctrl+G` | Jump to the latest output |
| Mouse wheel | Scroll transcript |
| `Ctrl+O` | Expand the current command output |
| `Tab` | Toggle Default/Plan; switch current/all inside a resume picker |
| `Esc` | Close the bottom panel (or cancel an approval) |
| `↑` / `↓` or `j` / `k` | Choose an answer when Codex asks a structured question |

Press `Ctrl+V` to attach an image from the system clipboard. Repeat to attach
multiple images; an image-only turn is also supported. If the clipboard holds
text, `Ctrl+V` inserts that text normally.

Rendered Markdown links with `http://` or `https://` targets use native terminal
hyperlinks. In Alacritty, hold `Shift` while hovering or clicking when Magdex or a
terminal multiplexer has captured the mouse; `Ctrl+Shift+O` opens keyboard hints.

Copy mode starts on the latest assistant response. Use `j`/`k` to move between
responses, `Enter` to inspect its Markdown blocks, `Enter` again to toggle any
number of blocks, and `y` to copy the selected blocks. `Esc` moves back one level.

The textual commands are `/new`, `/resume`, `/mode`, `/model`, `/reasoning`,
`/history`, `/bottom`, and `/copy`. `/history` lists your messages in the current
thread; selecting one jumps to it in the transcript.
Typing `/` in the composer shows and filters the available commands.
`/new` switches to a new thread without deleting the current one; `/resume`
switches to a stored thread. Its picker starts with the current directory and can
show every directory with `Tab`. Authentication is checked on startup and required
automatically when no Codex account is available. A directory without an existing
trust decision must be explicitly trusted before Magdex starts or resumes a thread.

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
