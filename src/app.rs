use std::{
    collections::HashMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{ensure, Result};
use arboard::Clipboard;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};
use serde_json::{json, Value};

use crate::{
    model::{
        ActionStatus, AppState, Approval, ApprovalKind, BlockKind, CollaborationModeInfo,
        CommandAction, CommandActionKind, Composer, ContextUsage, CopyMode, FileChange,
        FileChangeKind, ImageAttachment, ModelInfo, Popup, QueuedTurn, QuotaUsage, QuotaWindow,
        ResumePicker, ResumeScope, ServerPrompt, ThreadSummary, TranscriptBlock,
        TrustDirectoryPrompt, UserInputOption, UserInputQuestion, UserInputRequest,
    },
    notification::Notifier,
    rpc::{Incoming, RpcClient},
    ui::markdown_copy_ranges,
    update::{self, LatestVersion},
};

const MAX_COMMAND_OUTPUT_BYTES: usize = 256 * 1024;
const COMMAND_OUTPUT_TRUNCATED: &str = "\n[… command output truncated by Magdex …]";
const QUOTA_WARNING_REARM_PERCENT: u64 = 50;
pub const SLASH_COMMANDS: [(&str, &str); 8] = [
    ("/new", "New conversation"),
    ("/resume", "Resume conversation"),
    ("/mode", "Change mode"),
    ("/model", "Change model"),
    ("/reasoning", "Change reasoning effort"),
    ("/history", "Jump to a message"),
    ("/bottom", "Jump to latest output"),
    ("/copy", "Copy an assistant response"),
];

