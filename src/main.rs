mod app;
mod config;
mod model;
mod rpc;
mod ui;

use std::io::{self, IsTerminal};

use anyhow::{bail, Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, Event, EventStream,
        KeyCode, KeyEventKind, MouseEventKind,
    },
    execute,
    style::Print,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};

use app::Controller;
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
        debug,
        resume_on_start,
    )
    .await?;

    let mut guard = TerminalGuard::enter(config.mouse)?;
    let mut events = EventStream::new();
    let mut rpc_open = true;
    let mut dirty = true;
    let mut redraw = tokio::time::interval(std::time::Duration::from_millis(33));
    redraw.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = redraw.tick(), if dirty => {
                guard
                    .terminal
                    .draw(|frame| ui::draw(frame, &mut controller.state))?;
                dirty = false;
            }
            _ = wait_for_next_working_frame(controller.state.turn_started_at) => dirty = true,
            event = events.next() => {
                let redraw_after_event = event.as_ref().is_some_and(|event| {
                    event.as_ref().is_ok_and(|event| event_requests_redraw(event, config.mouse))
                });
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
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
                    Some(Ok(Event::Resize(_, _))) => {}
                    Some(Err(error)) => return Err(error.into()),
                    None => break,
                    _ => {}
                }
                if redraw_after_event {
                    dirty = true;
                }
            }
            incoming = controller.next_rpc(), if rpc_open => {
                if let Some(incoming) = incoming {
                    let disconnected = matches!(&incoming, rpc::Incoming::Disconnected(_));
                    controller.handle_incoming(incoming)?;
                    if disconnected {
                        rpc_open = false;
                    }
                } else {
                    rpc_open = false;
                    controller.state.popup = Some(model::Popup::Disconnected {
                        reason: "Codex backend event channel closed".into(),
                        selected: 0,
                    });
                }
                dirty = true;
            }
        }
        if controller.state.quit {
            break;
        }
    }

    guard.restore()?;
    controller.shutdown().await;
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
        Event::Key(key) => key.kind == KeyEventKind::Press,
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

fn print_help() {
    println!(
        "magdex {}\n\nMagdex — minimal TUI for Codex App Server\n\nUsage: magdex [--debug] [resume]\n\nCommands:\n  resume    Choose a recent conversation from the current directory\n\nOptions:\n  --debug   Record JSON-RPC traffic\n  -h, --help\n  -V, --version",
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
        execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
        if mouse {
            // Normal tracking reports clicks and wheel events without the
            // pointer-motion events enabled by Crossterm's broad preset.
            execute!(stdout, Print("\x1b[?1000h\x1b[?1006h"))?;
        }
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
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
            DisableBracketedPaste,
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
    use super::{next_working_redraw, WORKING_FRAME_MILLIS};

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
}
