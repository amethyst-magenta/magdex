use std::{
    env,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use serde::de::DeserializeOwned;
use serde::Deserialize;
use tokio::process::Command;

#[derive(Debug)]
pub struct Notifier {
    enabled: bool,
    visibility: VisibilityProbe,
}

impl Notifier {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            visibility: VisibilityProbe::from_environment(),
        }
    }

    pub fn set_terminal_focused(&self, focused: bool) {
        self.visibility
            .terminal_focused
            .store(focused, Ordering::Relaxed);
    }

    pub fn action_required(&self, project: &str, detail: &str) {
        self.send(
            "Magdex needs your input",
            notification_body(project, Some(detail)),
            "dialog-question",
        );
    }

    pub fn response_ready(&self, project: &str) {
        self.send(
            "Magdex response ready",
            notification_body(project, None),
            "dialog-information",
        );
    }

    pub fn turn_failed(&self, project: &str) {
        self.send(
            "Magdex needs your attention",
            notification_body(project, Some("The turn failed")),
            "dialog-error",
        );
    }

    pub fn quota_low(&self, project: &str, detail: &str) {
        self.send(
            "Magdex quota warning",
            notification_body(project, Some(detail)),
            "dialog-warning",
        );
    }

    fn send(&self, summary: &'static str, body: String, icon: &'static str) {
        if !self.enabled {
            return;
        }
        let visibility = self.visibility.clone();
        tokio::spawn(async move {
            if visibility.is_visible().await {
                return;
            }
            let _ = Command::new("notify-send")
                .args([
                    "--app-name=Magdex",
                    "--urgency=normal",
                    "--category=im.received",
                    "--icon",
                    icon,
                    "--",
                    summary,
                    &body,
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;
        });
    }
}

#[derive(Clone, Debug)]
struct VisibilityProbe {
    terminal_focused: Arc<AtomicBool>,
    zellij: Option<ZellijContext>,
    niri: Option<NiriContext>,
}

impl VisibilityProbe {
    fn from_environment() -> Self {
        let zellij = env::var("ZELLIJ_SESSION_NAME")
            .ok()
            .zip(
                env::var("ZELLIJ_PANE_ID")
                    .ok()
                    .and_then(|id| id.parse().ok()),
            )
            .map(|(session, pane_id)| ZellijContext { session, pane_id });
        let niri = if env::var_os("NIRI_SOCKET").is_some() {
            terminal_pid_from_environment().map(|terminal_pid| NiriContext {
                terminal_pid,
                title_hint: zellij.as_ref().map(|context| context.session.clone()),
            })
        } else {
            None
        };
        Self {
            terminal_focused: Arc::new(AtomicBool::new(true)),
            zellij,
            niri,
        }
    }

    async fn is_visible(&self) -> bool {
        let terminal_focused = self.terminal_focused.load(Ordering::Relaxed);
        let (zellij_visible, niri_visible) = tokio::join!(
            async {
                match &self.zellij {
                    Some(context) => context.is_visible().await,
                    None => None,
                }
            },
            async {
                match &self.niri {
                    Some(context) => context.is_visible().await,
                    None => None,
                }
            }
        );

        visibility_from_checks(terminal_focused, zellij_visible, niri_visible)
    }
}

fn visibility_from_checks(
    terminal_focused: bool,
    zellij_visible: Option<bool>,
    niri_visible: Option<bool>,
) -> bool {
    let window_visible = niri_visible.unwrap_or(terminal_focused);
    let pane_visible = zellij_visible.unwrap_or(true);
    window_visible && pane_visible
}

#[derive(Clone, Debug)]
struct ZellijContext {
    session: String,
    pane_id: u64,
}

impl ZellijContext {
    async fn is_visible(&self) -> Option<bool> {
        let pane_args = [
            "--session",
            &self.session,
            "action",
            "list-panes",
            "--json",
            "--all",
        ];
        let tab_args = [
            "--session",
            &self.session,
            "action",
            "list-tabs",
            "--json",
            "--all",
        ];
        let panes = run_json::<Vec<ZellijPane>>("zellij", &pane_args);
        let tabs = run_json::<Vec<ZellijTab>>("zellij", &tab_args);
        let (panes, tabs) = tokio::join!(panes, tabs);
        zellij_pane_visible(&panes?, &tabs?, self.pane_id)
    }
}

#[derive(Debug, Deserialize)]
struct ZellijPane {
    id: u64,
    is_plugin: bool,
    is_fullscreen: bool,
    is_floating: bool,
    is_suppressed: bool,
    tab_id: u64,
}

#[derive(Debug, Deserialize)]
struct ZellijTab {
    tab_id: u64,
    active: bool,
    is_fullscreen_active: bool,
    are_floating_panes_visible: bool,
}

fn zellij_pane_visible(panes: &[ZellijPane], tabs: &[ZellijTab], pane_id: u64) -> Option<bool> {
    let pane = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == pane_id)?;
    let tab = tabs.iter().find(|tab| tab.tab_id == pane.tab_id)?;
    Some(
        tab.active
            && !pane.is_suppressed
            && (!tab.is_fullscreen_active || pane.is_fullscreen)
            && (!pane.is_floating || tab.are_floating_panes_visible),
    )
}

#[derive(Clone, Debug)]
struct NiriContext {
    terminal_pid: u32,
    title_hint: Option<String>,
}

