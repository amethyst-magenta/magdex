use std::collections::VecDeque;

use ratatui::text::Line;
use serde_json::Value;

#[derive(Clone, Debug, Default)]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub efforts: Vec<String>,
    pub default_effort: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CollaborationModeInfo {
    pub id: String,
    pub name: String,
    pub model: Option<String>,
    pub effort: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    User,
    Assistant,
    Commentary,
    Reasoning,
    Command,
    File,
    Web,
    Error,
    Status,
    TurnEnd,
}

#[derive(Clone, Debug)]
pub struct TranscriptBlock {
    pub id: Option<String>,
    pub kind: BlockKind,
    pub title: String,
    pub text: String,
    pub expanded: bool,
}

impl TranscriptBlock {
    pub fn new(kind: BlockKind, title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            title: title.into(),
            text: text.into(),
            expanded: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ThreadSummary {
    pub id: String,
    pub title: String,
    pub cwd: String,
    pub updated_at: i64,
}

#[derive(Clone, Debug)]
pub struct ResumePicker {
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Popup {
    Palette {
        selected: usize,
    },
    Models {
        selected: usize,
    },
    CollaborationModes {
        selected: usize,
    },
    Reasoning {
        selected: usize,
    },
    Resume {
        selected: usize,
        loading: bool,
    },
    Login {
        url: Option<String>,
        error: Option<String>,
    },
    Approval(Approval),
    UserInput(UserInputRequest),
    Disconnected {
        reason: String,
        selected: usize,
    },
}

#[derive(Clone, Debug)]
pub enum ApprovalKind {
    Command,
    File,
    Permissions,
    Legacy,
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct Approval {
    pub id: Value,
    pub kind: ApprovalKind,
    pub title: String,
    pub detail: String,
    pub params: Value,
    pub selected: usize,
}

#[derive(Clone, Debug)]
pub struct UserInputOption {
    pub label: String,
    pub description: String,
}

#[derive(Clone, Debug)]
pub struct UserInputQuestion {
    pub id: String,
    pub header: String,
    pub question: String,
    pub options: Vec<UserInputOption>,
    pub allow_other: bool,
    pub secret: bool,
}

#[derive(Clone, Debug)]
pub struct UserInputRequest {
    pub id: Value,
    pub questions: Vec<UserInputQuestion>,
    pub current: usize,
    pub answers: Vec<(String, Vec<String>)>,
    pub selected: usize,
    pub input: Composer,
    pub entering_other: bool,
}

impl UserInputRequest {
    pub fn current_question(&self) -> Option<&UserInputQuestion> {
        self.questions.get(self.current)
    }

    pub fn is_editing(&self) -> bool {
        self.entering_other
            || self
                .current_question()
                .is_some_and(|question| question.options.is_empty())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Composer {
    pub text: String,
    pub cursor: usize,
}

#[derive(Clone, Debug)]
pub struct ImageAttachment {
    pub data_url: String,
    pub width: usize,
    pub height: usize,
}

impl Composer {
    pub fn insert(&mut self, ch: char) {
        self.text.insert(self.cursor, ch);
        self.cursor += ch.len_utf8();
    }

    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
    }

    pub fn newline(&mut self) {
        self.insert('\n');
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let previous = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.text.replace_range(previous..self.cursor, "");
        self.cursor = previous;
    }

    pub fn delete(&mut self) {
        if self.cursor >= self.text.len() {
            return;
        }
        let next = self.text[self.cursor..]
            .char_indices()
            .nth(1)
            .map(|(offset, _)| self.cursor + offset)
            .unwrap_or(self.text.len());
        self.text.replace_range(self.cursor..next, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
    }

    pub fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(offset, _)| self.cursor + offset)
                .unwrap_or(self.text.len());
        }
    }

    pub fn clear(&mut self) -> String {
        self.cursor = 0;
        std::mem::take(&mut self.text)
    }
}

pub struct AppState {
    pub cwd: String,
    pub project: String,
    pub thread_id: Option<String>,
    pub turn_id: Option<String>,
    pub turn_started_at: Option<std::time::Instant>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub default_mode_effort: Option<String>,
    pub collaboration_mode: String,
    pub explicit_model: bool,
    pub explicit_effort: bool,
    pub explicit_collaboration_mode: bool,
    pub models: Vec<ModelInfo>,
    pub collaboration_modes: Vec<CollaborationModeInfo>,
    pub threads: Vec<ThreadSummary>,
    pub resume_picker: Option<ResumePicker>,
    pub blocks: Vec<TranscriptBlock>,
    pub transcript_revision: u64,
    pub transcript_cache_width: u16,
    pub transcript_cache_revision: u64,
    pub transcript_cache_lines: Vec<Line<'static>>,
    pub composer: Composer,
    pub message_history: Vec<String>,
    pub message_history_position: Option<usize>,
    pub message_history_draft: String,
    pub image_attachments: Vec<ImageAttachment>,
    pub popup: Option<Popup>,
    pub pending_server_requests: VecDeque<ServerPrompt>,
    pub scroll: usize,
    pub transcript_max_scroll: usize,
    pub at_bottom: bool,
    pub new_output: bool,
    pub context_percent: Option<u8>,
    pub show_reasoning: bool,
    pub quit: bool,
}

#[derive(Clone, Debug)]
pub enum ServerPrompt {
    Approval(Approval),
    UserInput(UserInputRequest),
}

impl ServerPrompt {
    fn id(&self) -> &Value {
        match self {
            Self::Approval(approval) => &approval.id,
            Self::UserInput(request) => &request.id,
        }
    }
}

impl AppState {
    pub fn new(cwd: String, show_reasoning: bool) -> Self {
        let project = std::path::Path::new(&cwd)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&cwd)
            .to_string();
        Self {
            cwd,
            project,
            thread_id: None,
            turn_id: None,
            turn_started_at: None,
            model: None,
            effort: None,
            default_mode_effort: None,
            collaboration_mode: "default".into(),
            explicit_model: false,
            explicit_effort: false,
            explicit_collaboration_mode: false,
            models: vec![],
            collaboration_modes: vec![
                CollaborationModeInfo {
                    id: "plan".into(),
                    name: "Plan".into(),
                    model: None,
                    effort: Some("medium".into()),
                },
                CollaborationModeInfo {
                    id: "default".into(),
                    name: "Default".into(),
                    model: None,
                    effort: None,
                },
            ],
            threads: vec![],
            resume_picker: None,
            blocks: vec![TranscriptBlock::new(
                BlockKind::Status,
                "Codex",
                "Connecting to app-server…",
            )],
            transcript_revision: 1,
            transcript_cache_width: 0,
            transcript_cache_revision: 0,
            transcript_cache_lines: vec![],
            composer: Composer::default(),
            message_history: vec![],
            message_history_position: None,
            message_history_draft: String::new(),
            image_attachments: vec![],
            popup: None,
            pending_server_requests: VecDeque::new(),
            scroll: 0,
            transcript_max_scroll: 0,
            at_bottom: true,
            new_output: false,
            context_percent: None,
            show_reasoning,
            quit: false,
        }
    }

    pub fn remember_message(&mut self, text: impl Into<String>) {
        let text = text.into();
        if !text.is_empty() {
            self.message_history.push(text);
        }
        self.message_history_position = None;
        self.message_history_draft.clear();
    }

    pub fn clear_message_history(&mut self) {
        self.message_history.clear();
        self.message_history_position = None;
        self.message_history_draft.clear();
    }

    pub fn previous_message(&mut self) {
        if self.message_history.is_empty() {
            return;
        }
        let position = match self.message_history_position {
            Some(position) => position.saturating_sub(1),
            None => {
                self.message_history_draft = self.composer.text.clone();
                self.message_history.len() - 1
            }
        };
        self.message_history_position = Some(position);
        self.set_composer(self.message_history[position].clone());
    }

    pub fn next_message(&mut self) {
        let Some(position) = self.message_history_position else {
            return;
        };
        if position + 1 < self.message_history.len() {
            let next = position + 1;
            self.message_history_position = Some(next);
            self.set_composer(self.message_history[next].clone());
        } else {
            self.message_history_position = None;
            let draft = std::mem::take(&mut self.message_history_draft);
            self.set_composer(draft);
        }
    }

    fn set_composer(&mut self, text: String) {
        self.composer.text = text;
        self.composer.cursor = self.composer.text.len();
    }

    pub fn push(&mut self, block: TranscriptBlock) {
        self.blocks.push(block);
        self.mark_transcript_dirty();
        if self.at_bottom {
            self.scroll = usize::MAX;
        } else {
            self.new_output = true;
        }
    }

    pub fn upsert(&mut self, id: &str, block: TranscriptBlock) {
        if let Some(existing) = self
            .blocks
            .iter_mut()
            .find(|candidate| candidate.id.as_deref() == Some(id))
        {
            let expanded = existing.expanded;
            *existing = block;
            existing.expanded = expanded;
            self.mark_transcript_dirty();
            if self.at_bottom {
                self.scroll = usize::MAX;
            } else {
                self.new_output = true;
            }
        } else {
            self.push(block);
        }
    }

    pub fn append_delta(&mut self, id: &str, kind: BlockKind, title: &str, delta: &str) {
        if let Some(block) = self
            .blocks
            .iter_mut()
            .find(|candidate| candidate.id.as_deref() == Some(id))
        {
            block.text.push_str(delta);
            self.mark_transcript_dirty();
        } else {
            let mut block = TranscriptBlock::new(kind, title, delta);
            block.id = Some(id.to_string());
            self.push(block);
        }
        if self.at_bottom {
            self.scroll = usize::MAX;
        } else {
            self.new_output = true;
        }
    }

    pub fn clear_blocks(&mut self) {
        self.blocks.clear();
        self.mark_transcript_dirty();
    }

    pub fn mark_transcript_dirty(&mut self) {
        self.transcript_revision = self.transcript_revision.wrapping_add(1);
    }

    pub fn present_server_prompt(&mut self, prompt: ServerPrompt) {
        if matches!(self.popup, Some(Popup::Approval(_) | Popup::UserInput(_))) {
            self.pending_server_requests.push_back(prompt);
            return;
        }
        self.popup = Some(match prompt {
            ServerPrompt::Approval(approval) => Popup::Approval(approval),
            ServerPrompt::UserInput(request) => Popup::UserInput(request),
        });
    }

    pub fn show_next_server_prompt(&mut self) {
        if matches!(self.popup, Some(Popup::Approval(_) | Popup::UserInput(_))) {
            return;
        }
        if let Some(prompt) = self.pending_server_requests.pop_front() {
            self.popup = Some(match prompt {
                ServerPrompt::Approval(approval) => Popup::Approval(approval),
                ServerPrompt::UserInput(request) => Popup::UserInput(request),
            });
        }
    }

    pub fn resolve_server_prompt(&mut self, id: &Value) {
        let current_matches = match self.popup.as_ref() {
            Some(Popup::Approval(approval)) => &approval.id == id,
            Some(Popup::UserInput(request)) => &request.id == id,
            _ => false,
        };
        if current_matches {
            self.popup = None;
            self.show_next_server_prompt();
        } else {
            self.pending_server_requests
                .retain(|request| request.id() != id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AppState, BlockKind, Composer, TranscriptBlock};

    #[test]
    fn composer_edits_unicode_on_boundaries() {
        let mut composer = Composer::default();
        composer.insert('я');
        composer.insert('🙂');
        composer.left();
        composer.backspace();
        assert_eq!(composer.text, "🙂");
        assert_eq!(composer.cursor, 0);
        composer.delete();
        assert!(composer.text.is_empty());

        composer.insert_str("привет");
        assert_eq!(composer.text, "привет");
        assert_eq!(composer.cursor, composer.text.len());
    }

    #[test]
    fn streaming_delta_keeps_following_the_bottom() {
        let mut state = AppState::new("/project".into(), true);
        state.blocks.clear();
        let mut block = TranscriptBlock::new(BlockKind::Commentary, "", "first");
        block.id = Some("message".into());
        state.blocks.push(block);
        state.scroll = 12;
        state.at_bottom = true;

        state.append_delta("message", BlockKind::Commentary, "", " second");

        assert_eq!(state.blocks[0].text, "first second");
        assert_eq!(state.scroll, usize::MAX);
    }

    #[test]
    fn message_history_recalls_messages_and_restores_the_draft() {
        let mut state = AppState::new("/project".into(), true);
        state.remember_message("first");
        state.remember_message("second");
        state.composer.insert_str("draft");

        state.previous_message();
        assert_eq!(state.composer.text, "second");
        state.previous_message();
        assert_eq!(state.composer.text, "first");
        state.next_message();
        assert_eq!(state.composer.text, "second");
        state.next_message();
        assert_eq!(state.composer.text, "draft");
        assert_eq!(state.composer.cursor, state.composer.text.len());
    }
}
