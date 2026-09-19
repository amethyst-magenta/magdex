mod app;
mod config;
mod model;
mod notification;
mod rpc;
mod ui;
mod update;

use std::io::{self, IsTerminal, Write};

use anyhow::{bail, Context, Result};
use crossterm::{
    cursor::{MoveTo, RestorePosition, SavePosition},
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, Event, EventStream, KeyCode, KeyEventKind, KeyModifiers,
        KeyboardEnhancementFlags, MouseEventKind, PopKeyboardEnhancementFlags,
        PushKeyboardEnhancementFlags,
    },
    execute, queue,
    style::{Attribute, Colors, Print, ResetColor, SetAttribute, SetColors},
    terminal::{
        disable_raw_mode, enable_raw_mode, BeginSynchronizedUpdate, EndSynchronizedUpdate,
        EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use futures_util::StreamExt;
use ratatui::{
    backend::{Backend, CrosstermBackend},
    style::Modifier,
    Terminal,
};

use app::{normalize_control_shortcut, Controller};
use config::ClientConfig;

const WORKING_FRAME_MILLIS: u64 = 50;

#[tokio::main]
async fn main() -> Result<()> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return Ok(());
    }
    if args.iter().any(|arg| arg == "--version" || arg == "-V") {
        println!("magdex {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let debug = args.iter().any(|arg| arg == "--debug");
    let positional = args
        .iter()
        .filter(|arg| arg.as_str() != "--debug")
        .collect::<Vec<_>>();
    let resume_on_start = match positional.as_slice() {
        [] => false,
        [command] if command.as_str() == "resume" => true,
        [argument, ..] => bail!("unknown argument: {argument}\n\nUsage: magdex [--debug] [resume]"),
    };
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("magdex needs an interactive terminal");
    }

    let config = ClientConfig::load();
    let cwd = std::env::current_dir()
        .context("cannot determine current working directory")?
        .to_string_lossy()
        .into_owned();
    let mut controller = Controller::new(
        cwd,
        config.show_reasoning,
        config.default_mode_request_user_input,
        config.notifications,
        debug,
        resume_on_start,
    )
    .await?;

    let mut guard = TerminalGuard::enter(config.mouse)?;
    let mut events = EventStream::new();
    let mut rpc_open = true;
    let mut dirty = true;
    let mut full_redraw = false;
    let mut hyperlink_overlay = Vec::new();
    let zellij_redraw_workaround = running_in_zellij();
    let mut rendered_block_count = controller.state.blocks.len();
    let mut redraw = tokio::time::interval(std::time::Duration::from_millis(33));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = redraw.tick(), if dirty => {
                let was_full_redraw = full_redraw;
                let previous_max_scroll = controller.state.transcript_max_scroll;
                let previous_viewport_height = controller.state.transcript_viewport_height;
                if full_redraw {
                    invalidate_previous_frame(&mut guard.terminal);
                }
                let next_hyperlink_overlay = synchronized_terminal_update(
                    &mut guard.terminal,
                    |terminal| {
                        let (next_hyperlink_overlay, cleanup) = {
                            let completed = terminal
                                .draw(|frame| ui::draw(frame, &mut controller.state))?;
                            let next_hyperlink_overlay = ui::terminal_hyperlink_overlay(
                                completed.buffer,
                                &controller.state.visible_hyperlinks,
                            );
                            let cleanup = if next_hyperlink_overlay != hyperlink_overlay {
                                ui::terminal_hyperlink_cleanup(
                                    completed.buffer,
                                    &hyperlink_overlay,
                                )
                            } else {
                                Vec::new()
                            };
                            (next_hyperlink_overlay, cleanup)
                        };
                        let overlay_changed = next_hyperlink_overlay != hyperlink_overlay;
                        if overlay_changed || was_full_redraw {
                            write_terminal_hyperlinks(
                                terminal.backend_mut(),
                                &cleanup,
                                &next_hyperlink_overlay,
                            )?;
                        }
                        Ok(next_hyperlink_overlay)
                    },
                )?;
                hyperlink_overlay = next_hyperlink_overlay;
                rendered_block_count = controller.state.blocks.len();
                let transcript_shifted = previous_max_scroll
                    != controller.state.transcript_max_scroll
                    || previous_viewport_height != controller.state.transcript_viewport_height;
                let followup_full_redraw =
                    zellij_redraw_workaround && !was_full_redraw && transcript_shifted;
                full_redraw = followup_full_redraw;
                dirty = followup_full_redraw;
            }
            _ = wait_for_next_working_frame(controller.state.turn_started_at) => dirty = true,
            event = events.next() => {
                let redraw_after_event = event.as_ref().is_some_and(|event| {
                    event.as_ref().is_ok_and(|event| event_requests_redraw(event, config.mouse))
                });
                let full_redraw_after_event = event.as_ref().is_some_and(|event| {
                    event.as_ref().is_ok_and(|event| {
                        event_requests_full_redraw(
                            event,
                            zellij_redraw_workaround,
                        )
                    })
                });
                match event {
                    Some(Ok(Event::Key(key)))
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        let restart = matches!(
                            controller.state.popup,
                            Some(model::Popup::Disconnected { selected: 0, .. })
                        ) && key.code == KeyCode::Enter;
                        if restart {
                            match controller.restart().await {
                                Ok(()) => rpc_open = true,
                                Err(error) => {
                                    controller.state.popup = Some(model::Popup::Disconnected {
                                        reason: error.to_string(),
                                        selected: 0,
                                    });
                                }
                            }
                        } else {
                            controller.handle_key(key)?;
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) if config.mouse => controller.handle_mouse(mouse),
                    Some(Ok(Event::Paste(text))) => controller.handle_paste(&text),
                    Some(Ok(Event::FocusGained)) => controller.set_terminal_focused(true),
                    Some(Ok(Event::FocusLost)) => controller.set_terminal_focused(false),
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Err(error)) => return Err(error.into()),
                    None => break,
                    _ => {}
                }
                if redraw_after_event {
                    dirty = true;
                }
                if full_redraw_after_event {
                    full_redraw = true;
                }
            }
            incoming = controller.next_rpc(), if rpc_open => {
                if let Some(incoming) = incoming {
                    let disconnected = incoming.is_disconnected();
                    controller.handle_incoming(incoming)?;
                    if disconnected {
                        rpc_open = false;
                    }
                } else {
                    rpc_open = false;
                    controller.backend_disconnected(
                        "Codex backend event channel closed".into(),
                    );
                }
                dirty = true;
            }
        }
        if controller.state.quit {
            break;
        }
        if state_requests_full_redraw(
            &controller.state,
            rendered_block_count,
            guard.terminal.size()?,
            zellij_redraw_workaround,
        ) {
            full_redraw = true;
            dirty = true;
        }
    }

    let update_codex = controller.update_codex_on_exit();
    guard.restore()?;
    controller.shutdown().await;
    if update_codex {
        update::run_update().await?;
    }
    Ok(())
}