#[derive(Debug)]
enum Pending {
    Initialize,
    Account,
    RateLimits,
    Login,
    Models,
    CollaborationModes,
    Config,
    TrustProject {
        trust_target: String,
    },
    StartThread,
    ListThreads {
        target: ThreadListTarget,
        scope: ResumeScope,
    },
    ResumeThread(ResumeTarget),
    ThreadTurns {
        thread_id: String,
    },
    StartTurn,
    SteerTurn,
    UpdateMode,
    Interrupt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ControlCAction {
    ClearedComposer,
    RemovedQueuedTurn,
    Interrupt,
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollDirection {
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
enum ThreadListTarget {
    Popup,
    Picker,
}

#[derive(Clone, Copy, Debug)]
enum ResumeTarget {
    Popup,
    Picker,
}

#[derive(Debug)]
enum DeferredTurnEvent {
    Transcript(TranscriptBlock),
    QuotaWarning(String),
}

pub struct Controller {
    pub state: AppState,
    rpc: RpcClient,
    pending: HashMap<u64, Pending>,
    notifier: Notifier,
    default_mode_request_user_input: bool,
    debug: bool,
    startup_resume_pending: bool,
    quota_warnings_ready: bool,
    warned_quota_usage: QuotaUsage,
    rate_limits_refresh_needed: bool,
    deferred_turn_events: Vec<DeferredTurnEvent>,
    update_sender: Option<tokio::sync::mpsc::UnboundedSender<Option<LatestVersion>>>,
    update_receiver: tokio::sync::mpsc::UnboundedReceiver<Option<LatestVersion>>,
    update_receiver_closed: bool,
    codex_version: Option<String>,
    latest_codex_version: Option<LatestVersion>,
    startup_ready: bool,
    update_prompt_shown: bool,
    update_codex_on_exit: bool,
}

pub(crate) enum ControllerIncoming {
    Rpc(Incoming),
    Update(Option<LatestVersion>),
}

impl ControllerIncoming {
    pub(crate) fn is_disconnected(&self) -> bool {
        matches!(self, Self::Rpc(Incoming::Disconnected(_)))
    }
}

impl Controller {
    pub async fn new(
        cwd: String,
        show_reasoning: bool,
        default_mode_request_user_input: bool,
        notifications: bool,
        debug: bool,
        resume_on_start: bool,
    ) -> Result<Self> {
        let rpc = RpcClient::spawn(debug, default_mode_request_user_input).await?;
        let (update_sender, update_receiver) = tokio::sync::mpsc::unbounded_channel();
        let state = AppState::new(cwd, show_reasoning);
        let mut this = Self {
            state,
            rpc,
            pending: HashMap::new(),
            notifier: Notifier::new(notifications),
            default_mode_request_user_input,
            debug,
            startup_resume_pending: resume_on_start,
            quota_warnings_ready: false,
            warned_quota_usage: QuotaUsage::default(),
            rate_limits_refresh_needed: false,
            deferred_turn_events: Vec::new(),
            update_sender: Some(update_sender),
            update_receiver,
            update_receiver_closed: false,
            codex_version: None,
            latest_codex_version: None,
            startup_ready: false,
            update_prompt_shown: false,
            update_codex_on_exit: false,
        };
        this.initialize()?;
        Ok(this)
    }

    fn initialize(&mut self) -> Result<()> {
        let id = self.rpc.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "magdex",
                    "title": "Magdex",
                    "version": env!("CARGO_PKG_VERSION")
                },
                "capabilities": {"experimentalApi": true}
            }),
        )?;
        self.pending.insert(id, Pending::Initialize);
        Ok(())
    }

    pub async fn next_rpc(&mut self) -> Option<ControllerIncoming> {
        loop {
            tokio::select! {
                incoming = self.rpc.incoming.recv() => {
                    return incoming.map(ControllerIncoming::Rpc);
                }
                update = self.update_receiver.recv(), if !self.update_receiver_closed => {
                    match update {
                        Some(update) => return Some(ControllerIncoming::Update(update)),
                        None => self.update_receiver_closed = true,
                    }
                }
            }
        }
    }

    pub fn handle_incoming(&mut self, incoming: ControllerIncoming) -> Result<()> {
        match incoming {
            ControllerIncoming::Rpc(Incoming::Message(message)) => self.handle_rpc_message(message),
            ControllerIncoming::Rpc(Incoming::Disconnected(reason)) => {
                self.backend_disconnected(reason);
                Ok(())
            }
            ControllerIncoming::Update(update) => {
                self.latest_codex_version = update;
                self.maybe_show_update();
                Ok(())
            }
        }
    }

    pub fn backend_disconnected(&mut self, reason: String) {
        self.notify_action_required("Codex backend disconnected");
        self.state.turn_id = None;
        self.state.turn_started_at = None;
        self.flush_deferred_turn_events();
        self.state.popup = Some(Popup::Disconnected {
            reason,
            selected: 0,
        });
    }

    fn handle_rpc_message(&mut self, message: Value) -> Result<()> {
        if message.get("method").is_some() && message.get("id").is_some() {
            return self.handle_server_request(message);
        }
        if let Some(id) = message.get("id").and_then(Value::as_u64) {
            return self.handle_response(id, message);
        }
        if let Some(method) = message.get("method").and_then(Value::as_str) {
            let params = message.get("params").cloned().unwrap_or(Value::Null);
            return self.handle_notification(method, params);
        }
        Ok(())
    }

    fn handle_response(&mut self, id: u64, message: Value) -> Result<()> {
        let Some(pending) = self.pending.remove(&id) else {
            return Ok(());
        };
        if let Some(error) = message.get("error") {
            if matches!(&pending, Pending::RateLimits) {
                self.rate_limits_refresh_needed = false;
                return Ok(());
            }
            let detail = error
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| error.to_string());
            self.state.push(TranscriptBlock::new(
                BlockKind::Error,
                "Error",
                detail.clone(),
            ));
            match pending {
                Pending::ListThreads { target, scope } => {
                    self.apply_thread_list_error(target, scope, detail);
                }
                Pending::ResumeThread(ResumeTarget::Popup) | Pending::ThreadTurns { .. } => {
                    self.state.popup = None;
                    self.quota_warnings_ready = true;
                }
                Pending::ResumeThread(ResumeTarget::Picker) => {
                    self.quota_warnings_ready = true;
                    if let Some(picker) = self.state.resume_picker.as_mut() {
                        picker.loading = false;
                        picker.error = Some(detail);
                    }
                }
                Pending::StartTurn => {
                    self.state.turn_id = None;
                    self.state.turn_started_at = None;
                    self.flush_deferred_turn_events();
                }
                Pending::Account => {
                    self.state.popup = Some(Popup::Login {
                        url: None,
                        error: Some(format!("Could not check Codex account: {detail}")),
                    });
                }
                Pending::Login => {
                    self.state.popup = Some(Popup::Login {
                        url: None,
                        error: Some(detail),
                    });
                }
                Pending::Config => {
                    self.state.popup = Some(Popup::Disconnected {
                        reason: format!("Could not read Codex configuration: {detail}"),
                        selected: 0,
                    });
                }
                Pending::TrustProject { trust_target } => {
                    if let Some(Popup::TrustDirectory(prompt)) = self.state.popup.as_mut() {
                        if prompt.trust_target == trust_target {
                            prompt.saving = false;
                            prompt.error = Some(detail);
                        }
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        let result = message.get("result").cloned().unwrap_or(Value::Null);
        match pending {
            Pending::Initialize => {
                self.codex_version = result
                    .get("userAgent")
                    .and_then(Value::as_str)
                    .and_then(update::version_from_user_agent);
                self.after_initialize()
            }
            Pending::Account => self.apply_account(&result),
            Pending::RateLimits => {
                self.apply_rate_limits(&result);
                if std::mem::take(&mut self.rate_limits_refresh_needed) {
                    self.request_rate_limits()
                } else {
                    Ok(())
                }
            }
            Pending::Login => self.apply_login(&result),
            Pending::Models => {
                self.apply_models(&result);
                Ok(())
            }
            Pending::CollaborationModes => {
                self.apply_collaboration_modes(&result);
                Ok(())
            }
            Pending::Config => {
                self.apply_config(&result);
                if let Some(trust_target) = trust_target_from_config(&result, &self.state.cwd) {
                    self.state.popup = Some(Popup::TrustDirectory(TrustDirectoryPrompt {
                        cwd: self.state.cwd.clone(),
                        trust_target,
                        selected: 0,
                        saving: false,
                        error: None,
                    }));
                    Ok(())
                } else {
                    self.finish_startup()
                }
            }
            Pending::TrustProject { .. } => self.request_config(),
            Pending::StartThread => {
                self.load_thread_response(&result, false);
                self.startup_ready = true;
                self.maybe_show_update();
                self.quota_warnings_ready = true;
                self.request_rate_limits()
            }
            Pending::ListThreads { target, scope } => {
                self.apply_thread_list(&result, target, scope);
                Ok(())
            }
            Pending::ResumeThread(_) => {
                self.load_thread_response(&result, true);
                self.state.clear_blocks();
                self.state.reset_context_stats();
                self.state.push(TranscriptBlock::new(
                    BlockKind::Status,
                    "Codex",
                    "Loading conversation…",
                ));
                self.request_thread_turns(None)
            }
            Pending::ThreadTurns { thread_id } => self.apply_thread_turns(&thread_id, &result),
            Pending::StartTurn => {
                if let Some(turn_id) = result.pointer("/turn/id").and_then(Value::as_str) {
                    self.state.turn_id = Some(turn_id.to_string());
                }
                if self.state.turn_started_at.is_none() {
                    self.state.turn_started_at = Some(std::time::Instant::now());
                }
                Ok(())
            }
            Pending::SteerTurn => Ok(()),
            Pending::UpdateMode => Ok(()),
            Pending::Interrupt => Ok(()),
        }
    }

    fn after_initialize(&mut self) -> Result<()> {
        self.rpc.notify("initialized", json!({}))?;
        let account = self.rpc.request("account/read", json!({}))?;
        self.pending.insert(account, Pending::Account);
        Ok(())
    }

    fn after_authentication(&mut self) -> Result<()> {
        self.request_rate_limits()?;
        let models = self
            .rpc
            .request("model/list", json!({"includeHidden": false}))?;
        self.pending.insert(models, Pending::Models);
        let modes = self.rpc.request("collaborationMode/list", json!({}))?;
        self.pending.insert(modes, Pending::CollaborationModes);
        self.request_config()
    }

    fn request_rate_limits(&mut self) -> Result<()> {
        if self
            .pending
            .values()
            .any(|pending| matches!(pending, Pending::RateLimits))
        {
            return Ok(());
        }
        let id = self.rpc.request("account/rateLimits/read", json!({}))?;
        self.pending.insert(id, Pending::RateLimits);
        Ok(())
    }

    fn apply_rate_limits(&mut self, result: &Value) {
        let next = quota_usage_from_response(result);
        self.state.quota_usage = next;
        if !self.quota_warnings_ready {
            return;
        }
        let warning_snapshot = merge_quota_warning_state(self.warned_quota_usage, next);
        let warnings =
            quota_warning_messages(self.warned_quota_usage, warning_snapshot, unix_timestamp());
        self.warned_quota_usage = warning_snapshot;
        for warning in warnings {
            self.publish_or_defer_turn_event(DeferredTurnEvent::QuotaWarning(warning));
        }
    }

    fn publish_or_defer_turn_event(&mut self, event: DeferredTurnEvent) {
        if let Some(event) =
            defer_turn_event_if_active(&self.state, &mut self.deferred_turn_events, event)
        {
            self.publish_turn_event(event);
        }
    }

    fn publish_turn_event(&mut self, event: DeferredTurnEvent) {
        match event {
            DeferredTurnEvent::Transcript(block) => self.state.push(block),
            DeferredTurnEvent::QuotaWarning(warning) => {
                self.state.push(TranscriptBlock::new(
                    BlockKind::Status,
                    "Quota",
                    warning.clone(),
                ));
                self.notifier.quota_low(&self.state.project, &warning);
            }
        }
    }

    fn flush_deferred_turn_events(&mut self) {
        for event in std::mem::take(&mut self.deferred_turn_events) {
            self.publish_turn_event(event);
        }
    }

    fn request_config(&mut self) -> Result<()> {
        let config = self.rpc.request(
            "config/read",
            json!({"cwd": self.state.cwd, "includeLayers": true}),
        )?;
        self.pending.insert(config, Pending::Config);
        Ok(())
    }

    fn finish_startup(&mut self) -> Result<()> {
        if matches!(self.state.popup, Some(Popup::TrustDirectory(_))) {
            self.state.popup = None;
        }
        if std::mem::take(&mut self.startup_resume_pending) {
            self.state.resume_picker = Some(ResumePicker {
                selected: 0,
                loading: true,
                error: None,
                scope: ResumeScope::CurrentDirectory,
            });
            self.request_threads_for(ThreadListTarget::Picker, ResumeScope::CurrentDirectory)?;
            self.startup_ready = true;
            self.maybe_show_update();
        } else {
            self.start_thread()?;
        }
        Ok(())
    }

    fn start_update_check(&mut self) {
        let Some(sender) = self.update_sender.take() else {
            return;
        };
        tokio::spawn(async move {
            let _ = sender.send(update::check_latest_version().await);
        });
    }

    fn maybe_show_update(&mut self) {
        if !self.startup_ready || self.update_prompt_shown || self.state.popup.is_some() {
            return;
        }
        let (Some(current), Some(latest)) = (
            self.codex_version.as_deref(),
            self.latest_codex_version.as_ref(),
        ) else {
            return;
        };
        if latest.dismissed || !update::is_newer(&latest.version, current) {
            return;
        }
        self.state.popup = Some(Popup::Update {
            current: current.to_string(),
            latest: latest.version.clone(),
            selected: 0,
        });
        self.update_prompt_shown = true;
    }

    fn trust_project(&mut self) -> Result<()> {
        let Some(Popup::TrustDirectory(prompt)) = self.state.popup.as_mut() else {
            return Ok(());
        };
        if prompt.saving {
            return Ok(());
        }
        let trust_target = prompt.trust_target.clone();
        prompt.saving = true;
        prompt.error = None;
        let id = self
            .rpc
            .request("config/batchWrite", trust_write_params(&trust_target))?;
        self.pending
            .insert(id, Pending::TrustProject { trust_target });
        Ok(())
    }

    fn start_thread(&mut self) -> Result<()> {
        self.quota_warnings_ready = false;
        let mut params = json!({"cwd": self.state.cwd});
        if self.state.explicit_model {
            params["model"] = self
                .state
                .model
                .clone()
                .map(Value::String)
                .unwrap_or(Value::Null);
        }
        let id = self.rpc.request("thread/start", params)?;
        self.pending.insert(id, Pending::StartThread);
        Ok(())
    }

    fn apply_account(&mut self, result: &Value) -> Result<()> {
        if account_is_available(result) {
            return self.after_authentication();
        }
        self.begin_login()
    }

    fn begin_login(&mut self) -> Result<()> {
        let id = self.rpc.request(
            "account/login/start",
            json!({"type": "chatgpt", "appBrand": "codex"}),
        )?;
        self.pending.insert(id, Pending::Login);
        self.state.popup = Some(Popup::Login {
            url: None,
            error: None,
        });
        Ok(())
    }

    fn apply_login(&mut self, result: &Value) -> Result<()> {
        let Some(url) = result.get("authUrl").and_then(Value::as_str) else {
            self.state.popup = Some(Popup::Login {
                url: None,
                error: Some("App Server did not return an authentication URL".into()),
            });
            return Ok(());
        };
        open_browser(url);
        self.state.popup = Some(Popup::Login {
            url: Some(url.to_string()),
            error: None,
        });
        Ok(())
    }

    fn apply_models(&mut self, result: &Value) {
        self.state.models = result
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let id = item
                    .get("model")
                    .or_else(|| item.get("id"))?
                    .as_str()?
                    .to_string();
                let efforts = item
                    .get("supportedReasoningEfforts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|effort| {
                        effort
                            .get("reasoningEffort")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect();
                Some(ModelInfo {
                    name: item
                        .get("displayName")
                        .and_then(Value::as_str)
                        .unwrap_or(&id)
                        .to_string(),
                    default_effort: item
                        .get("defaultReasoningEffort")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                    id,
                    efforts,
                })
            })
            .collect();
    }

    fn apply_collaboration_modes(&mut self, result: &Value) {
        let modes = collaboration_modes_from_response(result);
        if !modes.is_empty() {
            self.state.collaboration_modes = modes;
        }
    }

    fn apply_config(&mut self, result: &Value) {
        let config = result.get("config").unwrap_or(result);
        let check_for_update = config
            .get("checkForUpdateOnStartup")
            .or_else(|| config.get("check_for_update_on_startup"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if check_for_update {
            self.start_update_check();
        } else {
            self.update_sender = None;
        }
        if !self.state.explicit_model {
            self.state.model = config
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if !self.state.explicit_effort {
            self.state.effort = config
                .get("modelReasoningEffort")
                .or_else(|| config.get("model_reasoning_effort"))
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
    }

    fn load_thread_response(&mut self, result: &Value, resumed: bool) {
        let Some(thread) = result.get("thread") else {
            return;
        };
        self.state.thread_id = thread.get("id").and_then(Value::as_str).map(str::to_owned);
        self.state.turn_started_at = None;
        self.state.reset_context_stats();
        self.state.model = result
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.state.model.clone());
        self.state.effort = result
            .get("reasoningEffort")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.state.effort.clone());
        self.state.clear_blocks();
        self.state.clear_message_history();
        if resumed {
            self.load_history(thread);
        }
        if self.state.blocks.is_empty() {
            self.state.push(TranscriptBlock::new(
                BlockKind::Status,
                "Codex",
                if resumed {
                    "Conversation resumed."
                } else {
                    "Ready."
                },
            ));
        }
        self.state.popup = None;
        self.state.resume_picker = None;
        self.state.scroll = usize::MAX;
        self.state.at_bottom = true;
    }

    fn load_history(&mut self, thread: &Value) {
        for turn in thread
            .get("turns")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let turn_id = turn.get("id").and_then(Value::as_str);
            for item in turn
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(block) = context_compaction_block(&mut self.state, item, turn_id) {
                    self.state.push(block);
                }
                if let Some(text) = user_message_text(item) {
                    self.state.remember_message(text);
                }
                if let Some(block) = block_from_item(item, true, self.state.show_reasoning) {
                    self.state.push(block);
                }
            }
            if let Some(duration_ms) = turn_duration_ms(turn) {
                self.state.push(turn_end_block(duration_ms));
            }
        }
    }

    fn request_threads(&mut self) -> Result<()> {
        self.request_threads_for(ThreadListTarget::Popup, ResumeScope::CurrentDirectory)
    }

    fn request_threads_for(&mut self, target: ThreadListTarget, scope: ResumeScope) -> Result<()> {
        self.state.threads.clear();
        match target {
            ThreadListTarget::Popup => {
                self.state.popup = Some(Popup::Resume {
                    selected: 0,
                    loading: true,
                    scope,
                });
            }
            ThreadListTarget::Picker => {
                self.state.resume_picker = Some(ResumePicker {
                    selected: 0,
                    loading: true,
                    error: None,
                    scope,
                });
            }
        }
        let id = self
            .rpc
            .request("thread/list", thread_list_params(&self.state.cwd, scope))?;
        self.pending
            .insert(id, Pending::ListThreads { target, scope });
        Ok(())
    }

    fn active_thread_list_scope(&self, target: ThreadListTarget) -> Option<ResumeScope> {
        match target {
            ThreadListTarget::Popup => match self.state.popup.as_ref() {
                Some(Popup::Resume { scope, .. }) => Some(*scope),
                _ => None,
            },
            ThreadListTarget::Picker => {
                self.state.resume_picker.as_ref().map(|picker| picker.scope)
            }
        }
    }

    fn apply_thread_list_error(
        &mut self,
        target: ThreadListTarget,
        scope: ResumeScope,
        detail: String,
    ) {
        if self.active_thread_list_scope(target) != Some(scope) {
            return;
        }
        match target {
            ThreadListTarget::Popup => {
                self.state.popup = Some(Popup::Resume {
                    selected: 0,
                    loading: false,
                    scope,
                });
            }
            ThreadListTarget::Picker => {
                self.state.resume_picker = Some(ResumePicker {
                    selected: 0,
                    loading: false,
                    error: Some(detail),
                    scope,
                });
            }
        }
    }

    fn apply_thread_list(&mut self, result: &Value, target: ThreadListTarget, scope: ResumeScope) {
        if self.active_thread_list_scope(target) != Some(scope) {
            return;
        }
        self.state.threads = thread_summaries_from_response(result);
        match target {
            ThreadListTarget::Popup => {
                self.state.popup = Some(Popup::Resume {
                    selected: 0,
                    loading: false,
                    scope,
                });
            }
            ThreadListTarget::Picker => {
                self.state.resume_picker = Some(ResumePicker {
                    selected: 0,
                    loading: false,
                    error: None,
                    scope,
                });
            }
        }
    }

    fn resume(&mut self, index: usize) -> Result<()> {
        let Some(thread) = self.state.threads.get(index) else {
            return Ok(());
        };
        self.state.queued_turns.clear();
        let id = self.rpc.request(
            "thread/resume",
            thread_resume_params(&thread.id, &self.state.cwd),
        )?;
        self.quota_warnings_ready = false;
        let (target, scope) = if let Some(picker) = self.state.resume_picker.as_ref() {
            (ResumeTarget::Picker, picker.scope)
        } else {
            let scope = match self.state.popup.as_ref() {
                Some(Popup::Resume { scope, .. }) => *scope,
                _ => ResumeScope::CurrentDirectory,
            };
            (ResumeTarget::Popup, scope)
        };
        self.pending.insert(id, Pending::ResumeThread(target));
        match target {
            ResumeTarget::Popup => {
                self.state.popup = Some(Popup::Resume {
                    selected: index,
                    loading: true,
                    scope,
                });
            }
            ResumeTarget::Picker => {
                if let Some(picker) = self.state.resume_picker.as_mut() {
                    picker.selected = index;
                    picker.loading = true;
                    picker.error = None;
                }
            }
        }
        Ok(())
    }

    fn request_thread_turns(&mut self, cursor: Option<String>) -> Result<()> {
        let Some(thread_id) = self.state.thread_id.clone() else {
            return Ok(());
        };
        let id = self.rpc.request(
            "thread/turns/list",
            json!({
                "threadId": thread_id,
                "cursor": cursor,
                "limit": 50,
                "sortDirection": "asc",
                "itemsView": "full"
            }),
        )?;
        self.pending.insert(id, Pending::ThreadTurns { thread_id });
        Ok(())
    }

    fn apply_thread_turns(&mut self, thread_id: &str, result: &Value) -> Result<()> {
        if self.state.thread_id.as_deref() != Some(thread_id) {
            return Ok(());
        }
        if self.state.blocks.len() == 1
            && self.state.blocks[0].kind == BlockKind::Status
            && self.state.blocks[0].text == "Loading conversation…"
        {
            self.state.clear_blocks();
        }
        for turn in result
            .get("data")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let turn_id = turn.get("id").and_then(Value::as_str);
            for item in turn
                .get("items")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(block) = context_compaction_block(&mut self.state, item, turn_id) {
                    self.state.push(block);
                }
                if let Some(text) = user_message_text(item) {
                    self.state.remember_message(text);
                }
                if let Some(block) = block_from_item(item, true, self.state.show_reasoning) {
                    self.state.push(block);
                }
            }
            if let Some(duration_ms) = turn_duration_ms(turn) {
                self.state.push(turn_end_block(duration_ms));
            }
        }
        if let Some(cursor) = result.get("nextCursor").and_then(Value::as_str) {
            self.request_thread_turns(Some(cursor.to_string()))?;
        } else {
            if self.state.blocks.is_empty() {
                self.state.push(TranscriptBlock::new(
                    BlockKind::Status,
                    "Codex",
                    "Conversation resumed.",
                ));
            }
            self.state.scroll = usize::MAX;
            self.state.at_bottom = true;
            self.state.popup = None;
            self.quota_warnings_ready = true;
            self.request_rate_limits()?;
        }
        Ok(())
    }

    fn send_composer(&mut self) -> Result<()> {
        let text = self.state.composer.text.trim().to_string();
        if text.is_empty() && self.state.image_attachments.is_empty() {
            return Ok(());
        }
        if text.starts_with('/') && !text.contains(char::is_whitespace) {
            self.state.composer.clear();
            return self.run_text_command(&text);
        }
        let Some(thread_id) = self.state.thread_id.clone() else {
            return Ok(());
        };
        let images = self.state.image_attachments.clone();
        let queued = QueuedTurn { text, images };
        if turn_in_progress(&self.state) {
            self.state.remember_message(queued.text.clone());
            self.state.composer.clear();
            self.state.image_attachments.clear();
            self.state.queued_turns.push_back(queued);
            return Ok(());
        }
        self.start_queued_turn(&thread_id, &queued)?;
        self.state.remember_message(queued.text.clone());
        self.state.composer.clear();
        self.state.image_attachments.clear();
        Ok(())
    }

    fn start_queued_turn(&mut self, thread_id: &str, queued: &QueuedTurn) -> Result<()> {
        let input = turn_input(&queued.text, &queued.images);
        let mut params = json!({
            "threadId": thread_id,
            "input": input
        });
        if self.state.explicit_model {
            if let Some(model) = &self.state.model {
                params["model"] = Value::String(model.clone());
            }
        }
        if self.state.explicit_effort {
            if let Some(effort) = &self.state.effort {
                params["effort"] = Value::String(effort.clone());
            }
        }
        if self.state.explicit_collaboration_mode {
            if let Some(mode) = self.current_collaboration_mode_payload() {
                params["collaborationMode"] = mode;
            }
        }
        let id = self.rpc.request("turn/start", params)?;
        self.state.push(TranscriptBlock::new(
            BlockKind::User,
            "You",
            user_input_display(&queued.text, &queued.images),
        ));
        self.pending.insert(id, Pending::StartTurn);
        self.state.turn_started_at = Some(std::time::Instant::now());
        Ok(())
    }

    fn start_next_queued_turn(&mut self) -> Result<()> {
        let Some((thread_id, queued)) = take_next_queued_turn(&mut self.state) else {
            return Ok(());
        };
        if let Err(error) = self.start_queued_turn(&thread_id, &queued) {
            self.state.queued_turns.push_front(queued);
            return Err(error);
        }
        Ok(())
    }

    pub fn handle_paste(&mut self, pasted: &str) {
        let pasted = pasted.replace("\r\n", "\n").replace('\r', "\n");
        if let Some(Popup::Approval(approval)) = self.state.popup.as_mut() {
            if approval.entering_feedback {
                approval.feedback.insert_str(&pasted);
            }
            return;
        }
        if let Some(Popup::UserInput(request)) = self.state.popup.as_mut() {
            if request.is_editing() {
                request.input.insert_str(&pasted);
            }
            return;
        }
        if self.state.popup.is_some() {
            return;
        }
        if self.state.resume_picker.is_some() {
            return;
        }
        if self.state.copy_mode.is_some() {
            return;
        }
        self.state.composer.insert_str(&pasted);
    }

    fn paste_clipboard(&mut self) {
        let mut clipboard = match Clipboard::new() {
            Ok(clipboard) => clipboard,
            Err(error) => {
                self.state.push(TranscriptBlock::new(
                    BlockKind::Error,
                    "Clipboard",
                    error.to_string(),
                ));
                return;
            }
        };

        if self.state.popup.is_none() && self.state.resume_picker.is_none() {
            if let Ok(image) = clipboard.get_image() {
                match encode_clipboard_image(image.width, image.height, &image.bytes) {
                    Ok(attachment) => self.state.image_attachments.push(attachment),
                    Err(error) => self.state.push(TranscriptBlock::new(
                        BlockKind::Error,
                        "Clipboard",
                        error.to_string(),
                    )),
                }
                return;
            }
        }

        match clipboard.get_text() {
            Ok(text) => self.handle_paste(&text),
            Err(error) => self.state.push(TranscriptBlock::new(
                BlockKind::Error,
                "Clipboard",
                format!("No text or image available: {error}"),
            )),
        }
    }

    fn run_text_command(&mut self, command: &str) -> Result<()> {
        match command {
            "/new" => self.new_conversation(),
            "/resume" => self.request_threads(),
            "/mode" => {
                self.open_collaboration_modes();
                Ok(())
            }
            "/model" => {
                self.open_models();
                Ok(())
            }
            "/reasoning" => {
                self.open_reasoning();
                Ok(())
            }
            "/history" => {
                self.open_history();
                Ok(())
            }
            "/bottom" => {
                self.state.jump_to_bottom();
                Ok(())
            }
            "/copy" => {
                enter_copy_mode(&mut self.state);
                Ok(())
            }
            _ => {
                self.state.push(TranscriptBlock::new(
                    BlockKind::Error,
                    "Unknown command",
                    command,
                ));
                Ok(())
            }
        }
    }

    fn new_conversation(&mut self) -> Result<()> {
        if turn_in_progress(&self.state) {
            return Ok(());
        }
        self.state.popup = None;
        self.state.queued_turns.clear();
        self.state.clear_message_history();
        self.start_thread()
    }

    fn open_models(&mut self) {
        let selected = self
            .state
            .model
            .as_ref()
            .and_then(|current| self.state.models.iter().position(|m| &m.id == current))
            .unwrap_or(0);
        self.state.popup = Some(Popup::Models { selected });
    }

    fn open_history(&mut self) {
        let message_count = self
            .state
            .blocks
            .iter()
            .filter(|block| block.kind == BlockKind::User)
            .count();
        self.state.popup = Some(Popup::History {
            selected: message_count.saturating_sub(1),
        });
    }

    fn open_collaboration_modes(&mut self) {
        let selected = self
            .state
            .collaboration_modes
            .iter()
            .position(|mode| mode.id == self.state.collaboration_mode)
            .unwrap_or(0);
        self.state.popup = Some(Popup::CollaborationModes { selected });
    }

    fn current_collaboration_mode_payload(&self) -> Option<Value> {
        collaboration_mode_payload(&self.state)
    }

    fn select_collaboration_mode(&mut self, selected: usize) -> Result<()> {
        let Some(mode) = self.state.collaboration_modes.get(selected).cloned() else {
            self.state.popup = None;
            return Ok(());
        };
        apply_collaboration_mode(&mut self.state, &mode);
        self.state.popup = None;

        let (Some(thread_id), Some(mode)) = (
            self.state.thread_id.clone(),
            self.current_collaboration_mode_payload(),
        ) else {
            return Ok(());
        };
        let id = self.rpc.request(
            "thread/settings/update",
            json!({"threadId": thread_id, "collaborationMode": mode}),
        )?;
        self.pending.insert(id, Pending::UpdateMode);
        Ok(())
    }

    fn toggle_collaboration_mode(&mut self) -> Result<()> {
        let Some(selected) = alternate_collaboration_mode_index(&self.state) else {
            return Ok(());
        };
        self.select_collaboration_mode(selected)
    }

    fn supported_efforts(&self) -> Vec<String> {
        self.state
            .model
            .as_ref()
            .and_then(|current| self.state.models.iter().find(|model| &model.id == current))
            .map(|model| model.efforts.clone())
            .unwrap_or_default()
    }

    fn open_reasoning(&mut self) {
        let efforts = self.supported_efforts();
        let selected = self
            .state
            .effort
            .as_ref()
            .and_then(|current| efforts.iter().position(|effort| effort == current))
            .unwrap_or(0);
        self.state.popup = Some(Popup::Reasoning { selected });
    }

    fn interrupt(&mut self) -> Result<()> {
        let (Some(thread_id), Some(turn_id)) =
            (self.state.thread_id.clone(), self.state.turn_id.clone())
        else {
            return Ok(());
        };
        let id = self.rpc.request(
            "turn/interrupt",
            json!({"threadId": thread_id, "turnId": turn_id}),
        )?;
        self.pending.insert(id, Pending::Interrupt);
        Ok(())
    }

    pub async fn restart(&mut self) -> Result<()> {
        self.rpc = RpcClient::spawn(self.debug, self.default_mode_request_user_input).await?;
        self.pending.clear();
        self.state.popup = None;
        self.state.thread_id = None;
        self.state.turn_id = None;
        self.state.turn_started_at = None;
        self.state.queued_turns.clear();
        self.state.pending_server_requests.clear();
        self.initialize()
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        let key = normalize_control_shortcut(key);
        self.state.copy_feedback = None;
        if self.state.copy_mode.is_some() {
            if let Some(text) = handle_copy_mode_key(&mut self.state, key) {
                self.copy_text(text);
            }
            return Ok(());
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if prepare_control_c(&mut self.state) == ControlCAction::Interrupt {
                return self.interrupt();
            }
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('v') {
            self.paste_clipboard();
            return Ok(());
        }

        if let Some(direction) = control_scroll_direction(key) {
            if !scroll_expanded_approval(&mut self.state, direction, 3) {
                match direction {
                    ScrollDirection::Up => self.scroll_up(3),
                    ScrollDirection::Down => self.scroll_down(3),
                }
            }
            return Ok(());
        }

        if key.code == KeyCode::Esc && self.state.popup.is_none() && self.state.turn_id.is_some() {
            return self.interrupt();
        }

        if self.state.popup.is_some() {
            self.handle_popup_key(key)?;
            self.maybe_show_update();
            return Ok(());
        }

        if self.state.resume_picker.is_some() {
            return self.handle_resume_picker_key(key);
        }

        if handle_composer_arrow(&mut self.state.composer, key, self.state.composer_width) {
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('p') => self.state.previous_message(),
                KeyCode::Char('n') => self.state.next_message(),
                KeyCode::Char('g') => self.state.jump_to_bottom(),
                KeyCode::Char('o') => toggle_latest_command(&mut self.state),
                KeyCode::Char('y') => enter_copy_mode(&mut self.state),
                _ => {}
            }
            return Ok(());
        }

        match key.code {
            KeyCode::Tab => self.toggle_collaboration_mode()?,
            KeyCode::Enter if is_newline_key(key) => self.state.composer.newline(),
            KeyCode::Enter => self.send_composer()?,
            KeyCode::Char(ch) => self.state.composer.insert(ch),
            KeyCode::Backspace => self.state.composer.backspace(),
            KeyCode::Delete => self.state.composer.delete(),
            _ => {}
        }
        Ok(())
    }

    fn copy_text(&mut self, text: String) {
        let result = Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text));
        self.state.copy_feedback = Some(match result {
            Ok(()) => "Copied".into(),
            Err(error) => format!("Copy failed: {error}"),
        });
    }

    fn handle_resume_picker_key(&mut self, key: KeyEvent) -> Result<()> {
        let key = normalize_list_navigation(key);
        let Some(picker) = self.state.resume_picker.as_ref() else {
            return Ok(());
        };
        if picker.loading {
            return Ok(());
        }
        if key.code == KeyCode::Tab {
            return self.request_threads_for(ThreadListTarget::Picker, picker.scope.toggled());
        }
        let selected = picker.selected;
        if let Some(next) = jk_list_selection(key.code, selected, self.state.threads.len()) {
            if let Some(picker) = self.state.resume_picker.as_mut() {
                picker.selected = next;
            }
            return Ok(());
        }
        if key.code == KeyCode::Enter {
            return self.resume(selected);
        }
        Ok(())
    }

    fn handle_popup_key(&mut self, key: KeyEvent) -> Result<()> {
        if let Some(Popup::Update {
            latest, selected, ..
        }) = self.state.popup.as_mut()
        {
            let key = normalize_list_navigation(key);
            if let Some(next) = jk_list_selection(key.code, *selected, 3) {
                *selected = next;
                return Ok(());
            }
            match key.code {
                KeyCode::Enter if *selected == 0 => {
                    self.update_codex_on_exit = true;
                    self.state.quit = true;
                }
                KeyCode::Enter if *selected == 1 => self.state.popup = None,
                KeyCode::Enter if *selected == 2 => {
                    let latest = latest.clone();
                    self.state.popup = None;
                    if let Err(error) = update::dismiss_version(&latest) {
                        self.state.push(TranscriptBlock::new(
                            BlockKind::Error,
                            "Codex update",
                            format!("Could not remember skipped version: {error}"),
                        ));
                    }
                }
                KeyCode::Esc => self.state.popup = None,
                _ => {}
            }
            return Ok(());
        }
        if let Some(Popup::Disconnected { selected, .. }) = self.state.popup.as_mut() {
            let key = normalize_list_navigation(key);
            match key.code {
                KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Char('j') => *selected = (*selected + 1).min(1),
                KeyCode::Enter if *selected == 1 => self.state.quit = true,
                KeyCode::Esc => self.state.quit = true,
                _ => {}
            }
            return Ok(());
        }
        if let Some(Popup::TrustDirectory(prompt)) = self.state.popup.as_mut() {
            if prompt.saving {
                return Ok(());
            }
            let key = normalize_list_navigation(key);
            match key.code {
                KeyCode::Char('k') | KeyCode::Up => prompt.selected = 0,
                KeyCode::Char('j') | KeyCode::Down => prompt.selected = 1,
                KeyCode::Enter if prompt.selected == 0 => return self.trust_project(),
                KeyCode::Enter | KeyCode::Esc => self.state.quit = true,
                _ => {}
            }
            return Ok(());
        }
        if let Some(Popup::Approval(_)) = &self.state.popup {
            return self.handle_approval_key(key);
        }
        if let Some(Popup::UserInput(_)) = &self.state.popup {
            return self.handle_user_input_key(key);
        }
        if key.code == KeyCode::Tab {
            if let Some(Popup::Resume {
                scope,
                loading: false,
                ..
            }) = self.state.popup.as_ref()
            {
                return self.request_threads_for(ThreadListTarget::Popup, scope.toggled());
            }
        }
        if key.code == KeyCode::Esc {
            if matches!(self.state.popup, Some(Popup::Login { .. })) {
                return Ok(());
            }
            self.state.popup = None;
            return Ok(());
        }

        let key = normalize_list_navigation(key);
        match self.state.popup.clone() {
            Some(Popup::Models { selected }) => match key.code {
                KeyCode::Char('k') => {
                    self.state.popup = Some(Popup::Models {
                        selected: selected.saturating_sub(1),
                    });
                }
                KeyCode::Char('j') => {
                    self.state.popup = Some(Popup::Models {
                        selected: (selected + 1).min(self.state.models.len().saturating_sub(1)),
                    });
                }
                KeyCode::Enter => {
                    if let Some(model) = self.state.models.get(selected).cloned() {
                        self.state.model = Some(model.id);
                        self.state.explicit_model = true;
                        if !model
                            .efforts
                            .iter()
                            .any(|effort| Some(effort) == self.state.effort.as_ref())
                        {
                            self.state.effort = model.default_effort;
                            self.state.explicit_effort = false;
                        }
                    }
                    self.state.popup = None;
                }
                _ => {}
            },
            Some(Popup::CollaborationModes { selected }) => match key.code {
                KeyCode::Char('k') | KeyCode::Up => {
                    self.state.popup = Some(Popup::CollaborationModes {
                        selected: selected.saturating_sub(1),
                    });
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.state.popup = Some(Popup::CollaborationModes {
                        selected: (selected + 1)
                            .min(self.state.collaboration_modes.len().saturating_sub(1)),
                    });
                }
                KeyCode::Enter => return self.select_collaboration_mode(selected),
                _ => {}
            },
            Some(Popup::Reasoning { selected }) => {
                let efforts = self.supported_efforts();
                match key.code {
                    KeyCode::Char('k') => {
                        self.state.popup = Some(Popup::Reasoning {
                            selected: selected.saturating_sub(1),
                        });
                    }
                    KeyCode::Char('j') => {
                        self.state.popup = Some(Popup::Reasoning {
                            selected: (selected + 1).min(efforts.len().saturating_sub(1)),
                        });
                    }
                    KeyCode::Enter => {
                        if let Some(effort) = efforts.get(selected) {
                            self.state.effort = Some(effort.clone());
                            self.state.explicit_effort = true;
                        }
                        self.state.popup = None;
                    }
                    _ => {}
                }
            }
            Some(Popup::Resume {
                selected,
                loading: false,
                scope,
            }) => {
                if let Some(next) = jk_list_selection(key.code, selected, self.state.threads.len())
                {
                    self.state.popup = Some(Popup::Resume {
                        selected: next,
                        loading: false,
                        scope,
                    });
                    return Ok(());
                }
                if key.code == KeyCode::Enter {
                    return self.resume(selected);
                }
            }
            Some(Popup::History { selected }) => match key.code {
                KeyCode::Char('k') | KeyCode::Up => {
                    self.state.popup = Some(Popup::History {
                        selected: selected.saturating_sub(1),
                    });
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    let message_count = self
                        .state
                        .blocks
                        .iter()
                        .filter(|block| block.kind == BlockKind::User)
                        .count();
                    self.state.popup = Some(Popup::History {
                        selected: (selected + 1).min(message_count.saturating_sub(1)),
                    });
                }
                KeyCode::Enter => {
                    self.state.jump_to_user_message(selected);
                    self.state.popup = None;
                }
                _ => {}
            },
            _ => {}
        }
        Ok(())
    }

    fn handle_approval_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(Popup::Approval(current)) = self.state.popup.as_mut() else {
            return Ok(());
        };
        let key = normalize_approval_key(key, current.entering_feedback);
        if current.entering_feedback {
            if handle_composer_arrow(&mut current.feedback, key, self.state.composer_width) {
                return Ok(());
            }
            match key.code {
                KeyCode::Esc => {
                    current.entering_feedback = false;
                    current.feedback.clear();
                }
                KeyCode::Enter if is_newline_key(key) => current.feedback.newline(),
                KeyCode::Enter => {
                    if current.feedback.text.trim().is_empty() {
                        return Ok(());
                    }
                    return self.finish_approval(ApprovalChoice::DenyWithFeedback);
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    current.feedback.insert(ch)
                }
                KeyCode::Backspace => current.feedback.backspace(),
                KeyCode::Delete => current.feedback.delete(),
                _ => {}
            }
            return Ok(());
        }
        let option_count = if matches!(current.kind, ApprovalKind::Unsupported) {
            1
        } else {
            5
        };
        match key.code {
            KeyCode::Char('k') => {
                current.selected = current.selected.saturating_sub(1);
                return Ok(());
            }
            KeyCode::Char('j') => {
                current.selected = (current.selected + 1).min(option_count - 1);
                return Ok(());
            }
            KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                current.expanded = !current.expanded;
                current.scroll = 0;
                current.max_scroll = 0;
                return Ok(());
            }
            _ => {}
        }
        let unsupported = matches!(current.kind, ApprovalKind::Unsupported);
        let decision = match key.code {
            KeyCode::Enter if unsupported => Some(ApprovalChoice::Deny),
            KeyCode::Enter => match current.selected {
                0 => Some(ApprovalChoice::Allow),
                1 => Some(ApprovalChoice::Session),
                2 => Some(ApprovalChoice::Deny),
                3 => {
                    current.entering_feedback = true;
                    return Ok(());
                }
                4 => Some(ApprovalChoice::Cancel),
                _ => None,
            },
            KeyCode::Esc if unsupported => Some(ApprovalChoice::Deny),
            KeyCode::Esc => Some(ApprovalChoice::Cancel),
            _ => None,
        };
        let Some(decision) = decision else {
            return Ok(());
        };
        self.finish_approval(decision)
    }

    fn finish_approval(&mut self, decision: ApprovalChoice) -> Result<()> {
        let Some(Popup::Approval(approval)) = self.state.popup.take() else {
            return Ok(());
        };
        let result = approval_result(&approval, decision);
        let feedback = approval_feedback(&approval, decision);
        let steer = matches!(decision, ApprovalChoice::DenyWithFeedback)
            .then(|| approval.feedback.text.trim().to_string());
        if matches!(approval.kind, ApprovalKind::Unsupported) {
            self.rpc
                .respond_error(approval.id, "unsupported client request")?;
        } else {
            self.rpc.respond(approval.id, result)?;
        }
        self.state.push(TranscriptBlock::new(
            BlockKind::Status,
            "Approval",
            feedback,
        ));
        if let Some(steer) = steer {
            self.steer_active_turn(steer)?;
        }
        self.state.show_next_server_prompt();
        Ok(())
    }

    fn steer_active_turn(&mut self, text: String) -> Result<()> {
        let (Some(thread_id), Some(turn_id)) =
            (self.state.thread_id.clone(), self.state.turn_id.clone())
        else {
            self.state.push(TranscriptBlock::new(
                BlockKind::Error,
                "Error",
                "Could not send approval feedback because the active turn ended",
            ));
            return Ok(());
        };
        let id = self
            .rpc
            .request("turn/steer", turn_steer_params(&thread_id, &turn_id, &text))?;
        self.pending.insert(id, Pending::SteerTurn);
        Ok(())
    }

    fn handle_user_input_key(&mut self, key: KeyEvent) -> Result<()> {
        let Some(Popup::UserInput(request)) = self.state.popup.as_mut() else {
            return Ok(());
        };
        let Some(question) = request.current_question() else {
            return self.finish_user_input(false);
        };
        let editing = request.is_editing();

        if editing {
            if handle_composer_arrow(&mut request.input, key, self.state.composer_width) {
                return Ok(());
            }
            match key.code {
                KeyCode::Esc if request.entering_other => {
                    request.entering_other = false;
                    request.input.clear();
                    return Ok(());
                }
                KeyCode::Esc => return self.finish_user_input(true),
                KeyCode::Enter if is_newline_key(key) => {
                    request.input.newline();
                    return Ok(());
                }
                KeyCode::Enter => {
                    let answer = request.input.text.trim().to_string();
                    if answer.is_empty() {
                        return Ok(());
                    }
                    return self.answer_user_input(format!("user_note: {answer}"));
                }
                KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    request.input.insert(ch)
                }
                KeyCode::Backspace => request.input.backspace(),
                KeyCode::Delete => request.input.delete(),
                _ => {}
            }
            return Ok(());
        }

        let key = normalize_list_navigation(key);
        let option_count = question.options.len() + usize::from(question.allow_other);
        if let Some(selected) = jk_list_selection(key.code, request.selected, option_count) {
            request.selected = selected;
            return Ok(());
        }
        match key.code {
            KeyCode::Enter if request.selected < question.options.len() => {
                let answer = question.options[request.selected].label.clone();
                return self.answer_user_input(answer);
            }
            KeyCode::Enter if question.allow_other => {
                request.entering_other = true;
                request.input.clear();
            }
            KeyCode::Esc => return self.finish_user_input(true),
            _ => {}
        }
        Ok(())
    }

    fn answer_user_input(&mut self, answer: String) -> Result<()> {
        let Some(Popup::UserInput(request)) = self.state.popup.as_mut() else {
            return Ok(());
        };
        let Some(question_id) = request
            .current_question()
            .map(|question| question.id.clone())
        else {
            return self.finish_user_input(false);
        };
        request.answers.push((question_id, vec![answer]));
        request.current += 1;
        request.selected = 0;
        request.input.clear();
        request.entering_other = false;
        if request.current == request.questions.len() {
            self.finish_user_input(false)?;
        }
        Ok(())
    }

    fn finish_user_input(&mut self, cancelled: bool) -> Result<()> {
        let Some(Popup::UserInput(request)) = self.state.popup.take() else {
            return Ok(());
        };
        if cancelled {
            self.rpc
                .respond_error(request.id, "request_user_input cancelled by user")?;
            self.state.push(TranscriptBlock::new(
                BlockKind::Status,
                "Question",
                "Question cancelled.",
            ));
        } else {
            let response = user_input_response(&request);
            let summary = user_input_answer_summary(&request);
            self.rpc.respond(request.id, response)?;
            self.state
                .push(TranscriptBlock::new(BlockKind::Status, "Question", summary));
        }
        self.state.show_next_server_prompt();
        Ok(())
    }

    fn handle_server_request(&mut self, message: Value) -> Result<()> {
        let id = message.get("id").cloned().unwrap_or(Value::Null);
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        if method == "item/tool/requestUserInput" {
            if let Some(request) = parse_user_input_request(id.clone(), &params) {
                let detail = request
                    .current_question()
                    .map(|question| question.question.as_str())
                    .unwrap_or("Codex asked a question");
                self.notify_action_required(detail);
                self.state
                    .present_server_prompt(ServerPrompt::UserInput(request));
            } else {
                self.rpc
                    .respond_error(id, "invalid request_user_input payload")?;
            }
            return Ok(());
        }
        let (kind, title, detail) = match method.as_str() {
            "item/commandExecution/requestApproval" => (
                ApprovalKind::Command,
                "Run command?".to_string(),
                approval_command_display(&params),
            ),
            "item/fileChange/requestApproval" => (
                ApprovalKind::File,
                "Apply file changes?".to_string(),
                params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex requested permission to change files")
                    .to_string(),
            ),
            "item/permissions/requestApproval" => (
                ApprovalKind::Permissions,
                "Grant additional permissions?".to_string(),
                params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex requested additional permissions")
                    .to_string(),
            ),
            "execCommandApproval" => (
                ApprovalKind::Legacy,
                "Run command?".to_string(),
                params
                    .get("command")
                    .and_then(Value::as_array)
                    .map(|parts| {
                        parts
                            .iter()
                            .filter_map(Value::as_str)
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .unwrap_or_else(|| "Command execution".into()),
            ),
            "applyPatchApproval" => (
                ApprovalKind::Legacy,
                "Apply patch?".to_string(),
                params
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("Codex requested permission to modify files")
                    .to_string(),
            ),
            other => (
                ApprovalKind::Unsupported,
                "Unsupported server request".to_string(),
                format!("{other}\nThis request will not be approved."),
            ),
        };
        let approval = Approval {
            id,
            reason: matches!(kind, ApprovalKind::Command | ApprovalKind::Legacy)
                .then(|| {
                    params
                        .get("reason")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|reason| !reason.is_empty())
                        .map(str::to_owned)
                })
                .flatten(),
            kind,
            title,
            detail,
            params,
            selected: 0,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: false,
        };
        self.notify_action_required(&approval.title);
        // Server prompts take precedence over navigation popups: the server is
        // blocked until the client answers them.
        self.state
            .present_server_prompt(ServerPrompt::Approval(approval));
        Ok(())
    }

    fn handle_notification(&mut self, method: &str, params: Value) -> Result<()> {
        match method {
            "turn/started" => {
                self.state.jump_to_bottom();
                self.state.turn_id = params
                    .pointer("/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if self.state.turn_started_at.is_none() {
                    self.state.turn_started_at = Some(std::time::Instant::now());
                }
            }
            "turn/completed" => {
                let has_queued_turn = !self.state.queued_turns.is_empty();
                let error = params
                    .pointer("/turn/error/message")
                    .and_then(Value::as_str);
                if let Some(error) = error {
                    self.state
                        .push(TranscriptBlock::new(BlockKind::Error, "Error", error));
                }
                match params.pointer("/turn/status").and_then(Value::as_str) {
                    Some("completed") if !has_queued_turn => self.notify_response_ready(),
                    Some("completed") => {}
                    Some("failed") => self.notify_turn_failed(),
                    Some("interrupted") | Some("inProgress") => {}
                    Some(_) => {}
                    None if error.is_some() => self.notify_turn_failed(),
                    None if !has_queued_turn => self.notify_response_ready(),
                    None => {}
                }
                let duration_ms = params
                    .pointer("/turn/durationMs")
                    .and_then(Value::as_u64)
                    .or_else(|| {
                        self.state.turn_started_at.map(|started| {
                            started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
                        })
                    });
                self.state.turn_id = None;
                self.state.turn_started_at = None;
                if let Some(duration_ms) = duration_ms {
                    self.state.push(turn_end_block(duration_ms));
                }
                self.flush_deferred_turn_events();
                self.start_next_queued_turn()?;
            }
            "item/started" | "item/completed" => {
                if let Some(item) = params.get("item") {
                    let completed = method == "item/completed";
                    if completed {
                        let turn_id = params.get("turnId").and_then(Value::as_str);
                        if let Some(block) =
                            context_compaction_block(&mut self.state, item, turn_id)
                        {
                            self.publish_or_defer_turn_event(DeferredTurnEvent::Transcript(block));
                        }
                    }
                    if let Some(block) = block_from_item(item, completed, self.state.show_reasoning)
                    {
                        if block.kind == BlockKind::User {
                            if let Some(existing) = self.state.blocks.iter_mut().rev().find(|b| {
                                b.kind == BlockKind::User && b.id.is_none() && b.text == block.text
                            }) {
                                existing.id = block.id;
                                return Ok(());
                            }
                        }
                        if let Some(id) = block.id.clone() {
                            self.state.upsert(&id, block);
                        } else {
                            self.state.push(block);
                        }
                    }
                }
            }
            "item/agentMessage/delta" => {
                append_delta(&mut self.state, &params, BlockKind::Commentary, "Codex")
            }
            "item/reasoning/summaryTextDelta" if self.state.show_reasoning => {
                append_delta(&mut self.state, &params, BlockKind::Reasoning, "Thinking")
            }
            "item/commandExecution/outputDelta" => {
                append_delta(&mut self.state, &params, BlockKind::Command, "$ command")
            }
            "item/fileChange/patchUpdated" => update_file_change_patch(&mut self.state, &params),
            "thread/tokenUsage/updated" => {
                let matches_thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .zip(self.state.thread_id.as_deref())
                    .is_none_or(|(event, current)| event == current);
                if matches_thread {
                    self.state.context_usage = context_usage(&params);
                }
            }
            "thread/compacted" => {
                let matches_thread = params
                    .get("threadId")
                    .and_then(Value::as_str)
                    .zip(self.state.thread_id.as_deref())
                    .is_some_and(|(event, current)| event == current);
                if matches_thread {
                    if let Some(block) = legacy_context_compaction_block(&mut self.state, &params) {
                        self.publish_or_defer_turn_event(DeferredTurnEvent::Transcript(block));
                    }
                }
            }
            "thread/settings/updated" => {
                if let Some(mode) = params
                    .pointer("/threadSettings/collaborationMode/mode")
                    .and_then(Value::as_str)
                {
                    self.state.collaboration_mode = mode.to_string();
                }
                if let Some(model) = params
                    .pointer("/threadSettings/model")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        params
                            .pointer("/threadSettings/collaborationMode/settings/model")
                            .and_then(Value::as_str)
                    })
                {
                    self.state.model = Some(model.to_string());
                }
                self.state.effort = params
                    .pointer("/threadSettings/effort")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        params
                            .pointer("/threadSettings/collaborationMode/settings/reasoning_effort")
                            .and_then(Value::as_str)
                    })
                    .map(str::to_owned);
            }
            "account/login/completed" => {
                if params.get("success").and_then(Value::as_bool) == Some(true) {
                    self.state.popup = None;
                    self.state.push(TranscriptBlock::new(
                        BlockKind::Status,
                        "Account",
                        "Signed in with ChatGPT.",
                    ));
                    self.after_authentication()?;
                } else {
                    self.state.popup = Some(Popup::Login {
                        url: None,
                        error: Some(
                            params
                                .get("error")
                                .and_then(Value::as_str)
                                .unwrap_or("Login failed")
                                .to_string(),
                        ),
                    });
                }
            }
            "account/rateLimits/updated" => {
                if self
                    .pending
                    .values()
                    .any(|pending| matches!(pending, Pending::RateLimits))
                {
                    self.rate_limits_refresh_needed = true;
                } else {
                    self.request_rate_limits()?;
                }
            }
            "serverRequest/resolved" => {
                if let Some(id) = params.get("requestId") {
                    self.state.resolve_server_prompt(id);
                }
            }
            "error" => {
                let message = params
                    .pointer("/error/message")
                    .or_else(|| params.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Unknown App Server error");
                self.state
                    .push(TranscriptBlock::new(BlockKind::Error, "Error", message));
            }
            _ => {}
        }
        Ok(())
    }

    pub fn set_terminal_focused(&self, focused: bool) {
        self.notifier.set_terminal_focused(focused);
    }

    fn notify_action_required(&self, detail: &str) {
        self.notifier.action_required(&self.state.project, detail);
    }

    fn notify_response_ready(&self) {
        self.notifier.response_ready(&self.state.project);
    }

    fn notify_turn_failed(&self) {
        self.notifier.turn_failed(&self.state.project);
    }

    pub fn handle_mouse(&mut self, event: MouseEvent) {
        let approval_direction = match event.kind {
            MouseEventKind::ScrollUp => Some(ScrollDirection::Up),
            MouseEventKind::ScrollDown => Some(ScrollDirection::Down),
            _ => None,
        };
        if approval_direction
            .is_some_and(|direction| scroll_expanded_approval(&mut self.state, direction, 3))
        {
            return;
        }
        if self.state.popup.is_some() {
            return;
        }
        match event.kind {
            MouseEventKind::ScrollUp => self.scroll_up(3),
            MouseEventKind::ScrollDown => self.scroll_down(3),
            _ => {}
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        if self.state.scroll == usize::MAX {
            self.state.scroll = self.state.transcript_max_scroll;
        }
        self.state.scroll = self.state.scroll.saturating_sub(amount);
        self.state.at_bottom = false;
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.state.scroll = self.state.scroll.saturating_add(amount);
    }

    pub async fn shutdown(self) {
        self.rpc.shutdown().await;
    }

    pub fn update_codex_on_exit(&self) -> bool {
        self.update_codex_on_exit
    }
}

