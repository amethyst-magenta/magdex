use std::collections::VecDeque;

use ratatui::text::Line;
use serde_json::Value;
use unicode_width::UnicodeWidthChar;

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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionStatus {
    InProgress,
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandActionKind {
    Read,
    ListFiles,
    Search,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandAction {
    pub kind: CommandActionKind,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileChangeKind {
    Add,
    Delete,
    Update,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileChange {
    pub kind: FileChangeKind,
    pub path: String,
    pub move_path: Option<String>,
    pub diff: String,
}

#[derive(Clone, Debug)]
pub struct TranscriptBlock {
    pub id: Option<String>,
    pub kind: BlockKind,
    pub title: String,
    pub text: String,
    pub expanded: bool,
    pub action_status: Option<ActionStatus>,
    pub exit_code: Option<i64>,
    pub command_actions: Vec<CommandAction>,
    pub file_changes: Vec<FileChange>,
}

impl TranscriptBlock {
    pub fn new(kind: BlockKind, title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            id: None,
            kind,
            title: title.into(),
            text: text.into(),
            expanded: false,
            action_status: None,
            exit_code: None,
            command_actions: vec![],
            file_changes: vec![],
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResumeScope {
    CurrentDirectory,
    AllDirectories,
}

impl ResumeScope {
    pub fn toggled(self) -> Self {
        match self {
            Self::CurrentDirectory => Self::AllDirectories,
            Self::AllDirectories => Self::CurrentDirectory,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ResumePicker {
    pub selected: usize,
    pub loading: bool,
    pub error: Option<String>,
    pub scope: ResumeScope,
}

#[derive(Clone, Debug)]
pub struct TrustDirectoryPrompt {
    pub cwd: String,
    pub trust_target: String,
    pub selected: usize,
    pub saving: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Popup {
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
        scope: ResumeScope,
    },
    History {
        selected: usize,
    },
    Login {
        url: Option<String>,
        error: Option<String>,
    },
    TrustDirectory(TrustDirectoryPrompt),
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
    preferred_column: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ComposerLayout {
    pub lines: Vec<String>,
    pub cursor_row: usize,
    pub cursor_col: usize,
    cursor_positions: Vec<(usize, usize, usize)>,
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
        self.preferred_column = None;
    }

    pub fn insert_str(&mut self, text: &str) {
        self.text.insert_str(self.cursor, text);
        self.cursor += text.len();
        self.preferred_column = None;
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
        self.preferred_column = None;
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
        self.preferred_column = None;
    }

    pub fn left(&mut self) {
        self.cursor = self.text[..self.cursor]
            .char_indices()
            .next_back()
            .map(|(index, _)| index)
            .unwrap_or(0);
        self.preferred_column = None;
    }

    pub fn right(&mut self) {
        if self.cursor < self.text.len() {
            self.cursor = self.text[self.cursor..]
                .char_indices()
                .nth(1)
                .map(|(offset, _)| self.cursor + offset)
                .unwrap_or(self.text.len());
        }
        self.preferred_column = None;
    }

    pub fn up(&mut self, width: usize) {
        self.move_vertical(width, -1);
    }

    pub fn down(&mut self, width: usize) {
        self.move_vertical(width, 1);
    }

    fn move_vertical(&mut self, width: usize, direction: isize) {
        let layout = layout_composer(&self.text, self.cursor, width);
        let target_row = if direction < 0 {
            layout.cursor_row.checked_sub(1)
        } else {
            layout
                .cursor_row
                .checked_add(1)
                .filter(|row| *row < layout.lines.len())
        };
        let Some(target_row) = target_row else {
            return;
        };
        let target_column = self.preferred_column.unwrap_or(layout.cursor_col);
        if let Some((cursor, _, _)) = layout
            .cursor_positions
            .iter()
            .filter(|(_, row, _)| *row == target_row)
            .min_by_key(|(_, _, col)| col.abs_diff(target_column))
        {
            self.cursor = *cursor;
            self.preferred_column = Some(target_column);
        }
    }

    pub fn replace(&mut self, text: String) {
        self.text = text;
        self.cursor = self.text.len();
        self.preferred_column = None;
    }

    pub fn clear(&mut self) -> String {
        self.cursor = 0;
        self.preferred_column = None;
        std::mem::take(&mut self.text)
    }
}

pub(crate) fn layout_composer(text: &str, cursor: usize, width: usize) -> ComposerLayout {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut cursor_positions = Vec::new();
    let mut row = 0;
    let mut col = 0;
    let mut cursor_position = None;

    let mut previous = None;
    for (index, ch) in text.char_indices() {
        let char_width = ch.width().unwrap_or(0);
        let word_start = ch != '\n'
            && !ch.is_whitespace()
            && previous.is_none_or(|previous: char| previous.is_whitespace());
        if word_start {
            let word_width = text[index..]
                .chars()
                .take_while(|candidate| !candidate.is_whitespace())
                .map(|candidate| candidate.width().unwrap_or(0))
                .sum::<usize>();
            if col > 0 && col + word_width > width {
                row += 1;
                col = 0;
                lines.push(String::new());
            }
        }
        if ch != '\n' && col > 0 && col + char_width > width {
            row += 1;
            col = 0;
            lines.push(String::new());
            if ch.is_whitespace() {
                cursor_positions.push((index, row, col));
                if index == cursor {
                    cursor_position = Some((row, col));
                }
                previous = Some(ch);
                continue;
            }
        }
        cursor_positions.push((index, row, col));
        if index == cursor {
            cursor_position = Some((row, col));
        }
        if ch == '\n' {
            row += 1;
            col = 0;
            lines.push(String::new());
        } else {
            lines[row].push(ch);
            col += char_width;
            if col == width && index + ch.len_utf8() == cursor {
                row += 1;
                col = 0;
                lines.push(String::new());
                cursor_position = Some((row, col));
            }
        }
        previous = Some(ch);
    }
    if cursor == text.len() && cursor_position.is_none() {
        if col >= width {
            row += 1;
            col = 0;
            lines.push(String::new());
        }
        cursor_position = Some((row, col));
    }
    let end_position = (row, col);
    let (cursor_row, cursor_col) = cursor_position.unwrap_or(end_position);
    cursor_positions.push((text.len(), end_position.0, end_position.1));
    ComposerLayout {
        lines,
        cursor_row,
        cursor_col,
        cursor_positions,
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
    pub transcript_block_offsets: Vec<Option<usize>>,
    pub composer: Composer,
    pub message_history: Vec<String>,
    pub message_history_position: Option<usize>,
    pub message_history_draft: String,
    pub composer_width: usize,
    pub image_attachments: Vec<ImageAttachment>,
    pub popup: Option<Popup>,
    pub pending_server_requests: VecDeque<ServerPrompt>,
    pub scroll: usize,
    pub transcript_max_scroll: usize,
    pub at_bottom: bool,
    pub new_output: bool,
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
        let project = display_path(&cwd, std::env::var("HOME").ok().as_deref());
        Self {
            cwd,
            project,
            thread_id: None,
            turn_id: None,
            turn_started_at: None,
            model: None,
            effort: None,
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
            transcript_block_offsets: vec![],
            composer: Composer::default(),
            message_history: vec![],
            message_history_position: None,
            message_history_draft: String::new(),
            composer_width: 74,
            image_attachments: vec![],
            popup: None,
            pending_server_requests: VecDeque::new(),
            scroll: 0,
            transcript_max_scroll: 0,
            at_bottom: true,
            new_output: false,
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

    pub fn jump_to_bottom(&mut self) {
        self.scroll = usize::MAX;
        self.at_bottom = true;
        self.new_output = false;
    }

    pub fn jump_to_user_message(&mut self, index: usize) -> bool {
        let Some(block_index) = self
            .blocks
            .iter()
            .enumerate()
            .filter(|(_, block)| block.kind == BlockKind::User)
            .nth(index)
            .map(|(index, _)| index)
        else {
            return false;
        };
        let Some(offset) = self
            .transcript_block_offsets
            .get(block_index)
            .copied()
            .flatten()
        else {
            return false;
        };
        self.scroll = offset;
        self.at_bottom = false;
        self.new_output = false;
        true
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
        self.composer.replace(text);
    }

    pub fn push(&mut self, block: TranscriptBlock) {
        if block.kind == BlockKind::Error
            && self.blocks.last().is_some_and(|previous| {
                previous.kind == BlockKind::Error && previous.text == block.text
            })
        {
            return;
        }
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

pub fn display_path(path: &str, home: Option<&str>) -> String {
    let Some(home) = home.filter(|home| !home.is_empty()) else {
        return path.to_string();
    };
    if path == home {
        return "~".to_string();
    }
    path.strip_prefix(home)
        .and_then(|suffix| suffix.strip_prefix('/'))
        .map(|suffix| format!("~/{suffix}"))
        .unwrap_or_else(|| path.to_string())
}

#[cfg(test)]
mod tests {
    use super::{display_path, AppState, BlockKind, Composer, TranscriptBlock};

    #[test]
    fn display_path_replaces_the_home_prefix() {
        assert_eq!(
            display_path("/home/user/magdex", Some("/home/user")),
            "~/magdex"
        );
        assert_eq!(display_path("/home/user", Some("/home/user")), "~");
        assert_eq!(
            display_path("/home/username/magdex", Some("/home/user")),
            "/home/username/magdex"
        );
    }

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
    fn composer_arrows_move_between_visual_lines() {
        let mut composer = Composer::default();
        composer.insert_str("alpha beta gamma");

        composer.up(10);
        assert_eq!(composer.cursor, "alpha".len());
        composer.down(10);
        assert_eq!(composer.cursor, composer.text.len());

        composer.replace("abcd\nef\nwxyz".into());
        composer.up(20);
        assert_eq!(composer.cursor, "abcd\nef".len());
        composer.up(20);
        assert_eq!(composer.cursor, "abcd".len());
        composer.down(20);
        assert_eq!(composer.cursor, "abcd\nef".len());
        composer.down(20);
        assert_eq!(composer.cursor, composer.text.len());
    }

    #[test]
    fn composer_arrows_do_nothing_on_a_single_visual_line() {
        let mut composer = Composer::default();
        composer.insert_str("draft");
        composer.left();
        let cursor = composer.cursor;

        composer.up(80);
        composer.down(80);

        assert_eq!(composer.cursor, cursor);
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
    fn adjacent_duplicate_errors_are_shown_once() {
        let mut state = AppState::new("/project".into(), true);
        state.clear_blocks();

        state.push(TranscriptBlock::new(
            BlockKind::Error,
            "Error",
            "limit reached",
        ));
        state.push(TranscriptBlock::new(
            BlockKind::Error,
            "Error",
            "limit reached",
        ));

        assert_eq!(state.blocks.len(), 1);
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

    #[test]
    fn history_selection_jumps_to_the_user_block() {
        let mut state = AppState::new("/project".into(), true);
        state.blocks = vec![
            TranscriptBlock::new(BlockKind::User, "You", "first"),
            TranscriptBlock::new(BlockKind::Assistant, "Codex", "answer"),
            TranscriptBlock::new(BlockKind::User, "You", "second"),
        ];
        state.transcript_block_offsets = vec![Some(2), Some(6), Some(9)];

        assert!(state.jump_to_user_message(1));

        assert_eq!(state.scroll, 9);
        assert!(!state.at_bottom);
    }

    #[test]
    fn jump_to_bottom_clears_the_new_output_marker() {
        let mut state = AppState::new("/project".into(), true);
        state.scroll = 12;
        state.at_bottom = false;
        state.new_output = true;

        state.jump_to_bottom();

        assert_eq!(state.scroll, usize::MAX);
        assert!(state.at_bottom);
        assert!(!state.new_output);
    }
}