async fn wait_for_next_working_frame(started: Option<std::time::Instant>) {
    let Some(started) = started else {
        std::future::pending::<()>().await;
        return;
    };
    let deadline = next_working_redraw(started, std::time::Instant::now());
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
}

fn next_working_redraw(started: std::time::Instant, now: std::time::Instant) -> std::time::Instant {
    let elapsed_millis = now
        .saturating_duration_since(started)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    let next_frame_millis = (elapsed_millis / WORKING_FRAME_MILLIS).saturating_add(1);
    started
        + std::time::Duration::from_millis(next_frame_millis.saturating_mul(WORKING_FRAME_MILLIS))
}

fn event_requests_redraw(event: &Event, mouse: bool) -> bool {
    match event {
        Event::Key(key) => matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat),
        Event::Mouse(mouse_event) => {
            mouse
                && matches!(
                    mouse_event.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                )
        }
        Event::Paste(_) | Event::Resize(_, _) => true,
        _ => false,
    }
}

fn event_requests_full_redraw(event: &Event, zellij_workaround: bool) -> bool {
    match event {
        Event::Key(key)
            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            let key = normalize_control_shortcut(*key);
            zellij_workaround && matches!(key.code, KeyCode::Char('k' | 'j' | 'g' | 'o' | 'y'))
        }
        Event::Mouse(mouse) => {
            zellij_workaround
                && matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                )
        }
        Event::Resize(_, _) => true,
        _ => false,
    }
}

fn state_requests_full_redraw(
    state: &model::AppState,
    rendered_block_count: usize,
    size: ratatui::layout::Size,
    zellij_workaround: bool,
) -> bool {
    if !zellij_workaround {
        return false;
    }
    let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
    let viewport_will_change = ui::expected_transcript_viewport_height(state, area)
        .is_some_and(|height| height as usize != state.transcript_viewport_height);
    viewport_will_change || state.blocks.len() != rendered_block_count
}