fn defer_turn_event_if_active(
    state: &AppState,
    deferred: &mut Vec<DeferredTurnEvent>,
    event: DeferredTurnEvent,
) -> Option<DeferredTurnEvent> {
    if state.turn_id.is_some() || state.turn_started_at.is_some() {
        deferred.push(event);
        None
    } else {
        Some(event)
    }
}

fn turn_in_progress(state: &AppState) -> bool {
    state.turn_id.is_some() || state.turn_started_at.is_some()
}

fn take_next_queued_turn(state: &mut AppState) -> Option<(String, QueuedTurn)> {
    if turn_in_progress(state) {
        return None;
    }
    let thread_id = state.thread_id.clone()?;
    let queued = state.queued_turns.pop_front()?;
    Some((thread_id, queued))
}

fn toggle_latest_command(state: &mut AppState) {
    let expanded = state
        .blocks
        .iter_mut()
        .rev()
        .find(|block| {
            matches!(
                block.kind,
                BlockKind::Command | BlockKind::File | BlockKind::Web
            )
        })
        .and_then(|block| {
            if block.kind != BlockKind::Command {
                return None;
            }
            block.expanded = !block.expanded;
            Some(block.expanded)
        });
    if let Some(expanded) = expanded {
        state.mark_transcript_dirty();
        if expanded {
            state.jump_to_bottom();
        }
    }
}

