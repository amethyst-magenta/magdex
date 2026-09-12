mod app;
mod config;
mod model;
mod rpc;
mod ui;

use std::io::{self, IsTerminal};

use anyhow::{bail, Context, Result};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, EventStream, KeyCode, KeyEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures_util::StreamExt;
use ratatui::{backend::CrosstermBackend, Terminal};

use app::Controller;
use config::ClientConfig;

#[tokio::main]
async fn main() -> Result<()> {
    let debug = std::env::args().any(|arg| arg == "--debug");
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "magdex {}\n\nMagdex — minimal TUI for Codex App Server\n\nUsage: magdex [--debug]",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }
    if std::env::args().any(|arg| arg == "--version" || arg == "-V") {
        println!("magdex {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
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
    )
    .await?;

    let mut guard = TerminalGuard::enter(config.mouse)?;
    let mut events = EventStream::new();
    let mut animation = tokio::time::interval(std::time::Duration::from_millis(100));
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        guard
            .terminal
            .draw(|frame| ui::draw(frame, &mut controller.state))?;
        if controller.state.quit {
            break;
        }
        let animate = controller.state.turn_started_at.is_some();
        tokio::select! {
            _ = animation.tick(), if animate => {}
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        let restart = matches!(
                            controller.state.popup,
                            Some(model::Popup::Disconnected { selected: 0, .. })
                        ) && key.code == KeyCode::Enter;
                        if restart {
                            if let Err(error) = controller.restart().await {
                                controller.state.popup = Some(model::Popup::Disconnected {
                                    reason: error.to_string(),
                                    selected: 0,
                                });
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
            }
            incoming = controller.next_rpc() => {
                if let Some(incoming) = incoming {
                    controller.handle_incoming(incoming)?;
                } else {
                    controller.state.popup = Some(model::Popup::Disconnected {
                        reason: "Codex backend event channel closed".into(),
                        selected: 0,
                    });
                }
            }
        }
    }

    guard.restore()?;
    controller.shutdown().await;
    Ok(())
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
            execute!(stdout, EnableMouseCapture)?;
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