fn invalidate_previous_frame<B: Backend>(terminal: &mut Terminal<B>) {
    // The current buffer is empty between frames. Mark it as different from
    // every normally rendered cell before swapping it into the previous slot,
    // so the next diff also writes blank cells that must erase stale output.
    // This avoids a physical clear, which briefly blanks Zellij.
    for cell in &mut terminal.current_buffer_mut().content {
        cell.set_skip(true);
    }
    terminal.swap_buffers();
}

fn synchronized_terminal_update<B, T, F>(terminal: &mut Terminal<B>, update: F) -> Result<T>
where
    B: Backend + Write,
    F: FnOnce(&mut Terminal<B>) -> Result<T>,
{
    execute!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
    let update_result = update(terminal);
    let end_result = execute!(terminal.backend_mut(), EndSynchronizedUpdate);
    match (update_result, end_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Err(error), Ok(())) => Err(error),
        (_, Err(error)) => Err(error.into()),
    }
}

fn write_terminal_hyperlinks<W: Write>(
    writer: &mut W,
    cleanup: &[ui::TerminalOverlayCell],
    hyperlinks: &[ui::TerminalHyperlinkOverlay],
) -> io::Result<()> {
    if cleanup.is_empty() && hyperlinks.is_empty() {
        return Ok(());
    }

    queue!(writer, SavePosition)?;
    for cell in cleanup {
        queue_terminal_cell(writer, cell, None)?;
    }
    for hyperlink in hyperlinks {
        for cell in &hyperlink.cells {
            queue_terminal_cell(writer, cell, Some(&hyperlink.url))?;
        }
    }
    queue!(
        writer,
        SetAttribute(Attribute::Reset),
        ResetColor,
        RestorePosition
    )?;
    writer.flush()
}

fn queue_terminal_cell<W: Write>(
    writer: &mut W,
    overlay: &ui::TerminalOverlayCell,
    hyperlink: Option<&str>,
) -> io::Result<()> {
    let cell = &overlay.cell;
    queue!(
        writer,
        MoveTo(overlay.position.x, overlay.position.y),
        SetAttribute(Attribute::Reset),
        SetColors(Colors::new(cell.fg.into(), cell.bg.into()))
    )?;
    for (modifier, attribute) in [
        (Modifier::REVERSED, Attribute::Reverse),
        (Modifier::BOLD, Attribute::Bold),
        (Modifier::ITALIC, Attribute::Italic),
        (Modifier::UNDERLINED, Attribute::Underlined),
        (Modifier::DIM, Attribute::Dim),
        (Modifier::CROSSED_OUT, Attribute::CrossedOut),
        (Modifier::SLOW_BLINK, Attribute::SlowBlink),
        (Modifier::RAPID_BLINK, Attribute::RapidBlink),
        (Modifier::HIDDEN, Attribute::Hidden),
    ] {
        if cell.modifier.contains(modifier) {
            queue!(writer, SetAttribute(attribute))?;
        }
    }

    if let Some(url) = hyperlink {
        queue!(
            writer,
            Print(format!(
                "\x1b]8;;{url}\x1b\\{}\x1b]8;;\x1b\\",
                cell.symbol()
            ))
        )
    } else {
        queue!(writer, Print(cell.symbol()))
    }
}

fn running_in_zellij() -> bool {
    std::env::var_os("ZELLIJ").is_some() || std::env::var_os("ZELLIJ_SESSION_NAME").is_some()
}

fn print_help() {
    println!(
        "magdex {}\n\nMagdex — minimal TUI for Codex App Server\n\nUsage: magdex [--debug] [resume]\n\nCommands:\n  resume    Choose a recent conversation (Tab switches directory scope)\n\nOptions:\n  --debug   Record JSON-RPC traffic\n  -h, --help\n  -V, --version",
        env!("CARGO_PKG_VERSION")
    );
}

struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<io::Stdout>>,
    mouse: bool,
    restored: bool,
}

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(
            stdout,
            EnterAlternateScreen,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES),
            EnableBracketedPaste,
            EnableFocusChange
        )?;
        if mouse {
            // Normal tracking reports clicks and wheel events without the
            // pointer-motion events enabled by Crossterm's broad preset.
            execute!(stdout, Print("\x1b[?1000h\x1b[?1006h"))?;
        }
        let mut terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        terminal.clear()?;
        Ok(Self {
            terminal,
            mouse,
            restored: false,
        })
    }

    fn restore(&mut self) -> Result<()> {
        if self.restored {
            return Ok(());
        }
        disable_raw_mode()?;
        if self.mouse {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }
        execute!(
            self.terminal.backend_mut(),
            DisableFocusChange,
            DisableBracketedPaste,
            PopKeyboardEnhancementFlags,
            LeaveAlternateScreen
        )?;
        self.terminal.show_cursor()?;
        self.restored = true;
        Ok(())
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

#[cfg(test)]
mod tests {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::{
        backend::TestBackend,
        buffer::Cell,
        layout::Position,
        style::{Color, Modifier},
        Terminal,
    };

    use super::{
        event_requests_full_redraw, invalidate_previous_frame, next_working_redraw,
        state_requests_full_redraw, write_terminal_hyperlinks, WORKING_FRAME_MILLIS,
    };
    use crate::{
        model::{AppState, BlockKind, TranscriptBlock},
        ui::{self, TerminalHyperlinkOverlay, TerminalOverlayCell},
    };

    #[test]
    fn working_redraw_aligns_with_turn_frames() {
        let now = std::time::Instant::now();
        let started = now - std::time::Duration::from_millis(1_275);

        assert_eq!(
            next_working_redraw(started, now).duration_since(now),
            std::time::Duration::from_millis(25)
        );
        assert_eq!(1_000 % WORKING_FRAME_MILLIS, 0);
    }

    #[test]
    fn zellij_transcript_workaround_requests_a_full_redraw() {
        for code in ['k', 'j', 'g', 'o', 'y'] {
            let event = Event::Key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL));
            assert!(event_requests_full_redraw(&event, true));
            assert!(!event_requests_full_redraw(&event, false));
        }
        for code in ['л', 'о', 'п', 'щ', 'н'] {
            let event = Event::Key(KeyEvent::new(KeyCode::Char(code), KeyModifiers::CONTROL));
            assert!(event_requests_full_redraw(&event, true));
            assert!(!event_requests_full_redraw(&event, false));
        }

        let plain_key = Event::Key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
        assert!(!event_requests_full_redraw(&plain_key, true));
        for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
            let wheel = Event::Mouse(MouseEvent {
                kind,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            });
            assert!(event_requests_full_redraw(&wheel, true));
            assert!(!event_requests_full_redraw(&wheel, false));
        }
        let moved = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!event_requests_full_redraw(&moved, true));
        assert!(event_requests_full_redraw(&Event::Resize(120, 40), false));
    }

    #[test]
    fn zellij_state_changes_request_a_full_redraw() {
        let size = ratatui::layout::Size::new(40, 20);
        let area = ratatui::layout::Rect::new(0, 0, size.width, size.height);
        let mut state = AppState::new("/project".into(), true);
        state.transcript_viewport_height =
            ui::expected_transcript_viewport_height(&state, area).unwrap() as usize;
        let rendered_blocks = state.blocks.len();

        assert!(!state_requests_full_redraw(
            &state,
            rendered_blocks,
            size,
            true
        ));
        state
            .blocks
            .push(TranscriptBlock::new(BlockKind::Status, "Quota", "warning"));
        assert!(state_requests_full_redraw(
            &state,
            rendered_blocks,
            size,
            true
        ));
        assert!(!state_requests_full_redraw(
            &state,
            rendered_blocks,
            size,
            false
        ));

        let rendered_blocks = state.blocks.len();
        state.composer.insert_str(&"long composer text ".repeat(12));
        assert!(state_requests_full_redraw(
            &state,
            rendered_blocks,
            size,
            true
        ));
    }

    #[test]
    fn full_redraw_overwrites_stale_terminal_cells() {
        let backend = TestBackend::with_lines(["stale"]);
        let mut terminal = Terminal::new(backend).unwrap();

        invalidate_previous_frame(&mut terminal);
        terminal.draw(|_| {}).unwrap();

        assert_eq!(terminal.backend().buffer(), TestBackend::new(5, 1).buffer());
    }

    #[test]
    fn hyperlink_overlay_writes_osc8_without_changing_visible_text() {
        let mut cell = Cell::default();
        cell.set_symbol("L")
            .set_fg(Color::Blue)
            .set_style(Modifier::UNDERLINED);
        let overlay = TerminalHyperlinkOverlay {
            url: "https://example.com".into(),
            cells: vec![TerminalOverlayCell {
                position: Position::new(2, 3),
                cell,
            }],
        };
        let mut output = Vec::new();

        write_terminal_hyperlinks(&mut output, &[], &[overlay]).unwrap();

        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("\x1b]8;;https://example.com\x1b\\L\x1b]8;;\x1b\\"));
    }
}