fn enter_copy_mode(state: &mut AppState) {
    let Some(block_index) = state
        .blocks
        .iter()
        .rposition(|block| block.kind == BlockKind::Assistant && !block.text.trim().is_empty())
    else {
        state.copy_feedback = Some("No assistant response to copy".into());
        return;
    };
    state.copy_mode = Some(CopyMode::Answers { block_index });
    state.mark_transcript_dirty();
    scroll_to_transcript_block(state, block_index);
}

fn handle_copy_mode_key(state: &mut AppState, key: KeyEvent) -> Option<String> {
    let key = normalize_copy_navigation(key);
    if key.code == KeyCode::Esc
        || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
    {
        match state.copy_mode.clone() {
            Some(CopyMode::Markdown { block_index, .. }) if key.code == KeyCode::Esc => {
                state.copy_mode = Some(CopyMode::Answers { block_index });
            }
            Some(_) => state.copy_mode = None,
            None => {}
        }
        state.mark_transcript_dirty();
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }

    match state.copy_mode.clone()? {
        CopyMode::Answers { block_index } => {
            let assistants = assistant_block_indices(state);
            let position = assistants
                .iter()
                .position(|candidate| *candidate == block_index)?;
            let next_position = match key.code {
                KeyCode::Char('k') => position.saturating_sub(1),
                KeyCode::Char('j') => (position + 1).min(assistants.len().saturating_sub(1)),
                KeyCode::Char('g') => 0,
                KeyCode::Char('G') => assistants.len().saturating_sub(1),
                KeyCode::Enter => {
                    let count = markdown_copy_ranges(&state.blocks[block_index].text).len();
                    if count > 0 {
                        state.copy_mode = Some(CopyMode::Markdown {
                            block_index,
                            markdown_index: 0,
                            selected: vec![],
                        });
                        state.mark_transcript_dirty();
                        reveal_markdown_block(state, block_index, 0);
                    }
                    return None;
                }
                KeyCode::Char('y' | 'Y') => {
                    let text = state.blocks[block_index].text.clone();
                    state.copy_mode = None;
                    state.mark_transcript_dirty();
                    return Some(text);
                }
                _ => return None,
            };
            let next = assistants[next_position];
            if next != block_index {
                state.copy_mode = Some(CopyMode::Answers { block_index: next });
                state.mark_transcript_dirty();
                scroll_to_transcript_block(state, next);
            }
        }
        CopyMode::Markdown {
            block_index,
            markdown_index,
            mut selected,
        } => {
            let ranges = markdown_copy_ranges(&state.blocks[block_index].text);
            if ranges.is_empty() {
                state.copy_mode = Some(CopyMode::Answers { block_index });
                state.mark_transcript_dirty();
                return None;
            }
            let next_index = match key.code {
                KeyCode::Char('k') => Some(markdown_index.saturating_sub(1)),
                KeyCode::Char('j') => Some((markdown_index + 1).min(ranges.len() - 1)),
                KeyCode::Char('g') => Some(0),
                KeyCode::Char('G') => Some(ranges.len() - 1),
                KeyCode::Enter => {
                    match selected.binary_search(&markdown_index) {
                        Ok(position) => {
                            selected.remove(position);
                        }
                        Err(position) => selected.insert(position, markdown_index),
                    }
                    state.copy_mode = Some(CopyMode::Markdown {
                        block_index,
                        markdown_index,
                        selected,
                    });
                    state.mark_transcript_dirty();
                    return None;
                }
                KeyCode::Char('y' | 'Y') => {
                    let chosen = if selected.is_empty() {
                        vec![markdown_index]
                    } else {
                        selected
                    };
                    let source = &state.blocks[block_index].text;
                    let text = chosen
                        .into_iter()
                        .filter_map(|index| ranges.get(index))
                        .map(|range| source[range.clone()].to_string())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    state.copy_mode = None;
                    state.mark_transcript_dirty();
                    return Some(text);
                }
                _ => None,
            };
            if let Some(next_index) = next_index {
                state.copy_mode = Some(CopyMode::Markdown {
                    block_index,
                    markdown_index: next_index,
                    selected,
                });
                state.mark_transcript_dirty();
                reveal_markdown_block(state, block_index, next_index);
            }
        }
    }
    None
}

fn assistant_block_indices(state: &AppState) -> Vec<usize> {
    state
        .blocks
        .iter()
        .enumerate()
        .filter_map(|(index, block)| {
            (block.kind == BlockKind::Assistant && !block.text.trim().is_empty()).then_some(index)
        })
        .collect()
}

fn scroll_to_transcript_block(state: &mut AppState, block_index: usize) {
    let Some(offset) = state
        .transcript_block_offsets
        .get(block_index)
        .copied()
        .flatten()
    else {
        state.scroll = usize::MAX;
        state.at_bottom = true;
        state.new_output = false;
        return;
    };
    state.scroll = offset;
    state.at_bottom = false;
    state.new_output = false;
}

fn reveal_markdown_block(state: &mut AppState, block_index: usize, markdown_index: usize) {
    let Some((start, end)) = state
        .transcript_markdown_ranges
        .get(block_index)
        .and_then(|ranges| ranges.get(markdown_index))
        .copied()
    else {
        return;
    };
    let height = state.transcript_viewport_height.max(1);
    if start < state.scroll {
        state.scroll = start;
    } else if end > state.scroll.saturating_add(height) {
        state.scroll = end.saturating_sub(height);
    }
    state.at_bottom = false;
    state.new_output = false;
}

fn prepare_control_c(state: &mut AppState) -> ControlCAction {
    if !state.composer.text.is_empty() || !state.image_attachments.is_empty() {
        state.composer.clear();
        state.image_attachments.clear();
        ControlCAction::ClearedComposer
    } else if state.queued_turns.pop_back().is_some() {
        ControlCAction::RemovedQueuedTurn
    } else if turn_in_progress(state) {
        ControlCAction::Interrupt
    } else {
        state.quit = true;
        ControlCAction::Quit
    }
}

fn control_scroll_direction(key: KeyEvent) -> Option<ScrollDirection> {
    if !key.modifiers.contains(KeyModifiers::CONTROL) {
        return None;
    }
    match normalize_control_shortcut(key).code {
        KeyCode::Char('k') => Some(ScrollDirection::Up),
        KeyCode::Char('j') => Some(ScrollDirection::Down),
        _ => None,
    }
}

fn handle_composer_arrow(composer: &mut Composer, key: KeyEvent, width: usize) -> bool {
    if key.modifiers.contains(KeyModifiers::ALT) {
        return false;
    }
    let selecting = key.modifiers.contains(KeyModifiers::SHIFT);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    match (key.code, control) {
        (KeyCode::Left, false) => composer.move_left(selecting),
        (KeyCode::Right, false) => composer.move_right(selecting),
        (KeyCode::Up, false) => composer.move_up(width, selecting),
        (KeyCode::Down, false) => composer.move_down(width, selecting),
        (KeyCode::Left, true) => composer.move_word_left(selecting),
        (KeyCode::Right, true) => composer.move_word_right(selecting),
        (KeyCode::Up, true) => composer.move_to_start(selecting),
        (KeyCode::Down, true) => composer.move_to_end(selecting),
        _ => return false,
    }
    true
}

fn scroll_expanded_approval(
    state: &mut AppState,
    direction: ScrollDirection,
    amount: usize,
) -> bool {
    let Some(Popup::Approval(approval)) = state.popup.as_mut() else {
        return false;
    };
    if !approval.expanded || approval.entering_feedback {
        return false;
    }
    approval.scroll = match direction {
        ScrollDirection::Up => approval.scroll.saturating_sub(amount),
        ScrollDirection::Down => approval
            .scroll
            .saturating_add(amount)
            .min(approval.max_scroll),
    };
    true
}