impl NiriContext {
    async fn is_visible(&self) -> Option<bool> {
        let windows = run_json::<Vec<NiriWindow>>("niri", &["msg", "-j", "windows"]);
        let workspaces = run_json::<Vec<NiriWorkspace>>("niri", &["msg", "-j", "workspaces"]);
        let (windows, workspaces) = tokio::join!(windows, workspaces);
        niri_window_visible(
            &windows?,
            &workspaces?,
            self.terminal_pid,
            self.title_hint.as_deref(),
        )
    }
}

#[derive(Debug, Deserialize)]
struct NiriWindow {
    pid: Option<u32>,
    title: Option<String>,
    workspace_id: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct NiriWorkspace {
    id: u64,
    is_active: bool,
}

fn niri_window_visible(
    windows: &[NiriWindow],
    workspaces: &[NiriWorkspace],
    terminal_pid: u32,
    title_hint: Option<&str>,
) -> Option<bool> {
    let process_windows = windows
        .iter()
        .filter(|window| window.pid == Some(terminal_pid))
        .collect::<Vec<_>>();
    if process_windows.is_empty() {
        return None;
    }
    let titled_windows = title_hint
        .map(|title| {
            process_windows
                .iter()
                .copied()
                .filter(|window| window.title.as_deref() == Some(title))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let matching_windows = if titled_windows.is_empty() {
        process_windows
    } else {
        titled_windows
    };
    Some(matching_windows.iter().any(|window| {
        window.workspace_id.is_some_and(|workspace_id| {
            workspaces
                .iter()
                .any(|workspace| workspace.id == workspace_id && workspace.is_active)
        })
    }))
}

fn terminal_pid_from_environment() -> Option<u32> {
    env::var("ALACRITTY_SOCKET")
        .ok()
        .as_deref()
        .and_then(alacritty_pid_from_socket)
}

fn alacritty_pid_from_socket(socket: &str) -> Option<u32> {
    socket
        .strip_suffix(".sock")?
        .rsplit('-')
        .next()?
        .parse()
        .ok()
}

async fn run_json<T: DeserializeOwned>(program: &str, args: &[&str]) -> Option<T> {
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    serde_json::from_slice(&output.stdout).ok()
}

fn notification_body(project: &str, detail: Option<&str>) -> String {
    let project = escape_markup(project);
    match detail {
        Some(detail) => format!("{project}\n{}", escape_markup(detail)),
        None => project,
    }
}

fn escape_markup(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u64, tab_id: u64) -> ZellijPane {
        ZellijPane {
            id,
            is_plugin: false,
            is_fullscreen: false,
            is_floating: false,
            is_suppressed: false,
            tab_id,
        }
    }

    fn tab(tab_id: u64, active: bool) -> ZellijTab {
        ZellijTab {
            tab_id,
            active,
            is_fullscreen_active: false,
            are_floating_panes_visible: false,
        }
    }

    #[test]
    fn zellij_keeps_visible_unfocused_panes_quiet() {
        let panes = vec![pane(2, 10), pane(3, 10)];
        assert_eq!(zellij_pane_visible(&panes, &[tab(10, true)], 3), Some(true));
    }

    #[test]
    fn zellij_detects_hidden_tabs_and_fullscreen_panes() {
        let panes = vec![pane(2, 10), pane(3, 11)];
        assert_eq!(
            zellij_pane_visible(&panes, &[tab(10, true), tab(11, false)], 3),
            Some(false)
        );
        let mut fullscreen_tab = tab(10, true);
        fullscreen_tab.is_fullscreen_active = true;
        assert_eq!(
            zellij_pane_visible(&panes, &[fullscreen_tab], 2),
            Some(false)
        );
    }

    #[test]
    fn niri_detects_whether_the_terminal_workspace_is_visible() {
        let windows = vec![NiriWindow {
            pid: Some(4179),
            title: Some("work".into()),
            workspace_id: Some(7),
        }];
        assert_eq!(
            niri_window_visible(
                &windows,
                &[NiriWorkspace {
                    id: 7,
                    is_active: true,
                }],
                4179,
                Some("work"),
            ),
            Some(true)
        );
        assert_eq!(
            niri_window_visible(
                &windows,
                &[NiriWorkspace {
                    id: 7,
                    is_active: false,
                }],
                4179,
                Some("work"),
            ),
            Some(false)
        );
    }

    #[test]
    fn visible_workspace_overrides_multiplexer_pane_focus() {
        assert!(visibility_from_checks(false, None, Some(true)));
        assert!(visibility_from_checks(false, Some(true), Some(true)));
        assert!(!visibility_from_checks(false, Some(false), Some(true)));
        assert!(!visibility_from_checks(false, Some(true), Some(false)));
    }

    #[test]
    fn terminal_focus_is_used_when_window_visibility_is_unknown() {
        assert!(visibility_from_checks(true, Some(true), None));
        assert!(!visibility_from_checks(false, Some(true), None));
    }

    #[test]
    fn alacritty_socket_exposes_the_terminal_process() {
        assert_eq!(
            alacritty_pid_from_socket("/run/user/1000/Alacritty-wayland-1-4179.sock"),
            Some(4179)
        );
    }

    #[test]
    fn action_body_contains_project_and_reason_without_markup() {
        assert_eq!(
            notification_body("~/code/a&b", Some("Choose <unsafe>?")),
            "~/code/a&amp;b\nChoose &lt;unsafe&gt;?"
        );
    }
}