pub(crate) fn normalize_control_shortcut(mut key: KeyEvent) -> KeyEvent {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        key.code = match key.code {
            KeyCode::Char(ch) => KeyCode::Char(latin_shortcut_char(ch)),
            code => code,
        };
    }
    key
}

fn normalize_list_navigation(mut key: KeyEvent) -> KeyEvent {
    key.code = match key.code {
        KeyCode::Char('о' | 'О') => KeyCode::Char('j'),
        KeyCode::Char('л' | 'Л') => KeyCode::Char('k'),
        code => code,
    };
    key
}

fn jk_list_selection(code: KeyCode, selected: usize, len: usize) -> Option<usize> {
    match code {
        KeyCode::Char('k') => Some(selected.saturating_sub(1)),
        KeyCode::Char('j') => Some((selected + 1).min(len.saturating_sub(1))),
        _ => None,
    }
}

fn normalize_approval_key(key: KeyEvent, editing_feedback: bool) -> KeyEvent {
    if editing_feedback {
        key
    } else {
        normalize_list_navigation(key)
    }
}

fn normalize_copy_navigation(mut key: KeyEvent) -> KeyEvent {
    if key.modifiers.contains(KeyModifiers::CONTROL) {
        return normalize_control_shortcut(key);
    }
    key.code = match key.code {
        KeyCode::Char('о' | 'О') => KeyCode::Char('j'),
        KeyCode::Char('л' | 'Л') => KeyCode::Char('k'),
        KeyCode::Char('п') => KeyCode::Char('g'),
        KeyCode::Char('П') => KeyCode::Char('G'),
        KeyCode::Char('н') => KeyCode::Char('y'),
        KeyCode::Char('Н') => KeyCode::Char('Y'),
        code => code,
    };
    key
}

fn is_newline_key(key: KeyEvent) -> bool {
    key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::SHIFT)
}

fn latin_shortcut_char(ch: char) -> char {
    match ch {
        'с' | 'С' => 'c',
        'м' | 'М' => 'v',
        'з' | 'З' => 'p',
        'т' | 'Т' => 'n',
        'л' | 'Л' => 'k',
        'о' | 'О' => 'j',
        'п' | 'П' => 'g',
        'щ' | 'Щ' => 'o',
        'н' | 'Н' => 'y',
        _ => ch,
    }
}

#[derive(Clone, Copy)]
enum ApprovalChoice {
    Allow,
    Session,
    Deny,
    DenyWithFeedback,
    Cancel,
}

fn approval_result(approval: &Approval, choice: ApprovalChoice) -> Value {
    match approval.kind {
        ApprovalKind::Command | ApprovalKind::File => json!({
            "decision": match choice {
                ApprovalChoice::Allow => "accept",
                ApprovalChoice::Session => {
                    if matches!(approval.kind, ApprovalKind::Command) {
                        if let Some(amendment) = approval_execpolicy_amendment(approval) {
                            return json!({
                                "decision": {
                                    "acceptWithExecpolicyAmendment": {
                                        "execpolicy_amendment": amendment
                                    }
                                }
                            });
                        }
                    }
                    "acceptForSession"
                }
                ApprovalChoice::Deny | ApprovalChoice::DenyWithFeedback => "decline",
                ApprovalChoice::Cancel => "cancel",
            }
        }),
        ApprovalKind::Permissions => match choice {
            ApprovalChoice::Allow | ApprovalChoice::Session => json!({
                "permissions": approval.params.get("permissions").cloned().unwrap_or(json!({})),
                "scope": if matches!(choice, ApprovalChoice::Session) { "session" } else { "turn" }
            }),
            ApprovalChoice::Deny | ApprovalChoice::DenyWithFeedback | ApprovalChoice::Cancel => {
                json!({"permissions": {}, "scope": "turn"})
            }
        },
        ApprovalKind::Legacy => json!({
            "decision": match choice {
                ApprovalChoice::Allow => Value::String("approved".into()),
                ApprovalChoice::Session => Value::String("approved_for_session".into()),
                ApprovalChoice::Deny => json!({"denied": {"rejection": "Denied by user"}}),
                ApprovalChoice::DenyWithFeedback => json!({
                    "denied": {"rejection": approval.feedback.text.trim()}
                }),
                ApprovalChoice::Cancel => Value::String("abort".into()),
            }
        }),
        ApprovalKind::Unsupported => Value::Null,
    }
}

fn approval_execpolicy_amendment(approval: &Approval) -> Option<Vec<String>> {
    let amendment = approval
        .params
        .get("proposedExecpolicyAmendment")?
        .as_array()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!amendment.is_empty()).then_some(amendment)
}

fn approval_feedback(approval: &Approval, choice: ApprovalChoice) -> String {
    let command = approval_command_summary(&approval.detail);
    match (&approval.kind, choice) {
        (ApprovalKind::Command, ApprovalChoice::Allow)
        | (ApprovalKind::Legacy, ApprovalChoice::Allow)
            if approval.title == "Run command?" =>
        {
            format!("You approved Magdex to run {command} this time")
        }
        (ApprovalKind::Command, ApprovalChoice::Session)
        | (ApprovalKind::Legacy, ApprovalChoice::Session)
            if approval.title == "Run command?" =>
        {
            if let Some(amendment) = approval_execpolicy_amendment(approval) {
                format!(
                    "You approved Magdex to run {command} and remember commands starting with {}",
                    amendment.join(" ")
                )
            } else {
                format!("You approved Magdex to run {command} for this session")
            }
        }
        (ApprovalKind::Command, ApprovalChoice::Deny)
        | (ApprovalKind::Legacy, ApprovalChoice::Deny)
            if approval.title == "Run command?" =>
        {
            format!("You denied Magdex permission to run {command}")
        }
        (ApprovalKind::Command, ApprovalChoice::DenyWithFeedback)
        | (ApprovalKind::Legacy, ApprovalChoice::DenyWithFeedback)
            if approval.title == "Run command?" =>
        {
            let direction = approval_command_summary(approval.feedback.text.trim());
            format!("You denied Magdex permission to run {command} and asked it to {direction}")
        }
        (ApprovalKind::Command, ApprovalChoice::Cancel)
        | (ApprovalKind::Legacy, ApprovalChoice::Cancel)
            if approval.title == "Run command?" =>
        {
            format!("You denied Magdex permission to run {command} and cancelled the turn")
        }
        (_, ApprovalChoice::Allow) => "You approved Magdex this time".to_string(),
        (_, ApprovalChoice::Session) => "You approved Magdex for this session".to_string(),
        (_, ApprovalChoice::Deny) => "You denied Magdex permission".to_string(),
        (_, ApprovalChoice::DenyWithFeedback) => {
            let direction = approval_command_summary(approval.feedback.text.trim());
            format!("You denied Magdex permission and asked it to {direction}")
        }
        (_, ApprovalChoice::Cancel) => {
            "You denied Magdex permission and cancelled the turn".to_string()
        }
    }
}

fn approval_command_summary(command: &str) -> String {
    const MAX_CHARS: usize = 96;
    let command = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if command.chars().count() <= MAX_CHARS {
        return command;
    }
    let mut summary = command.chars().take(MAX_CHARS - 1).collect::<String>();
    summary.push('…');
    summary
}

fn collaboration_modes_from_response(result: &Value) -> Vec<CollaborationModeInfo> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let id = item.get("mode")?.as_str()?.to_string();
            Some(CollaborationModeInfo {
                name: item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_string(),
                model: item.get("model").and_then(Value::as_str).map(str::to_owned),
                effort: item
                    .get("reasoning_effort")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                id,
            })
        })
        .collect()
}

fn collaboration_mode_payload(state: &AppState) -> Option<Value> {
    let mode = state
        .collaboration_modes
        .iter()
        .find(|mode| mode.id == state.collaboration_mode)?;
    let model = state.model.clone().or_else(|| mode.model.clone())?;
    Some(json!({
        "mode": mode.id,
        "settings": {
            "model": model,
            "reasoning_effort": state.effort.clone().or_else(|| mode.effort.clone()),
            "developer_instructions": null
        }
    }))
}

fn apply_collaboration_mode(state: &mut AppState, mode: &CollaborationModeInfo) {
    state.collaboration_mode = mode.id.clone();
    state.explicit_collaboration_mode = true;
}

fn alternate_collaboration_mode_index(state: &AppState) -> Option<usize> {
    let target = if state.collaboration_mode == "plan" {
        "default"
    } else {
        "plan"
    };
    state
        .collaboration_modes
        .iter()
        .position(|mode| mode.id == target)
}

fn parse_user_input_request(id: Value, params: &Value) -> Option<UserInputRequest> {
    let questions = params
        .get("questions")?
        .as_array()?
        .iter()
        .map(|question| {
            let options = question
                .get("options")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|option| {
                    Some(UserInputOption {
                        label: option.get("label")?.as_str()?.to_string(),
                        description: option.get("description")?.as_str()?.to_string(),
                    })
                })
                .collect::<Option<Vec<_>>>()?;
            Some(UserInputQuestion {
                id: question.get("id")?.as_str()?.to_string(),
                header: question.get("header")?.as_str()?.to_string(),
                question: question.get("question")?.as_str()?.to_string(),
                options,
                allow_other: question
                    .get("isOther")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                secret: question
                    .get("isSecret")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Option<Vec<_>>>()?;
    if questions.is_empty() {
        return None;
    }
    Some(UserInputRequest {
        id,
        questions,
        current: 0,
        answers: Vec::new(),
        selected: 0,
        input: Default::default(),
        entering_other: false,
    })
}

fn user_input_response(request: &UserInputRequest) -> Value {
    let answers = request
        .answers
        .iter()
        .map(|(id, answers)| (id.clone(), json!({"answers": answers})))
        .collect::<serde_json::Map<_, _>>();
    json!({"answers": answers})
}

fn user_input_answer_summary(request: &UserInputRequest) -> String {
    let show_labels = request.answers.len() > 1;
    let answers = request
        .answers
        .iter()
        .map(|(id, answers)| {
            let question = request.questions.iter().find(|question| &question.id == id);
            let secret = question.is_some_and(|question| question.secret);
            let answer = if secret {
                "[hidden]".to_string()
            } else {
                answers
                    .iter()
                    .map(|answer| {
                        answer
                            .strip_prefix("user_note: ")
                            .unwrap_or(answer)
                            .split_whitespace()
                            .collect::<Vec<_>>()
                            .join(" ")
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if show_labels {
                let label = question
                    .map(|question| question.header.trim())
                    .filter(|label| !label.is_empty())
                    .unwrap_or(id);
                format!("{label} — {answer}")
            } else {
                answer
            }
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("Answered: {answers}")
}

fn append_delta(state: &mut AppState, params: &Value, kind: BlockKind, title: &str) {
    if let (Some(id), Some(delta)) = (
        params.get("itemId").and_then(Value::as_str),
        params.get("delta").and_then(Value::as_str),
    ) {
        if kind == BlockKind::Command {
            let delta = sanitize_terminal_output(delta);
            state.append_delta(id, kind, title, &delta);
            if let Some(block) = state
                .blocks
                .iter_mut()
                .find(|candidate| candidate.id.as_deref() == Some(id))
            {
                truncate_command_output(&mut block.text);
            }
        } else {
            state.append_delta(id, kind, title, delta);
        }
    }
}

fn block_from_item(item: &Value, completed: bool, show_reasoning: bool) -> Option<TranscriptBlock> {
    let kind = item.get("type")?.as_str()?;
    let id = item.get("id").and_then(Value::as_str).map(str::to_owned);
    let mut block = match kind {
        "userMessage" => TranscriptBlock::new(
            BlockKind::User,
            "You",
            user_message_display(item.get("content")),
        ),
        "agentMessage" => TranscriptBlock::new(
            if item.get("phase").and_then(Value::as_str) == Some("commentary") {
                BlockKind::Commentary
            } else {
                BlockKind::Assistant
            },
            "Codex",
            item.get("text").and_then(Value::as_str).unwrap_or(""),
        ),
        "reasoning" if show_reasoning => TranscriptBlock::new(
            BlockKind::Reasoning,
            "Thinking",
            item.get("summary")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        "commandExecution" => {
            let command = display_command(item);
            let mut output = sanitize_terminal_output(
                item.get("aggregatedOutput")
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            );
            truncate_command_output(&mut output);
            let mut block = TranscriptBlock::new(BlockKind::Command, command, output);
            block.action_status = Some(action_status(item, completed));
            block.exit_code = item.get("exitCode").and_then(Value::as_i64);
            block.command_actions = command_actions_from_item(item);
            block
        }
        "fileChange" => {
            let mut block = TranscriptBlock::new(BlockKind::File, "Files", "");
            block.action_status = Some(action_status(item, completed));
            block.file_changes = file_changes_from_item(item);
            block
        }
        "webSearch" => {
            let (title, detail) = web_action_display(item);
            TranscriptBlock::new(BlockKind::Web, title, detail)
        }
        "error" => TranscriptBlock::new(
            BlockKind::Error,
            "Error",
            item.get("message")
                .and_then(Value::as_str)
                .unwrap_or("Unknown error"),
        ),
        _ => return None,
    };
    block.id = id;
    Some(block)
}

fn action_status(item: &Value, completed: bool) -> ActionStatus {
    match item.get("status").and_then(Value::as_str) {
        Some("inProgress") => ActionStatus::InProgress,
        Some("completed") => ActionStatus::Completed,
        Some("failed") => ActionStatus::Failed,
        Some("declined") => ActionStatus::Declined,
        _ if completed => ActionStatus::Completed,
        _ => ActionStatus::InProgress,
    }
}

fn command_actions_from_item(item: &Value) -> Vec<CommandAction> {
    item.get("commandActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| {
            let action_type = action.get("type").and_then(Value::as_str);
            let command = action
                .get("command")
                .and_then(Value::as_str)
                .map(normalize_shell_command)
                .unwrap_or_default();
            let (kind, label) = match action_type {
                Some("read") => (
                    CommandActionKind::Read,
                    first_string(action, &["name", "path"]).unwrap_or(command),
                ),
                Some("listFiles") => (CommandActionKind::ListFiles, command),
                Some("search") => {
                    let query = first_string(action, &["query"]);
                    let path = first_string(action, &["path"]);
                    let label = match (query, path) {
                        (Some(query), Some(path)) => format!("{query} in {path}"),
                        (Some(query), None) => query,
                        _ => command,
                    };
                    (CommandActionKind::Search, label)
                }
                _ => (CommandActionKind::Unknown, command),
            };
            (!label.is_empty()).then_some(CommandAction { kind, label })
        })
        .collect()
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str))
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn file_changes_from_item(item: &Value) -> Vec<FileChange> {
    item.get("changes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|change| {
            let path = change.get("path")?.as_str()?.to_string();
            let kind_value = change.get("kind");
            let kind_name = kind_value
                .and_then(|kind| kind.get("type"))
                .and_then(Value::as_str)
                .or_else(|| kind_value.and_then(Value::as_str));
            let kind = match kind_name {
                Some("add") => FileChangeKind::Add,
                Some("delete") => FileChangeKind::Delete,
                Some("update") => FileChangeKind::Update,
                _ => FileChangeKind::Unknown,
            };
            let move_path = kind_value
                .and_then(|kind| kind.get("move_path"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let diff = change
                .get("diff")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            Some(FileChange {
                kind,
                path,
                move_path,
                diff,
            })
        })
        .collect()
}

fn update_file_change_patch(state: &mut AppState, params: &Value) {
    let Some(id) = params.get("itemId").and_then(Value::as_str) else {
        return;
    };
    let mut block = TranscriptBlock::new(BlockKind::File, "Files", "");
    block.id = Some(id.to_string());
    block.action_status = Some(ActionStatus::InProgress);
    block.file_changes = file_changes_from_item(params);
    state.upsert(id, block);
}

fn web_action_display(item: &Value) -> (&'static str, String) {
    let action = item.get("action");
    match action
        .and_then(|action| action.get("type"))
        .and_then(Value::as_str)
    {
        Some("openPage") => (
            "Opened",
            action
                .and_then(|action| action.get("url"))
                .and_then(Value::as_str)
                .unwrap_or("a web page")
                .to_string(),
        ),
        Some("findInPage") => {
            let pattern = action
                .and_then(|action| action.get("pattern"))
                .and_then(Value::as_str)
                .unwrap_or("text");
            let url = action
                .and_then(|action| action.get("url"))
                .and_then(Value::as_str);
            let detail = url
                .map(|url| format!("{pattern} in {url}"))
                .unwrap_or_else(|| pattern.to_string());
            ("Found on page", detail)
        }
        _ => (
            "Searched the web for",
            item.get("query")
                .and_then(Value::as_str)
                .unwrap_or("search")
                .to_string(),
        ),
    }
}

fn sanitize_terminal_output(input: &str) -> String {
    let mut output = String::with_capacity(input.len().min(MAX_COMMAND_OUTPUT_BYTES));
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            match chars.next() {
                Some('[') => {
                    for candidate in chars.by_ref() {
                        if ('@'..='~').contains(&candidate) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(candidate) = chars.next() {
                        if candidate == '\u{7}' {
                            break;
                        }
                        if candidate == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                Some(_) | None => {}
            }
            continue;
        }
        if ch == '\r' {
            if chars.peek() != Some(&'\n') {
                output.push('\n');
            }
        } else if ch == '\n' || ch == '\t' || !ch.is_control() {
            output.push(ch);
        }
        if output.len() > MAX_COMMAND_OUTPUT_BYTES {
            truncate_command_output(&mut output);
            break;
        }
    }
    output
}

fn truncate_command_output(output: &mut String) {
    if output.len() <= MAX_COMMAND_OUTPUT_BYTES {
        return;
    }
    let mut end = MAX_COMMAND_OUTPUT_BYTES;
    while !output.is_char_boundary(end) {
        end -= 1;
    }
    output.truncate(end);
    output.push_str(COMMAND_OUTPUT_TRUNCATED);
}

fn turn_input(text: &str, images: &[ImageAttachment]) -> Vec<Value> {
    let mut input = Vec::with_capacity(images.len() + usize::from(!text.is_empty()));
    if !text.is_empty() {
        input.push(json!({"type": "text", "text": text}));
    }
    input.extend(
        images
            .iter()
            .map(|image| json!({"type": "image", "url": image.data_url})),
    );
    input
}

fn turn_steer_params(thread_id: &str, turn_id: &str, text: &str) -> Value {
    json!({
        "threadId": thread_id,
        "expectedTurnId": turn_id,
        "input": turn_input(text, &[]),
    })
}

fn user_input_display(text: &str, images: &[ImageAttachment]) -> String {
    let mut parts = Vec::with_capacity(2);
    if !text.is_empty() {
        parts.push(text.to_string());
    }
    if !images.is_empty() {
        parts.push(image_labels(images.len()));
    }
    parts.join("\n")
}

fn user_message_display(content: Option<&Value>) -> String {
    let mut parts = Vec::new();
    let mut image_count = 0;
    for part in content.and_then(Value::as_array).into_iter().flatten() {
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            Some("localImage" | "image") => image_count += 1,
            _ => {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
        }
    }
    if image_count > 0 {
        parts.push(image_labels(image_count));
    }
    parts.join("\n")
}

fn user_message_text(item: &Value) -> Option<String> {
    if item.get("type").and_then(Value::as_str) != Some("userMessage") {
        return None;
    }
    let text = item
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n");
    (!text.is_empty()).then_some(text)
}

fn image_labels(count: usize) -> String {
    (1..=count)
        .map(|index| format!("[Image {index}]"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn trust_target_from_config(result: &Value, cwd: &str) -> Option<String> {
    let config = result.get("config").unwrap_or(result);
    let projects = config.get("projects").and_then(Value::as_object);
    let project_layers = result
        .get("layers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|layer| layer.pointer("/name/type").and_then(Value::as_str) == Some("project"))
        .collect::<Vec<_>>();

    if let Some(layer) = project_layers.iter().rev().find(|layer| {
        layer
            .get("disabledReason")
            .and_then(Value::as_str)
            .is_some()
    }) {
        let reason = layer
            .get("disabledReason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let trust_target = trust_target_from_disabled_layer(layer, reason)
            .unwrap_or_else(|| project_root_from_config(config, cwd));
        if project_has_trust_decision(projects, &trust_target)
            || reason.contains("marked as untrusted")
        {
            return None;
        }
        if reason.contains("trusted project") {
            return Some(trust_target);
        }
    }

    if project_layers
        .iter()
        .any(|layer| layer.get("disabledReason").is_none())
    {
        return None;
    }

    let trust_target = project_root_from_config(config, cwd);
    if project_has_trust_decision(projects, &trust_target)
        || projects.into_iter().flatten().any(|(path, project)| {
            project.get("trust_level").and_then(Value::as_str) == Some("untrusted")
                && Path::new(cwd).starts_with(path)
        })
    {
        None
    } else {
        Some(trust_target)
    }
}

fn trust_target_from_disabled_layer(layer: &Value, reason: &str) -> Option<String> {
    reason
        .split_once(", add ")
        .and_then(|(_, suffix)| suffix.rsplit_once(" as a trusted project in "))
        .map(|(path, _)| path.to_string())
        .or_else(|| {
            layer
                .pointer("/name/dotCodexFolder")
                .and_then(Value::as_str)
                .and_then(|path| {
                    path.strip_suffix("/.codex")
                        .or_else(|| path.strip_suffix("\\.codex"))
                })
                .map(str::to_owned)
        })
}

fn project_has_trust_decision(
    projects: Option<&serde_json::Map<String, Value>>,
    trust_target: &str,
) -> bool {
    projects
        .and_then(|projects| projects.get(trust_target))
        .and_then(|project| project.get("trust_level"))
        .and_then(Value::as_str)
        .is_some_and(|level| matches!(level, "trusted" | "untrusted"))
}

fn project_root_from_config(config: &Value, cwd: &str) -> String {
    let markers = config
        .get("project_root_markers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    for ancestor in Path::new(cwd).ancestors() {
        if markers.iter().any(|marker| ancestor.join(marker).exists()) {
            return ancestor.to_string_lossy().into_owned();
        }
    }
    cwd.to_string()
}

fn trust_write_params(trust_target: &str) -> Value {
    let escaped = trust_target.replace('\\', "\\\\").replace('"', "\\\"");
    json!({
        "edits": [{
            "keyPath": format!("projects.\"{escaped}\".trust_level"),
            "value": "trusted",
            "mergeStrategy": "replace"
        }],
        "reloadUserConfig": true
    })
}

fn thread_list_params(cwd: &str, scope: ResumeScope) -> Value {
    let mut params = json!({
        "limit": 50,
        "sortKey": "updated_at",
        "sortDirection": "desc"
    });
    if scope == ResumeScope::CurrentDirectory {
        params["cwd"] = Value::String(cwd.to_string());
    }
    params
}

fn thread_resume_params(thread_id: &str, cwd: &str) -> Value {
    json!({
        "threadId": thread_id,
        "cwd": cwd,
        "excludeTurns": true
    })
}

fn thread_summaries_from_response(result: &Value) -> Vec<ThreadSummary> {
    result
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|thread| {
            Some(ThreadSummary {
                id: thread.get("id")?.as_str()?.to_string(),
                title: thread
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|name| !name.is_empty())
                    .or_else(|| thread.get("preview").and_then(Value::as_str))
                    .unwrap_or("Untitled conversation")
                    .lines()
                    .next()
                    .unwrap_or("Untitled conversation")
                    .to_string(),
                cwd: thread
                    .get("cwd")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                updated_at: thread
                    .get("updatedAt")
                    .or_else(|| thread.get("createdAt"))
                    .and_then(Value::as_i64)
                    .unwrap_or(0),
            })
        })
        .collect()
}

fn encode_clipboard_image(width: usize, height: usize, rgba: &[u8]) -> Result<ImageAttachment> {
    let expected_len = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4));
    ensure!(
        expected_len == Some(rgba.len()),
        "clipboard returned invalid RGBA image data"
    );
    let width_u32 = u32::try_from(width)?;
    let height_u32 = u32::try_from(height)?;
    let mut png_bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut png_bytes, width_u32, height_u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(rgba)?;
    }
    Ok(ImageAttachment {
        data_url: format!("data:image/png;base64,{}", BASE64.encode(png_bytes)),
        width,
        height,
    })
}

fn turn_duration_ms(turn: &Value) -> Option<u64> {
    turn.get("durationMs").and_then(Value::as_u64).or_else(|| {
        let started = turn.get("startedAt")?.as_i64()?;
        let completed = turn.get("completedAt")?.as_i64()?;
        completed
            .checked_sub(started)
            .and_then(|seconds| u64::try_from(seconds).ok())
            .and_then(|seconds| seconds.checked_mul(1_000))
    })
}

fn turn_end_block(duration_ms: u64) -> TranscriptBlock {
    TranscriptBlock::new(BlockKind::TurnEnd, "", format_duration(duration_ms))
}

fn context_usage(params: &Value) -> Option<ContextUsage> {
    Some(ContextUsage {
        input_tokens: params.pointer("/tokenUsage/last/inputTokens")?.as_u64()?,
        context_window: params
            .pointer("/tokenUsage/modelContextWindow")
            .and_then(Value::as_u64),
    })
}

fn quota_usage_from_response(result: &Value) -> QuotaUsage {
    let snapshot = result
        .pointer("/rateLimitsByLimitId/codex")
        .or_else(|| result.get("rateLimits"));
    let mut usage = QuotaUsage::default();
    for window in snapshot
        .into_iter()
        .flat_map(|snapshot| [snapshot.get("primary"), snapshot.get("secondary")])
        .flatten()
    {
        let Some(window) = quota_window(window) else {
            continue;
        };
        match window.window_minutes {
            300 => usage.five_hour = Some(window),
            10_080 => usage.weekly = Some(window),
            _ => {}
        }
    }
    usage
}

fn quota_window(value: &Value) -> Option<QuotaWindow> {
    Some(QuotaWindow {
        used_percent: value.get("usedPercent")?.as_u64()?.min(100),
        window_minutes: value.get("windowDurationMins")?.as_u64()?,
        resets_at: value.get("resetsAt").and_then(Value::as_u64),
    })
}

fn merge_quota_warning_state(previous: QuotaUsage, next: QuotaUsage) -> QuotaUsage {
    QuotaUsage {
        five_hour: merge_quota_warning_window(previous.five_hour, next.five_hour),
        weekly: merge_quota_warning_window(previous.weekly, next.weekly),
    }
}

fn merge_quota_warning_window(
    previous: Option<QuotaWindow>,
    next: Option<QuotaWindow>,
) -> Option<QuotaWindow> {
    match (previous, next) {
        (Some(previous), Some(mut next)) => {
            next.resets_at = next.resets_at.or(previous.resets_at);
            let previous_level = quota_warning_level(previous.remaining_percent());
            let next_level = quota_warning_level(next.remaining_percent());
            if previous_level > next_level && next.remaining_percent() < QUOTA_WARNING_REARM_PERCENT
            {
                next.used_percent = previous.used_percent;
            }
            Some(next)
        }
        (previous, None) => previous,
        (_, next) => next,
    }
}

fn quota_warning_messages(previous: QuotaUsage, next: QuotaUsage, now: u64) -> Vec<String> {
    [
        ("5-hour", previous.five_hour, next.five_hour),
        ("weekly", previous.weekly, next.weekly),
    ]
    .into_iter()
    .filter_map(|(label, previous, next)| {
        let next = next?;
        let level = quota_warning_level(next.remaining_percent());
        let previous_level = previous
            .map(|previous| quota_warning_level(previous.remaining_percent()))
            .unwrap_or(0);
        (level > previous_level).then(|| quota_warning_message(label, next, level, now))
    })
    .collect()
}

fn quota_warning_level(remaining_percent: u64) -> u8 {
    match remaining_percent {
        0 => 3,
        1..=10 => 2,
        11..=20 => 1,
        _ => 0,
    }
}

fn quota_warning_message(label: &str, window: QuotaWindow, level: u8, now: u64) -> String {
    let severity = match level {
        3 => "exhausted",
        2 => "critical",
        _ => "low",
    };
    let reset = window
        .resets_at
        .map(|resets_at| format!(" · {}", format_quota_reset(resets_at, now)))
        .unwrap_or_default();
    format!(
        "Quota {severity}: {label} limit has {}% left{reset}",
        window.remaining_percent()
    )
}

fn format_quota_reset(resets_at: u64, now: u64) -> String {
    let remaining = resets_at.saturating_sub(now);
    if remaining == 0 {
        return "resets soon".into();
    }
    let minutes = remaining.div_ceil(60);
    if minutes < 60 {
        format!("resets in {minutes}m")
    } else if minutes < 24 * 60 {
        format!("resets in {}h {}m", minutes / 60, minutes % 60)
    } else {
        format!(
            "resets in {}d {}h",
            minutes / (24 * 60),
            minutes % (24 * 60) / 60
        )
    }
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn context_compaction_block(
    state: &mut AppState,
    item: &Value,
    turn_id: Option<&str>,
) -> Option<TranscriptBlock> {
    (item.get("type").and_then(Value::as_str) == Some("contextCompaction")).then_some(())?;
    let id = item.get("id").and_then(Value::as_str)?;
    let dedupe_id = turn_id
        .map(|turn_id| format!("turn:{turn_id}"))
        .unwrap_or_else(|| format!("item:{id}"));
    context_compaction_block_with_id(state, &dedupe_id, id)
}

fn legacy_context_compaction_block(
    state: &mut AppState,
    params: &Value,
) -> Option<TranscriptBlock> {
    let turn_id = params.get("turnId").and_then(Value::as_str)?;
    let dedupe_id = format!("turn:{turn_id}");
    let block_id = format!("context-compaction:{turn_id}");
    context_compaction_block_with_id(state, &dedupe_id, &block_id)
}

fn context_compaction_block_with_id(
    state: &mut AppState,
    dedupe_id: &str,
    block_id: &str,
) -> Option<TranscriptBlock> {
    state.record_context_compaction(dedupe_id).then_some(())?;

    let mut block = TranscriptBlock::new(
        BlockKind::Compaction,
        "Context compacted",
        format!(
            "Earlier conversation was summarized to free context · {} total",
            state.context_compactions
        ),
    );
    block.id = Some(block_id.to_string());
    Some(block)
}

pub(crate) fn format_duration(duration_ms: u64) -> String {
    let total_seconds = duration_ms / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = total_seconds % 3_600 / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn display_command(item: &Value) -> String {
    let parsed = item
        .get("commandActions")
        .and_then(Value::as_array)
        .filter(|actions| actions.len() == 1)
        .and_then(|actions| actions[0].get("command"))
        .and_then(Value::as_str);
    let raw = parsed
        .or_else(|| item.get("command").and_then(Value::as_str))
        .unwrap_or("command");
    normalize_shell_command(raw)
}

fn approval_command_display(params: &Value) -> String {
    let actions = params
        .get("commandActions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|action| action.get("command").and_then(Value::as_str))
        .map(normalize_shell_command)
        .collect::<Vec<_>>();
    if !actions.is_empty() {
        return actions.join("\n");
    }
    params
        .get("command")
        .and_then(Value::as_str)
        .map(normalize_shell_command)
        .or_else(|| {
            params
                .get("reason")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Command execution".into())
}

fn normalize_shell_command(command: &str) -> String {
    const PREFIXES: [&str; 6] = [
        "/usr/bin/zsh -lc ",
        "/bin/zsh -lc ",
        "zsh -lc ",
        "/usr/bin/bash -lc ",
        "/bin/bash -lc ",
        "bash -lc ",
    ];
    let (command, wrapped) = PREFIXES
        .iter()
        .find_map(|prefix| command.strip_prefix(prefix).map(|command| (command, true)))
        .unwrap_or((command, false));
    let command = command.trim();
    if !wrapped {
        return command.to_string();
    }
    if command.len() >= 2 {
        let first = command.as_bytes()[0];
        let last = command.as_bytes()[command.len() - 1];
        if first == b'\'' && last == b'\'' {
            return command[1..command.len() - 1].to_string();
        }
        if first == b'"' && last == b'"' {
            return unescape_double_quoted_shell(&command[1..command.len() - 1]);
        }
    }
    command.to_string()
}

fn unescape_double_quoted_shell(command: &str) -> String {
    let mut output = String::with_capacity(command.len());
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            output.push(ch);
            continue;
        }
        let Some(next) = chars.next() else {
            output.push('\\');
            break;
        };
        if matches!(next, '"' | '\\' | '$' | '`' | '\n') {
            output.push(next);
        } else {
            output.push('\\');
            output.push(next);
        }
    }
    output
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(not(target_os = "macos"))]
    let command = "xdg-open";
    let _ = std::process::Command::new(command)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn account_is_available(result: &Value) -> bool {
    result
        .get("account")
        .is_some_and(|account| !account.is_null())
        || !result
            .get("requiresOpenaiAuth")
            .and_then(Value::as_bool)
            .unwrap_or(true)
}

pub fn relative_time(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(timestamp);
    match now.saturating_sub(timestamp) {
        0..=59 => "now".into(),
        60..=3599 => format!("{} min", (now - timestamp) / 60),
        3600..=86_399 => format!("{} h", (now - timestamp) / 3600),
        86_400..=172_799 => "yesterday".into(),
        seconds => format!("{} d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_items_are_ignored() {
        assert!(block_from_item(&json!({"id":"1", "type":"futureThing"}), true, true).is_none());
    }

    #[test]
    fn account_gate_requires_login_only_when_codex_requires_it() {
        assert!(account_is_available(
            &json!({"account": {"type": "chatgpt"}})
        ));
        assert!(account_is_available(
            &json!({"account": null, "requiresOpenaiAuth": false})
        ));
        assert!(!account_is_available(
            &json!({"account": null, "requiresOpenaiAuth": true})
        ));
    }

    #[test]
    fn undecided_project_trust_is_detected_from_disabled_config_layer() {
        let mut response = json!({
            "config": {
                "projects": {},
                "project_root_markers": [".git"]
            },
            "layers": [{
                "name": {
                    "type": "project",
                    "dotCodexFolder": "/work/project/.codex"
                },
                "disabledReason": "To load project-local config, hooks, and exec policies, add /work/project as a trusted project in /home/user/.codex/config.toml."
            }]
        });

        assert_eq!(
            trust_target_from_config(&response, "/work/project/src"),
            Some("/work/project".into())
        );

        response["config"]["projects"]["/work/project"] = json!({"trust_level": "trusted"});
        assert_eq!(
            trust_target_from_config(&response, "/work/project/src"),
            None
        );
    }

    #[test]
    fn project_trust_write_uses_the_official_config_batch_shape() {
        assert_eq!(
            trust_write_params("/work/project\"quoted\""),
            json!({
                "edits": [{
                    "keyPath": r#"projects."/work/project\"quoted\"".trust_level"#,
                    "value": "trusted",
                    "mergeStrategy": "replace"
                }],
                "reloadUserConfig": true
            })
        );
    }

    #[test]
    fn clipboard_images_use_the_app_server_input_shape() {
        let images = vec![
            ImageAttachment {
                data_url: "data:image/png;base64,one".into(),
                width: 10,
                height: 20,
            },
            ImageAttachment {
                data_url: "data:image/png;base64,two".into(),
                width: 30,
                height: 40,
            },
        ];
        assert_eq!(
            turn_input("describe these", &images),
            vec![
                json!({"type": "text", "text": "describe these"}),
                json!({"type": "image", "url": "data:image/png;base64,one"}),
                json!({"type": "image", "url": "data:image/png;base64,two"}),
            ]
        );
        assert_eq!(
            turn_input("", &images),
            vec![
                json!({"type": "image", "url": "data:image/png;base64,one"}),
                json!({"type": "image", "url": "data:image/png;base64,two"}),
            ]
        );
    }

    #[test]
    fn clipboard_rgba_is_encoded_as_a_png_data_url() {
        let image = encode_clipboard_image(1, 1, &[255, 0, 0, 255]).unwrap();
        assert_eq!((image.width, image.height), (1, 1));
        assert!(image.data_url.starts_with("data:image/png;base64,iVBOR"));
    }

    #[test]
    fn user_image_parts_are_visible_in_history() {
        let item = json!({
            "id": "user-1",
            "type": "userMessage",
            "content": [
                {"type": "text", "text": "what is this?"},
                {"type": "image", "url": "data:image/png;base64,one"},
                {"type": "image", "url": "data:image/png;base64,two"}
            ]
        });
        let block = block_from_item(&item, true, true).unwrap();
        assert_eq!(block.text, "what is this?\n[Image 1] [Image 2]");
        assert_eq!(user_message_text(&item).as_deref(), Some("what is this?"));

        let optimistic = user_input_display(
            "what is this?",
            &[
                ImageAttachment {
                    data_url: "data:image/png;base64,one".into(),
                    width: 640,
                    height: 480,
                },
                ImageAttachment {
                    data_url: "data:image/png;base64,two".into(),
                    width: 320,
                    height: 240,
                },
            ],
        );
        assert_eq!(optimistic, block.text);
    }

    #[test]
    fn thread_list_is_scoped_to_the_current_directory() {
        assert_eq!(
            thread_list_params("/work/magdex", ResumeScope::CurrentDirectory),
            json!({
                "cwd": "/work/magdex",
                "limit": 50,
                "sortKey": "updated_at",
                "sortDirection": "desc"
            })
        );
    }

    #[test]
    fn thread_list_can_include_all_directories() {
        assert_eq!(
            thread_list_params("/work/magdex", ResumeScope::AllDirectories),
            json!({
                "limit": 50,
                "sortKey": "updated_at",
                "sortDirection": "desc"
            })
        );
    }

    #[test]
    fn resumed_threads_use_the_current_working_directory() {
        assert_eq!(
            thread_resume_params("thread-1", "/work/current"),
            json!({
                "threadId": "thread-1",
                "cwd": "/work/current",
                "excludeTurns": true
            })
        );
    }

    #[test]
    fn invalid_clipboard_rgba_is_rejected() {
        assert!(encode_clipboard_image(2, 2, &[0; 4]).is_err());
    }

    #[test]
    fn parses_agent_item_without_rejecting_extra_fields() {
        let item = json!({"id":"a", "type":"agentMessage", "text":"hi", "future":42});
        let block = block_from_item(&item, true, true).unwrap();
        assert_eq!(block.text, "hi");
    }

    #[test]
    fn separates_commentary_from_the_final_answer() {
        let commentary = block_from_item(
            &json!({"id":"a", "type":"agentMessage", "text":"working", "phase":"commentary"}),
            true,
            true,
        )
        .unwrap();
        let final_answer = block_from_item(
            &json!({"id":"b", "type":"agentMessage", "text":"done", "phase":"final_answer"}),
            true,
            true,
        )
        .unwrap();
        assert_eq!(commentary.kind, BlockKind::Commentary);
        assert_eq!(final_answer.kind, BlockKind::Assistant);
    }

    #[test]
    fn modern_approval_decisions_match_protocol() {
        let approval = Approval {
            id: json!(7),
            kind: ApprovalKind::Command,
            title: String::new(),
            detail: String::new(),
            reason: None,
            params: json!({}),
            selected: 0,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: false,
        };
        assert_eq!(
            approval_result(&approval, ApprovalChoice::Session),
            json!({"decision": "acceptForSession"})
        );
        assert_eq!(
            approval_result(&approval, ApprovalChoice::Deny),
            json!({"decision": "decline"})
        );
    }

    #[test]
    fn command_approval_uses_the_proposed_rule_and_describes_the_result() {
        let mut approval = Approval {
            id: json!(7),
            kind: ApprovalKind::Command,
            title: "Run command?".into(),
            detail: "cargo test --locked".into(),
            reason: Some("Verify the approval flow".into()),
            params: json!({"proposedExecpolicyAmendment": ["cargo", "test"]}),
            selected: 0,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: false,
        };

        assert_eq!(
            approval_result(&approval, ApprovalChoice::Session),
            json!({
                "decision": {
                    "acceptWithExecpolicyAmendment": {
                        "execpolicy_amendment": ["cargo", "test"]
                    }
                }
            })
        );
        assert_eq!(
            approval_feedback(&approval, ApprovalChoice::Allow),
            "You approved Magdex to run cargo test --locked this time"
        );
        assert_eq!(
            approval_feedback(&approval, ApprovalChoice::Session),
            "You approved Magdex to run cargo test --locked and remember commands starting with cargo test"
        );
        approval
            .feedback
            .insert_str("run the focused approval test instead");
        assert_eq!(
            approval_result(&approval, ApprovalChoice::DenyWithFeedback),
            json!({"decision": "decline"})
        );
        assert_eq!(
            approval_feedback(&approval, ApprovalChoice::DenyWithFeedback),
            "You denied Magdex permission to run cargo test --locked and asked it to run the focused approval test instead"
        );
        assert_eq!(
            turn_steer_params("thread-1", "turn-1", &approval.feedback.text),
            json!({
                "threadId": "thread-1",
                "expectedTurnId": "turn-1",
                "input": [{"type": "text", "text": "run the focused approval test instead"}]
            })
        );
    }

    #[test]
    fn permission_approval_echoes_only_server_request() {
        let permissions = json!({"network": {"enabled": true}});
        let approval = Approval {
            id: json!(8),
            kind: ApprovalKind::Permissions,
            title: String::new(),
            detail: String::new(),
            reason: None,
            params: json!({"permissions": permissions}),
            selected: 0,
            expanded: false,
            scroll: 0,
            max_scroll: 0,
            feedback: Default::default(),
            entering_feedback: false,
        };
        assert_eq!(
            approval_result(&approval, ApprovalChoice::Allow),
            json!({"permissions": permissions, "scope": "turn"})
        );
    }

    #[test]
    fn parses_structured_user_input_and_builds_protocol_response() {
        let params = json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "item-1",
            "isBlocking": true,
            "questions": [{
                "id": "database",
                "header": "Database",
                "question": "Which database?",
                "isOther": true,
                "isSecret": false,
                "options": [
                    {"label": "SQLite", "description": "Local and simple"},
                    {"label": "Postgres", "description": "Server database"}
                ],
                "futureField": 42
            }]
        });
        let mut request = parse_user_input_request(json!(9), &params).unwrap();
        let question = request.current_question().unwrap();
        assert_eq!(question.id, "database");
        assert_eq!(question.options[1].label, "Postgres");
        assert!(question.allow_other);

        request
            .answers
            .push(("database".into(), vec!["Postgres".into()]));
        assert_eq!(
            user_input_response(&request),
            json!({"answers": {"database": {"answers": ["Postgres"]}}})
        );
        assert_eq!(user_input_answer_summary(&request), "Answered: Postgres");
    }

    #[test]
    fn user_input_summary_shows_notes_and_hides_secrets() {
        let request = UserInputRequest {
            id: json!(9),
            questions: vec![
                UserInputQuestion {
                    id: "target".into(),
                    header: "Target".into(),
                    question: "Where should this run?".into(),
                    options: vec![],
                    allow_other: true,
                    secret: false,
                },
                UserInputQuestion {
                    id: "token".into(),
                    header: "Token".into(),
                    question: "Enter the token".into(),
                    options: vec![],
                    allow_other: true,
                    secret: true,
                },
            ],
            current: 2,
            answers: vec![
                ("target".into(), vec!["user_note: staging\nserver".into()]),
                ("token".into(), vec!["do-not-render".into()]),
            ],
            selected: 0,
            input: Default::default(),
            entering_other: false,
        };

        assert_eq!(
            user_input_answer_summary(&request),
            "Answered: Target — staging server; Token — [hidden]"
        );
    }

    #[test]
    fn rejects_user_input_without_questions() {
        assert!(parse_user_input_request(json!(10), &json!({"questions": []})).is_none());
    }

    #[test]
    fn collaboration_modes_use_the_server_presets_and_protocol_shape() {
        let modes = collaboration_modes_from_response(&json!({
            "data": [
                {"name": "Plan", "mode": "plan", "model": null, "reasoning_effort": "medium"},
                {"name": "Default", "mode": "default", "model": null, "reasoning_effort": null}
            ]
        }));
        assert_eq!(modes.len(), 2);
        assert_eq!(modes[0].id, "plan");
        assert_eq!(modes[0].effort.as_deref(), Some("medium"));

        let mut state = AppState::new("/project".into(), true);
        state.collaboration_modes = modes;
        state.collaboration_mode = "plan".into();
        state.model = Some("gpt-test".into());
        assert_eq!(
            collaboration_mode_payload(&state),
            Some(json!({
                "mode": "plan",
                "settings": {
                    "model": "gpt-test",
                    "reasoning_effort": "medium",
                    "developer_instructions": null
                }
            }))
        );
    }

    #[test]
    fn tab_mode_toggle_alternates_between_default_and_plan() {
        let mut state = AppState::new("/project".into(), true);

        assert_eq!(alternate_collaboration_mode_index(&state), Some(0));
        state.collaboration_mode = "plan".into();
        assert_eq!(alternate_collaboration_mode_index(&state), Some(1));
    }

    #[test]
    fn changing_mode_preserves_reasoning_effort() {
        let mut state = AppState::new("/project".into(), true);
        state.model = Some("gpt-test".into());
        state.effort = Some("high".into());
        state.explicit_effort = true;
        let plan = state.collaboration_modes[0].clone();

        apply_collaboration_mode(&mut state, &plan);

        assert_eq!(state.collaboration_mode, "plan");
        assert_eq!(state.effort.as_deref(), Some("high"));
        assert!(state.explicit_effort);
        assert_eq!(
            collaboration_mode_payload(&state),
            Some(json!({
                "mode": "plan",
                "settings": {
                    "model": "gpt-test",
                    "reasoning_effort": "high",
                    "developer_instructions": null
                }
            }))
        );
    }

    #[test]
    fn only_shift_enter_inserts_a_newline() {
        assert!(is_newline_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::SHIFT
        )));
        assert!(!is_newline_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::ALT
        )));
        assert!(!is_newline_key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE
        )));
    }

    #[test]
    fn composer_arrows_apply_control_and_shift_movements() {
        let mut composer = Composer::default();
        composer.insert_str("one two\nthree");

        assert!(handle_composer_arrow(
            &mut composer,
            KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL),
            80,
        ));
        assert_eq!(composer.cursor, "one two\n".len());

        assert!(handle_composer_arrow(
            &mut composer,
            KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL | KeyModifiers::SHIFT,),
            80,
        ));
        assert_eq!(composer.cursor, 0);
        assert_eq!(composer.selection_range(), Some(0.."one two\n".len()));

        assert!(handle_composer_arrow(
            &mut composer,
            KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL),
            80,
        ));
        assert_eq!(composer.cursor, composer.text.len());
        assert_eq!(composer.selection_range(), None);
    }

    #[test]
    fn command_display_prefers_parsed_action_and_strips_shell() {
        let item = json!({
            "command": "/usr/bin/zsh -lc \"rg --files\"",
            "commandActions": [{"command": "rg --files"}]
        });
        assert_eq!(display_command(&item), "rg --files");
        assert_eq!(
            normalize_shell_command("/usr/bin/zsh -lc \"cargo test\""),
            "cargo test"
        );
        assert_eq!(
            normalize_shell_command(
                r#"/usr/bin/zsh -lc "printf '%s\n' '{\"id\":1}' | codex app-server""#
            ),
            r#"printf '%s\n' '{"id":1}' | codex app-server"#
        );
    }

    #[test]
    fn command_items_preserve_semantic_actions_and_status() {
        let item = json!({
            "id": "command-1",
            "type": "commandExecution",
            "command": "/usr/bin/zsh -lc \"rg -n needle src\"",
            "commandActions": [{
                "type": "search",
                "command": "rg -n needle src",
                "query": "needle",
                "path": "src"
            }],
            "aggregatedOutput": "src/app.rs:1:needle",
            "status": "completed",
            "exitCode": 0
        });

        let block = block_from_item(&item, true, true).unwrap();
        assert_eq!(block.action_status, Some(ActionStatus::Completed));
        assert_eq!(block.exit_code, Some(0));
        assert_eq!(
            block.command_actions,
            vec![CommandAction {
                kind: CommandActionKind::Search,
                label: "needle in src".into()
            }]
        );
        assert_eq!(block.text, "src/app.rs:1:needle");
    }

    #[test]
    fn file_change_items_preserve_kind_move_and_diff() {
        let item = json!({
            "id": "file-1",
            "type": "fileChange",
            "status": "completed",
            "changes": [{
                "path": "/project/old.rs",
                "kind": {"type": "update", "move_path": "/project/new.rs"},
                "diff": "@@ -1 +1 @@\n-old\n+new"
            }]
        });

        let block = block_from_item(&item, true, true).unwrap();
        assert_eq!(block.action_status, Some(ActionStatus::Completed));
        assert_eq!(
            block.file_changes,
            vec![FileChange {
                kind: FileChangeKind::Update,
                path: "/project/old.rs".into(),
                move_path: Some("/project/new.rs".into()),
                diff: "@@ -1 +1 @@\n-old\n+new".into()
            }]
        );
    }

    #[test]
    fn patch_updates_refresh_the_live_file_diff() {
        let mut state = AppState::new("/project".into(), true);
        state.clear_blocks();
        update_file_change_patch(
            &mut state,
            &json!({
                "itemId": "file-1",
                "changes": [{
                    "path": "/project/src/ui.rs",
                    "kind": {"type": "update", "move_path": null},
                    "diff": "@@ -1 +1 @@\n-old\n+new"
                }]
            }),
        );

        assert_eq!(state.blocks.len(), 1);
        assert_eq!(state.blocks[0].id.as_deref(), Some("file-1"));
        assert_eq!(
            state.blocks[0].action_status,
            Some(ActionStatus::InProgress)
        );
        assert_eq!(state.blocks[0].file_changes[0].path, "/project/src/ui.rs");
    }

    #[test]
    fn web_items_use_the_specific_action_label() {
        let opened = block_from_item(
            &json!({
                "id": "web-1",
                "type": "webSearch",
                "query": "fallback",
                "action": {"type": "openPage", "url": "https://example.com"}
            }),
            true,
            true,
        )
        .unwrap();
        assert_eq!(opened.title, "Opened");
        assert_eq!(opened.text, "https://example.com");
    }

    #[test]
    fn terminal_output_is_sanitized_and_bounded() {
        assert_eq!(
            sanitize_terminal_output("\u{1b}[?1049hhello\u{1b}[0m\rworld\u{1b}]0;title\u{7}\u{0}"),
            "hello\nworld"
        );

        let mut output = "x".repeat(MAX_COMMAND_OUTPUT_BYTES + 10);
        truncate_command_output(&mut output);
        assert!(output.ends_with(COMMAND_OUTPUT_TRUNCATED));
        assert!(output.len() <= MAX_COMMAND_OUTPUT_BYTES + COMMAND_OUTPUT_TRUNCATED.len());
    }

    #[test]
    fn approval_prefers_friendly_command_actions() {
        let params = json!({
            "command": "/usr/bin/zsh -lc \"a very long wrapped command\"",
            "commandActions": [
                {"type": "unknown", "command": "printf request data"},
                {"type": "unknown", "command": "codex app-server --enable test"}
            ]
        });
        assert_eq!(
            approval_command_display(&params),
            "printf request data\ncodex app-server --enable test"
        );
    }

    #[test]
    fn formats_live_and_historical_turn_durations() {
        assert_eq!(format_duration(595_999), "9m 55s");
        assert_eq!(format_duration(3_661_000), "1h 1m 1s");
        assert_eq!(
            turn_duration_ms(&json!({"startedAt": 100, "completedAt": 173})),
            Some(73_000)
        );
        assert_eq!(
            turn_duration_ms(&json!({"durationMs": 48_982})),
            Some(48_982)
        );
    }

    #[test]
    fn parses_active_context_usage() {
        let params = json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "tokenUsage": {
                "last": {
                    "inputTokens": 24_763,
                    "cachedInputTokens": 24_448,
                    "cacheWriteInputTokens": 12,
                    "outputTokens": 122,
                    "reasoningOutputTokens": 64,
                    "totalTokens": 24_885
                },
                "total": {
                    "inputTokens": 100_000,
                    "cachedInputTokens": 80_000,
                    "outputTokens": 4_000,
                    "reasoningOutputTokens": 2_000,
                    "totalTokens": 104_000
                },
                "modelContextWindow": 400_000
            }
        });

        let usage = context_usage(&params).unwrap();
        assert_eq!(usage.input_tokens, 24_763);
        assert_eq!(usage.context_window, Some(400_000));
    }

    #[test]
    fn parses_five_hour_and_weekly_quota_windows() {
        let result = json!({
            "rateLimits": {
                "limitId": "codex",
                "primary": {
                    "usedPercent": 19,
                    "windowDurationMins": 300,
                    "resetsAt": 1_789_767_849_u64
                },
                "secondary": {
                    "usedPercent": 74,
                    "windowDurationMins": 10_080,
                    "resetsAt": 1_790_090_321_u64
                }
            },
            "rateLimitsByLimitId": {
                "codex": {
                    "primary": {
                        "usedPercent": 19,
                        "windowDurationMins": 300,
                        "resetsAt": 1_789_767_849_u64
                    },
                    "secondary": {
                        "usedPercent": 74,
                        "windowDurationMins": 10_080,
                        "resetsAt": 1_790_090_321_u64
                    }
                }
            }
        });

        let usage = quota_usage_from_response(&result);

        assert_eq!(usage.five_hour.unwrap().remaining_percent(), 81);
        assert_eq!(usage.weekly.unwrap().remaining_percent(), 26);
    }

    #[test]
    fn quota_warnings_fire_only_when_crossing_low_thresholds() {
        let window = |used_percent| QuotaWindow {
            used_percent,
            window_minutes: 300,
            resets_at: Some(10_000),
        };
        let usage = |used_percent| QuotaUsage {
            five_hour: Some(window(used_percent)),
            weekly: None,
        };

        let low = quota_warning_messages(usage(79), usage(80), 1_000);
        let repeated = quota_warning_messages(usage(80), usage(81), 1_000);
        let critical = quota_warning_messages(usage(89), usage(90), 1_000);
        let exhausted = quota_warning_messages(usage(99), usage(100), 1_000);

        assert_eq!(low.len(), 1);
        assert!(low[0].contains("Quota low: 5-hour limit has 20% left"));
        assert!(repeated.is_empty());
        assert!(critical[0].contains("Quota critical"));
        assert!(exhausted[0].contains("Quota exhausted"));
    }

    #[test]
    fn quota_warnings_survive_sparse_snapshots_without_repeating() {
        let previous = QuotaUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 80,
                window_minutes: 300,
                resets_at: Some(10_000),
            }),
            weekly: None,
        };
        let missing_reset = QuotaUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 81,
                window_minutes: 300,
                resets_at: None,
            }),
            weekly: None,
        };

        let merged = merge_quota_warning_state(previous, missing_reset);
        assert_eq!(merged.five_hour.unwrap().resets_at, Some(10_000));
        assert!(quota_warning_messages(previous, merged, 1_000).is_empty());
        assert_eq!(
            merge_quota_warning_state(previous, QuotaUsage::default()),
            previous
        );

        let moved_reset = QuotaUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 82,
                window_minutes: 300,
                resets_at: Some(20_000),
            }),
            weekly: None,
        };
        assert!(quota_warning_messages(previous, moved_reset, 1_000).is_empty());

        let threshold_jitter = QuotaUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 79,
                window_minutes: 300,
                resets_at: Some(20_000),
            }),
            weekly: None,
        };
        let still_warned = merge_quota_warning_state(previous, threshold_jitter);
        assert_eq!(still_warned.five_hour.unwrap().remaining_percent(), 20);
        let low_again = QuotaUsage {
            five_hour: Some(QuotaWindow {
                used_percent: 81,
                window_minutes: 300,
                resets_at: Some(20_000),
            }),
            weekly: None,
        };
        assert!(quota_warning_messages(still_warned, low_again, 1_000).is_empty());

        let recovered = merge_quota_warning_state(
            still_warned,
            QuotaUsage {
                five_hour: Some(QuotaWindow {
                    used_percent: 50,
                    window_minutes: 300,
                    resets_at: Some(30_000),
                }),
                weekly: None,
            },
        );
        assert_eq!(recovered.five_hour.unwrap().remaining_percent(), 50);
        assert_eq!(quota_warning_messages(recovered, low_again, 1_000).len(), 1);
    }

    #[test]
    fn quota_reset_time_is_human_readable() {
        assert_eq!(format_quota_reset(1_060, 1_000), "resets in 1m");
        assert_eq!(format_quota_reset(8_260, 1_000), "resets in 2h 1m");
        assert_eq!(format_quota_reset(181_000, 1_000), "resets in 2d 2h");
        assert_eq!(format_quota_reset(999, 1_000), "resets soon");
    }

    #[test]
    fn service_events_wait_until_the_active_turn_finishes() {
        let mut state = AppState::new("/tmp/project".into(), true);
        let mut deferred = Vec::new();
        let immediate = defer_turn_event_if_active(
            &state,
            &mut deferred,
            DeferredTurnEvent::QuotaWarning("low".into()),
        );
        assert!(matches!(
            immediate,
            Some(DeferredTurnEvent::QuotaWarning(_))
        ));
        assert!(deferred.is_empty());

        state.turn_id = Some("turn-1".into());
        let immediate = defer_turn_event_if_active(
            &state,
            &mut deferred,
            DeferredTurnEvent::Transcript(TranscriptBlock::new(
                BlockKind::Compaction,
                "Context compacted",
                "summary",
            )),
        );
        assert!(immediate.is_none());
        assert_eq!(deferred.len(), 1);
        assert!(matches!(deferred[0], DeferredTurnEvent::Transcript(_)));
    }

    #[test]
    fn creates_one_block_for_each_context_compaction_turn() {
        let mut state = AppState::new("/tmp/project".into(), true);
        let item = json!({"id": "compact-1", "type": "contextCompaction"});

        let block = context_compaction_block(&mut state, &item, Some("turn-1")).unwrap();
        let duplicate = legacy_context_compaction_block(
            &mut state,
            &json!({"threadId": "thread-1", "turnId": "turn-1"}),
        );
        let unrelated = context_compaction_block(
            &mut state,
            &json!({"id": "message-1", "type": "agentMessage"}),
            Some("turn-1"),
        );

        assert_eq!(state.context_compactions, 1);
        assert_eq!(block.kind, BlockKind::Compaction);
        assert_eq!(block.id.as_deref(), Some("compact-1"));
        assert!(block.text.contains("1 total"));
        assert!(duplicate.is_none());
        assert!(unrelated.is_none());

        let mut legacy_first = AppState::new("/tmp/project".into(), true);
        assert!(legacy_context_compaction_block(
            &mut legacy_first,
            &json!({"threadId": "thread-1", "turnId": "turn-1"}),
        )
        .is_some());
        assert!(context_compaction_block(&mut legacy_first, &item, Some("turn-1")).is_none());
        assert_eq!(legacy_first.context_compactions, 1);
    }

    #[test]
    fn control_shortcuts_follow_the_same_physical_keys_on_russian_layout() {
        for (russian, latin) in [
            ('с', 'c'),
            ('м', 'v'),
            ('з', 'p'),
            ('т', 'n'),
            ('л', 'k'),
            ('о', 'j'),
            ('п', 'g'),
            ('щ', 'o'),
            ('н', 'y'),
        ] {
            let key = KeyEvent::new(KeyCode::Char(russian), KeyModifiers::CONTROL);
            assert_eq!(normalize_control_shortcut(key).code, KeyCode::Char(latin));
        }

        let text = KeyEvent::new(KeyCode::Char('с'), KeyModifiers::NONE);
        assert_eq!(normalize_control_shortcut(text).code, KeyCode::Char('с'));
    }

    #[test]
    fn copy_mode_opens_the_latest_assistant_and_moves_between_answers() {
        let mut state = copy_mode_state();

        enter_copy_mode(&mut state);
        assert_eq!(state.copy_mode, Some(CopyMode::Answers { block_index: 3 }));
        assert_eq!(state.scroll, 30);

        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('л'), KeyModifiers::NONE),
        );
        assert_eq!(state.copy_mode, Some(CopyMode::Answers { block_index: 1 }));
        assert_eq!(state.scroll, 10);

        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('П'), KeyModifiers::SHIFT),
        );
        assert_eq!(state.copy_mode, Some(CopyMode::Answers { block_index: 3 }));
    }

    #[test]
    fn copy_mode_marks_multiple_markdown_blocks_and_copies_in_source_order() {
        let mut state = copy_mode_state();
        enter_copy_mode(&mut state);

        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );
        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('о'), KeyModifiers::NONE),
        );
        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('о'), KeyModifiers::NONE),
        );
        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        assert_eq!(
            state.copy_mode,
            Some(CopyMode::Markdown {
                block_index: 3,
                markdown_index: 2,
                selected: vec![0, 2],
            })
        );
        let copied = handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Char('н'), KeyModifiers::NONE),
        );
        assert_eq!(copied.as_deref(), Some("## Newest\n\nlet value = 1;"));
        assert!(state.copy_mode.is_none());
    }

    #[test]
    fn copy_mode_escape_returns_to_answers_before_closing() {
        let mut state = copy_mode_state();
        enter_copy_mode(&mut state);
        handle_copy_mode_key(
            &mut state,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        );

        handle_copy_mode_key(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(state.copy_mode, Some(CopyMode::Answers { block_index: 3 }));
        handle_copy_mode_key(&mut state, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(state.copy_mode.is_none());
    }

    fn copy_mode_state() -> AppState {
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![
            TranscriptBlock::new(BlockKind::User, "You", "first"),
            TranscriptBlock::new(BlockKind::Assistant, "Codex", "# Older\n\nText"),
            TranscriptBlock::new(BlockKind::User, "You", "second"),
            TranscriptBlock::new(
                BlockKind::Assistant,
                "Codex",
                "## Newest\n\nParagraph\n\n```rust\nlet value = 1;\n```",
            ),
        ];
        state.transcript_block_offsets = vec![Some(0), Some(10), Some(20), Some(30)];
        state.transcript_markdown_ranges = vec![
            vec![],
            vec![(10, 11), (12, 13)],
            vec![],
            vec![(30, 31), (32, 33), (34, 35)],
        ];
        state.transcript_viewport_height = 8;
        state
    }

    #[test]
    fn control_c_clears_composer_before_interrupting_or_quitting() {
        let mut state = AppState::new("/project".into(), true);
        state.turn_id = Some("turn-1".into());
        state.composer.replace("draft".into());

        assert_eq!(
            prepare_control_c(&mut state),
            ControlCAction::ClearedComposer
        );
        assert!(state.composer.text.is_empty());
        state.queued_turns.push_back(QueuedTurn {
            text: "next request".into(),
            images: vec![],
        });
        assert_eq!(
            prepare_control_c(&mut state),
            ControlCAction::RemovedQueuedTurn
        );
        assert!(state.queued_turns.is_empty());
        assert_eq!(prepare_control_c(&mut state), ControlCAction::Interrupt);

        state.turn_id = None;
        assert_eq!(prepare_control_c(&mut state), ControlCAction::Quit);
        assert!(state.quit);
    }

    #[test]
    fn queued_turn_waits_for_the_active_turn_and_keeps_fifo_order() {
        let mut state = AppState::new("/project".into(), true);
        state.thread_id = Some("thread-1".into());
        state.turn_id = Some("turn-1".into());
        for text in ["second", "third"] {
            state.queued_turns.push_back(QueuedTurn {
                text: text.into(),
                images: vec![],
            });
        }

        assert!(take_next_queued_turn(&mut state).is_none());
        assert_eq!(state.queued_turns.len(), 2);

        state.turn_id = None;
        let (thread_id, queued) = take_next_queued_turn(&mut state).unwrap();
        assert_eq!(thread_id, "thread-1");
        assert_eq!(queued.text, "second");
        assert_eq!(state.queued_turns.front().unwrap().text, "third");
    }

    #[test]
    fn control_scroll_is_distinct_from_plain_menu_navigation() {
        for (character, direction) in [
            ('k', ScrollDirection::Up),
            ('л', ScrollDirection::Up),
            ('j', ScrollDirection::Down),
            ('о', ScrollDirection::Down),
        ] {
            let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL);
            assert_eq!(control_scroll_direction(key), Some(direction));
        }

        let plain = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        assert_eq!(control_scroll_direction(plain), None);
    }

    #[test]
    fn list_navigation_supports_russian_layout_without_changing_other_text() {
        let down = KeyEvent::new(KeyCode::Char('о'), KeyModifiers::NONE);
        let up = KeyEvent::new(KeyCode::Char('л'), KeyModifiers::NONE);
        let text = KeyEvent::new(KeyCode::Char('я'), KeyModifiers::NONE);

        assert_eq!(normalize_list_navigation(down).code, KeyCode::Char('j'));
        assert_eq!(normalize_list_navigation(up).code, KeyCode::Char('k'));
        assert_eq!(normalize_list_navigation(text).code, KeyCode::Char('я'));
    }

    #[test]
    fn jk_list_selection_ignores_arrow_keys() {
        assert_eq!(jk_list_selection(KeyCode::Char('k'), 1, 3), Some(0));
        assert_eq!(jk_list_selection(KeyCode::Char('j'), 1, 3), Some(2));
        assert_eq!(jk_list_selection(KeyCode::Up, 1, 3), None);
        assert_eq!(jk_list_selection(KeyCode::Down, 1, 3), None);
    }

    #[test]
    fn approval_feedback_keeps_russian_o_and_l_as_text() {
        for character in ['о', 'л'] {
            let key = KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE);
            assert_eq!(
                normalize_approval_key(key, true).code,
                KeyCode::Char(character)
            );
        }
        let navigation = KeyEvent::new(KeyCode::Char('о'), KeyModifiers::NONE);
        assert_eq!(
            normalize_approval_key(navigation, false).code,
            KeyCode::Char('j')
        );
    }

    #[test]
    fn expanded_approval_scroll_stays_within_its_body() {
        let mut state = AppState::new("/project".into(), true);
        state.popup = Some(Popup::Approval(Approval {
            id: json!(1),
            kind: ApprovalKind::Command,
            title: "Run command?".into(),
            detail: "long command".into(),
            reason: None,
            params: json!({}),
            selected: 0,
            expanded: true,
            scroll: 0,
            max_scroll: 7,
            feedback: Default::default(),
            entering_feedback: false,
        }));

        assert!(scroll_expanded_approval(
            &mut state,
            ScrollDirection::Down,
            3
        ));
        let Some(Popup::Approval(approval)) = state.popup.as_ref() else {
            panic!("approval popup missing");
        };
        assert_eq!(approval.scroll, 3);
        scroll_expanded_approval(&mut state, ScrollDirection::Down, 99);
        let Some(Popup::Approval(approval)) = state.popup.as_ref() else {
            panic!("approval popup missing");
        };
        assert_eq!(approval.scroll, 7);
        scroll_expanded_approval(&mut state, ScrollDirection::Up, 3);
        let Some(Popup::Approval(approval)) = state.popup.as_ref() else {
            panic!("approval popup missing");
        };
        assert_eq!(approval.scroll, 4);
    }

    #[test]
    fn expanding_latest_command_follows_its_output_to_the_bottom() {
        let mut state = AppState::new("/project".into(), true);
        state.scroll = 12;
        state.at_bottom = false;
        state.new_output = true;
        state.push(TranscriptBlock::new(
            BlockKind::Command,
            "python3 -c long-command",
            "long output",
        ));

        toggle_latest_command(&mut state);

        assert!(state.blocks.last().unwrap().expanded);
        assert_eq!(state.scroll, usize::MAX);
        assert!(state.at_bottom);
        assert!(!state.new_output);
    }
}
