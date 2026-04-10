use std::io;
use std::time::{SystemTime, UNIX_EPOCH};

use aistatus_app::{RefreshState, RefreshStatus, RefreshedProfile, run_refresh_cycle};
use aistatus_core::{
    AccountHealth, AccountMembership, AuthMode, MembershipTier, ProviderKind, QuotaWindow,
    RefreshCommand, UsageFamily,
};
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use serde::Deserialize;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct TuiModel {
    pub title: String,
    pub state: RefreshState,
    pub selected_profile: usize,
    pub show_help: bool,
    pub now_epoch_secs: u64,
    refresh_command: Option<RefreshCommand>,
    detail_view: DetailView,
    last_refresh_feedback: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailView {
    Account,
    Diagnostics,
}

impl TuiModel {
    pub fn new(title: impl Into<String>, state: RefreshState) -> Self {
        Self {
            title: title.into(),
            now_epoch_secs: infer_now_epoch_secs(&state),
            state,
            selected_profile: 0,
            show_help: false,
            refresh_command: None,
            detail_view: DetailView::Account,
            last_refresh_feedback: None,
        }
    }

    pub fn with_refresh_command(mut self, refresh_command: Option<RefreshCommand>) -> Self {
        if let Some(now_epoch_secs) = refresh_command
            .as_ref()
            .and_then(|command| command.now_epoch_secs)
        {
            self.now_epoch_secs = now_epoch_secs;
        }
        self.refresh_command = refresh_command;
        self
    }

    pub fn selected_profile(&self) -> Option<&RefreshedProfile> {
        self.state.profiles.values().nth(self.selected_profile)
    }

    fn selected_profile_id(&self) -> Option<String> {
        self.state
            .profiles
            .keys()
            .nth(self.selected_profile)
            .cloned()
    }

    pub fn next(&mut self) {
        let len = self.state.profiles.len();
        if len == 0 {
            self.selected_profile = 0;
        } else {
            self.selected_profile = (self.selected_profile + 1) % len;
        }
    }

    pub fn previous(&mut self) {
        let len = self.state.profiles.len();
        if len == 0 {
            self.selected_profile = 0;
        } else if self.selected_profile == 0 {
            self.selected_profile = len - 1;
        } else {
            self.selected_profile -= 1;
        }
    }

    pub fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    pub fn show_account_view(&mut self) {
        self.detail_view = DetailView::Account;
    }

    pub fn show_diagnostics_view(&mut self) {
        self.detail_view = DetailView::Diagnostics;
    }

    pub fn refresh_selected(&mut self) {
        let selected_id = self.selected_profile_id();
        let Some(mut command) = self.refresh_command.clone() else {
            self.last_refresh_feedback =
                Some("refresh unavailable: no fixture/config refresh source configured".into());
            return;
        };

        if command.now_epoch_secs.is_none() {
            command.now_epoch_secs = Some(self.now_epoch_secs.max(current_epoch_secs()));
        }

        match run_refresh_cycle(&command) {
            Ok(run) => {
                self.now_epoch_secs = run.now_epoch_secs;
                self.state = run.state;
                self.sync_selected_profile(selected_id.as_deref());
                self.last_refresh_feedback = selected_id
                    .as_deref()
                    .and_then(|profile_id| run.profile_lines.get(profile_id).cloned())
                    .or_else(|| Some(run.output.render()));
            }
            Err(error) => {
                self.last_refresh_feedback = Some(format!("refresh failed: {error}"));
            }
        }
    }

    fn sync_selected_profile(&mut self, selected_id: Option<&str>) {
        if let Some(selected_id) = selected_id
            && let Some(index) = self.state.profiles.keys().position(|id| id == selected_id)
        {
            self.selected_profile = index;
            return;
        }

        if self.state.profiles.is_empty() {
            self.selected_profile = 0;
        } else {
            self.selected_profile = self.selected_profile.min(self.state.profiles.len() - 1);
        }
    }
}

#[derive(Debug, Error)]
pub enum TuiError {
    #[error("io failure: {0}")]
    Io(#[from] io::Error),
    #[error("fixture parse failure: {0}")]
    Fixture(String),
}

pub fn render_to_string(model: &TuiModel, width: u16, height: u16) -> String {
    let mut lines = Vec::new();
    lines.push(format!("{} [{}x{}]", model.title, width, height));
    lines.push("=".repeat(width.min(80) as usize));

    for (index, profile) in model.state.profiles.values().enumerate() {
        let selected = if index == model.selected_profile {
            ">"
        } else {
            " "
        };
        let membership = profile
            .profile
            .membership
            .as_ref()
            .map(render_membership)
            .unwrap_or_else(|| "n/a".into());
        let five_hour = profile
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.primary_window())
            .map(render_window_summary)
            .unwrap_or_else(|| "5h: --".into());
        let weekly = profile
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.secondary_window())
            .map(render_window_summary)
            .unwrap_or_else(|| "weekly: --".into());
        lines.push(format!(
            "{selected} {} | status={} | provider={} | health={} | age={} | {} | {} | membership={}",
            profile.profile.display_name,
            render_status(&profile.status),
            render_provider(&profile.profile.provider),
            render_health(&profile.profile.health),
            render_refresh_age(model.now_epoch_secs, profile.last_updated_at),
            five_hour,
            weekly,
            membership,
        ));
    }

    lines.push(String::new());
    if let Some(profile) = model.selected_profile() {
        lines.push(format!("Detail: {}", profile.profile.display_name));
        lines.push(format!("  view: {}", render_detail_view(model.detail_view)));
        lines.push(format!("  account: {}", render_account_kind(profile)));
        lines.push(format!(
            "  membership: {}",
            profile
                .profile
                .membership
                .as_ref()
                .map(render_membership)
                .unwrap_or_else(|| "n/a".into())
        ));
        lines.push(format!(
            "  usage family: {}",
            match profile.usage_family {
                UsageFamily::SubscriptionQuota => "subscription_quota",
                UsageFamily::Api => "api",
            }
        ));
        lines.push(format!(
            "  provider: {}",
            render_provider(&profile.profile.provider)
        ));
        lines.push(format!(
            "  auth mode: {}",
            render_auth_mode(&profile.profile.auth_mode)
        ));
        lines.push(format!(
            "  refresh interval: {}s",
            profile.profile.refresh_policy.refresh_interval_secs
        ));
        lines.push(format!(
            "  manual refresh: {}",
            if profile.profile.refresh_policy.allow_manual_refresh {
                "enabled"
            } else {
                "disabled"
            }
        ));
        lines.push(format!("  status: {}", render_status(&profile.status)));
        lines.push(format!(
            "  health: {}",
            render_health(&profile.profile.health)
        ));
        lines.push(format!(
            "  last refresh age: {}",
            render_refresh_age(model.now_epoch_secs, profile.last_updated_at)
        ));
        lines.push(format!(
            "  protocol compatibility: {}",
            render_protocol_compatibility(profile)
        ));
        lines.push(format!(
            "  last error: {}",
            profile.last_error.clone().unwrap_or_else(|| "none".into())
        ));
        match model.detail_view {
            DetailView::Account => {
                lines.push(format!(
                    "5h summary: {}",
                    profile
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.primary_window())
                        .map(render_window_detail)
                        .unwrap_or_else(|| "unavailable".into())
                ));
                lines.push(format!(
                    "weekly summary: {}",
                    profile
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.secondary_window())
                        .map(render_window_detail)
                        .unwrap_or_else(|| "unavailable".into())
                ));
            }
            DetailView::Diagnostics => {
                lines.push(format!(
                    "last refresh feedback: {}",
                    model
                        .last_refresh_feedback
                        .clone()
                        .unwrap_or_else(|| "none".into())
                ));
                if let Some(snapshot) = &profile.snapshot {
                    for window in &snapshot.windows {
                        lines.push(format!("  - {}", render_window_detail(window)));
                    }
                } else {
                    lines.push("  no quota snapshot".into());
                }
            }
        }
    }

    if model.show_help {
        lines.push(String::new());
        lines.push(
            "Help: j/k or arrows move | r refresh | a account view | d diagnostics view | ? help | q quit"
                .into(),
        );
    }

    lines.join("\n")
}

pub fn render_frame(frame: &mut Frame<'_>, model: &TuiModel) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(frame.area());

    render_account_list(frame, chunks[0], model);
    render_detail_panel(frame, chunks[1], model);

    if model.show_help {
        render_help_overlay(frame, centered_rect(60, 35, frame.area()));
    }
}

pub fn run_fixture_tui(model: &mut TuiModel) -> Result<(), TuiError> {
    let mut session = TerminalSession::enter()?;
    session.activate_terminal()?;
    let terminal = session.terminal_mut()?;

    loop {
        terminal.draw(|frame| render_frame(frame, model))?;

        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') => break,
                KeyCode::Char('j') | KeyCode::Down => model.next(),
                KeyCode::Char('k') | KeyCode::Up => model.previous(),
                KeyCode::Char('a') => model.show_account_view(),
                KeyCode::Char('d') => model.show_diagnostics_view(),
                KeyCode::Char('?') => model.toggle_help(),
                KeyCode::Char('r') => model.refresh_selected(),
                _ => {}
            }
        }
    }

    Ok(())
}

struct TerminalSession {
    stdout: Option<io::Stdout>,
    terminal: Option<Terminal<CrosstermBackend<io::Stdout>>>,
}

impl TerminalSession {
    fn enter() -> Result<Self, TuiError> {
        let mut session = Self {
            stdout: Some(io::stdout()),
            terminal: None,
        };

        enable_raw_mode()?;

        let stdout = session
            .stdout
            .as_mut()
            .ok_or_else(|| TuiError::Io(io::Error::other("terminal session stdout unavailable")))?;
        execute!(stdout, EnterAlternateScreen)?;

        Ok(session)
    }

    fn activate_terminal(&mut self) -> Result<(), TuiError> {
        if self.terminal.is_some() {
            return Ok(());
        }

        let stdout = self
            .stdout
            .take()
            .ok_or_else(|| TuiError::Io(io::Error::other("terminal session stdout unavailable")))?;
        let backend = CrosstermBackend::new(stdout);
        let terminal = Terminal::new(backend)?;
        self.terminal = Some(terminal);
        Ok(())
    }

    fn terminal_mut(&mut self) -> Result<&mut Terminal<CrosstermBackend<io::Stdout>>, TuiError> {
        self.terminal
            .as_mut()
            .ok_or_else(|| TuiError::Io(io::Error::other("terminal session unavailable")))
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        if let Some(mut terminal) = self.terminal.take() {
            let _ = disable_raw_mode();
            let _ = leave_alternate_screen(terminal.backend_mut());
            let _ = terminal.show_cursor();
            return;
        }

        if let Some(mut stdout) = self.stdout.take() {
            let _ = disable_raw_mode();
            let _ = leave_alternate_screen(&mut stdout);
        }
    }
}

fn leave_alternate_screen<W: io::Write>(writer: &mut W) -> io::Result<()> {
    execute!(writer, LeaveAlternateScreen)
}

/// Fixture payload for the TUI, including optional refresh wiring so `r` can replay the shared
/// orchestration path during local smoke runs.
#[derive(Debug, Deserialize)]
pub struct LoadedTuiFixture {
    pub state: RefreshState,
    #[serde(default)]
    pub refresh_command: Option<RefreshCommand>,
}

pub fn load_fixture(json: &str) -> Result<LoadedTuiFixture, TuiError> {
    serde_json::from_str(json).map_err(|error| TuiError::Fixture(error.to_string()))
}

pub fn load_fixture_state(json: &str) -> Result<RefreshState, TuiError> {
    Ok(load_fixture(json)?.state)
}

fn render_account_list(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let items: Vec<ListItem<'_>> = model
        .state
        .profiles
        .values()
        .map(|profile| {
            let membership = profile
                .profile
                .membership
                .as_ref()
                .map(render_membership)
                .unwrap_or_else(|| "n/a".into());
            let content = vec![
                Line::from(vec![
                    Span::styled(
                        profile.profile.display_name.clone(),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  [{}]", render_status(&profile.status)),
                        Style::default().fg(render_status_color(&profile.status)),
                    ),
                ]),
                Line::from(format!(
                    "provider={} | health={} | age={}",
                    render_provider(&profile.profile.provider),
                    render_health(&profile.profile.health),
                    render_refresh_age(model.now_epoch_secs, profile.last_updated_at)
                )),
                Line::from(format!(
                    "{} | {} | membership={}",
                    profile
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.primary_window())
                        .map(render_window_summary)
                        .unwrap_or_else(|| "5h: --".into()),
                    profile
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.secondary_window())
                        .map(render_window_summary)
                        .unwrap_or_else(|| "weekly: --".into()),
                    membership
                )),
            ];
            ListItem::new(content)
        })
        .collect();

    let mut state = ListState::default();
    if !items.is_empty() {
        state.select(Some(model.selected_profile));
    }

    let list = List::new(items)
        .block(Block::default().title("Accounts").borders(Borders::ALL))
        .highlight_style(Style::default().bg(Color::Blue).fg(Color::Black))
        .highlight_symbol("▶ ");

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_detail_panel(frame: &mut Frame<'_>, area: Rect, model: &TuiModel) {
    let detail = if let Some(profile) = model.selected_profile() {
        let mut lines = vec![
            Line::from(vec![Span::styled(
                profile.profile.display_name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::from(format!(
                "view: {}   [a] account  [d] diagnostics",
                render_detail_view(model.detail_view)
            )),
            Line::from(format!("account kind: {}", render_account_kind(profile))),
            Line::from(format!(
                "membership: {}",
                profile
                    .profile
                    .membership
                    .as_ref()
                    .map(render_membership)
                    .unwrap_or_else(|| "n/a".into())
            )),
            Line::from(format!(
                "provider: {}",
                render_provider(&profile.profile.provider)
            )),
            Line::from(format!(
                "auth mode: {}",
                render_auth_mode(&profile.profile.auth_mode)
            )),
            Line::from(format!(
                "refresh interval: {}s",
                profile.profile.refresh_policy.refresh_interval_secs
            )),
            Line::from(format!(
                "manual refresh: {}",
                if profile.profile.refresh_policy.allow_manual_refresh {
                    "enabled"
                } else {
                    "disabled"
                }
            )),
            Line::from(format!("status: {}", render_status(&profile.status))),
            Line::from(format!(
                "health: {}",
                render_health(&profile.profile.health)
            )),
            Line::from(format!(
                "last refresh age: {}",
                render_refresh_age(model.now_epoch_secs, profile.last_updated_at)
            )),
            Line::from(format!(
                "protocol compatibility: {}",
                render_protocol_compatibility(profile)
            )),
            Line::from(format!(
                "last error: {}",
                profile.last_error.clone().unwrap_or_else(|| "none".into())
            )),
            Line::from(String::new()),
        ];

        match model.detail_view {
            DetailView::Account => {
                lines.push(Line::from(format!(
                    "5h summary: {}",
                    profile
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.primary_window())
                        .map(render_window_detail)
                        .unwrap_or_else(|| "unavailable".into())
                )));
                lines.push(Line::from(format!(
                    "weekly summary: {}",
                    profile
                        .snapshot
                        .as_ref()
                        .and_then(|snapshot| snapshot.secondary_window())
                        .map(render_window_detail)
                        .unwrap_or_else(|| "unavailable".into())
                )));
            }
            DetailView::Diagnostics => {
                lines.push(Line::from(format!(
                    "last refresh feedback: {}",
                    model
                        .last_refresh_feedback
                        .clone()
                        .unwrap_or_else(|| "none".into())
                )));
                if let Some(snapshot) = &profile.snapshot {
                    for window in &snapshot.windows {
                        lines.push(Line::from(render_window_detail(window)));
                    }
                } else {
                    lines.push(Line::from("no quota snapshot"));
                }
            }
        }
        lines
    } else {
        vec![Line::from("no profiles loaded")]
    };

    let paragraph = Paragraph::new(detail)
        .block(
            Block::default()
                .title(format!(
                    "Detail ({})",
                    render_detail_view(model.detail_view)
                ))
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(paragraph, area);
}

fn render_help_overlay(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Clear, area);
    let help = Paragraph::new(vec![
        Line::from("Keys"),
        Line::from("j / ↓ : next account"),
        Line::from("k / ↑ : previous account"),
        Line::from("a     : account view"),
        Line::from("d     : diagnostics view"),
        Line::from("r     : run refresh and show feedback"),
        Line::from("?     : toggle help"),
        Line::from("q     : quit"),
    ])
    .block(Block::default().title("Help").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(help, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn render_status(status: &RefreshStatus) -> &'static str {
    match status {
        RefreshStatus::Healthy => "healthy",
        RefreshStatus::Stale => "stale",
        RefreshStatus::AuthRequired => "auth_required",
        RefreshStatus::RateLimited => "rate_limited",
        RefreshStatus::Error => "error",
        RefreshStatus::Refreshing => "refreshing",
    }
}

fn render_status_color(status: &RefreshStatus) -> Color {
    match status {
        RefreshStatus::Healthy => Color::Green,
        RefreshStatus::Stale => Color::Yellow,
        RefreshStatus::AuthRequired | RefreshStatus::RateLimited => Color::LightRed,
        RefreshStatus::Error => Color::Red,
        RefreshStatus::Refreshing => Color::Cyan,
    }
}

fn render_account_kind(profile: &RefreshedProfile) -> &'static str {
    match profile.profile.account_kind {
        aistatus_core::AccountKind::Chatgpt => "chatgpt",
        aistatus_core::AccountKind::ApiKey => "api_key",
        aistatus_core::AccountKind::Other(_) => "other",
    }
}

fn render_provider(provider: &ProviderKind) -> &str {
    match provider {
        ProviderKind::CodexProtocol => "codex_protocol",
        ProviderKind::OpenAiApiUsage => "openai_api_usage",
        ProviderKind::Other(_) => "other",
    }
}

fn render_auth_mode(auth_mode: &AuthMode) -> &str {
    match auth_mode {
        AuthMode::Browser => "browser",
        AuthMode::Headless => "headless",
        AuthMode::ApiKey => "api_key",
    }
}

fn render_health(health: &AccountHealth) -> &str {
    match health {
        AccountHealth::Healthy => "healthy",
        AccountHealth::Stale => "stale",
        AccountHealth::AuthExpired => "auth_expired",
        AccountHealth::RateLimited => "rate_limited",
        AccountHealth::Degraded => "degraded",
        AccountHealth::Error => "error",
    }
}

fn render_detail_view(view: DetailView) -> &'static str {
    match view {
        DetailView::Account => "account",
        DetailView::Diagnostics => "diagnostics",
    }
}

fn render_membership(membership: &AccountMembership) -> String {
    match membership.tier {
        MembershipTier::Free => "free".into(),
        MembershipTier::Go => "go".into(),
        MembershipTier::Plus => "plus".into(),
        MembershipTier::Pro => "pro".into(),
        MembershipTier::Team => "team".into(),
        MembershipTier::Edu => "edu".into(),
        MembershipTier::Business => "business".into(),
        MembershipTier::Enterprise => "enterprise".into(),
        MembershipTier::Unknown => "unknown".into(),
        MembershipTier::Other => membership
            .raw_plan_type
            .clone()
            .unwrap_or_else(|| "other".into()),
    }
}

fn render_refresh_age(now_epoch_secs: u64, last_updated_at: Option<u64>) -> String {
    match last_updated_at {
        Some(last_updated_at) => format_duration(now_epoch_secs.saturating_sub(last_updated_at)),
        None => "never".into(),
    }
}

fn render_protocol_compatibility(profile: &RefreshedProfile) -> String {
    match &profile.profile.provider {
        ProviderKind::CodexProtocol => {
            let incompatible = profile.last_error.as_ref().is_some_and(|error| {
                let normalized = error.to_ascii_lowercase();
                normalized.contains("schema incompatibility")
                    || normalized.contains("incompatible schema")
                    || normalized.contains("expected v2")
            });

            if incompatible {
                "incompatible (schema drift)".into()
            } else {
                "compatible (schema v2)".into()
            }
        }
        ProviderKind::OpenAiApiUsage => "n/a (api usage provider)".into(),
        ProviderKind::Other(raw) => format!("unknown ({raw})"),
    }
}

fn infer_now_epoch_secs(state: &RefreshState) -> u64 {
    state
        .profiles
        .values()
        .filter_map(|profile| profile.last_updated_at)
        .max()
        .unwrap_or_else(current_epoch_secs)
}

fn current_epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn format_duration(seconds: u64) -> String {
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", seconds / 86_400)
    }
}

fn render_window_summary(window: &QuotaWindow) -> String {
    let label = match window.kind {
        aistatus_core::QuotaWindowKind::FiveHour => "5h",
        aistatus_core::QuotaWindowKind::Weekly => "weekly",
        aistatus_core::QuotaWindowKind::Unknown => window.label.as_str(),
    };
    format!("{label}: {:.1}%", window.used_percent)
}

fn render_window_detail(window: &QuotaWindow) -> String {
    format!(
        "{} | {:.1}% used | resets {} | {} mins",
        window.label, window.used_percent, window.resets_at, window.window_duration_mins
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aistatus_app::{RefreshState, RefreshStatus};
    use aistatus_config::{ConfiguredProfile, SecretRef};
    use aistatus_core::{
        AccountHealth, AccountKind, AccountMembership, AccountProfile, AuthMode,
        FutureSwitchBridge, MembershipTier, ProtocolRateLimitWindow, ProviderKind, QuotaSnapshot,
        RefreshPolicy,
    };

    fn sample_state() -> RefreshState {
        let mut state = RefreshState::new(&[
            ConfiguredProfile {
                profile: AccountProfile {
                    id: "acct-plus".into(),
                    display_name: "Primary ChatGPT".into(),
                    provider: ProviderKind::CodexProtocol,
                    auth_mode: AuthMode::Browser,
                    account_kind: AccountKind::Chatgpt,
                    membership: Some(AccountMembership {
                        tier: MembershipTier::Plus,
                        raw_plan_type: Some("plus".into()),
                    }),
                    health: AccountHealth::Healthy,
                    refresh_policy: RefreshPolicy {
                        refresh_interval_secs: 300,
                        allow_manual_refresh: true,
                    },
                    future_switch_bridge: FutureSwitchBridge::default(),
                },
                secret_ref: SecretRef::Managed {
                    id: "managed-plus".into(),
                },
            },
            ConfiguredProfile {
                profile: AccountProfile {
                    id: "acct-pro".into(),
                    display_name: "Work Pro".into(),
                    provider: ProviderKind::CodexProtocol,
                    auth_mode: AuthMode::Headless,
                    account_kind: AccountKind::Chatgpt,
                    membership: Some(AccountMembership {
                        tier: MembershipTier::Pro,
                        raw_plan_type: Some("pro".into()),
                    }),
                    health: AccountHealth::Stale,
                    refresh_policy: RefreshPolicy {
                        refresh_interval_secs: 300,
                        allow_manual_refresh: true,
                    },
                    future_switch_bridge: FutureSwitchBridge::default(),
                },
                secret_ref: SecretRef::Managed {
                    id: "managed-pro".into(),
                },
            },
            ConfiguredProfile {
                profile: AccountProfile {
                    id: "acct-broken".into(),
                    display_name: "Broken Browser Session".into(),
                    provider: ProviderKind::CodexProtocol,
                    auth_mode: AuthMode::Browser,
                    account_kind: AccountKind::Chatgpt,
                    membership: Some(AccountMembership {
                        tier: MembershipTier::Unknown,
                        raw_plan_type: Some("unknown".into()),
                    }),
                    health: AccountHealth::Error,
                    refresh_policy: RefreshPolicy {
                        refresh_interval_secs: 300,
                        allow_manual_refresh: true,
                    },
                    future_switch_bridge: FutureSwitchBridge::default(),
                },
                secret_ref: SecretRef::Managed {
                    id: "managed-broken".into(),
                },
            },
        ]);

        if let Some(first) = state.profiles.get_mut("acct-plus") {
            first.snapshot = Some(QuotaSnapshot::from_protocol_windows(vec![
                ProtocolRateLimitWindow {
                    limit_id: "codex-5h".into(),
                    label: None,
                    used_percent: 42.5,
                    window_duration_mins: 300,
                    resets_at: "2026-04-10T05:00:00Z".into(),
                },
                ProtocolRateLimitWindow {
                    limit_id: "codex-weekly".into(),
                    label: None,
                    used_percent: 15.0,
                    window_duration_mins: 10_080,
                    resets_at: "2026-04-14T00:00:00Z".into(),
                },
            ]));
            first.status = RefreshStatus::Healthy;
            first.last_updated_at = Some(1_712_700_000);
        }

        if let Some(second) = state.profiles.get_mut("acct-pro") {
            second.snapshot = Some(QuotaSnapshot::from_protocol_windows(vec![
                ProtocolRateLimitWindow {
                    limit_id: "codex-5h".into(),
                    label: None,
                    used_percent: 82.0,
                    window_duration_mins: 300,
                    resets_at: "2026-04-10T05:00:00Z".into(),
                },
                ProtocolRateLimitWindow {
                    limit_id: "codex-weekly".into(),
                    label: None,
                    used_percent: 65.0,
                    window_duration_mins: 10_080,
                    resets_at: "2026-04-14T00:00:00Z".into(),
                },
            ]));
            second.status = RefreshStatus::Stale;
            second.last_updated_at = Some(1_712_700_300);
        }

        if let Some(third) = state.profiles.get_mut("acct-broken") {
            third.status = RefreshStatus::Error;
            third.last_error = Some("empty browser session payload".into());
        }

        state
    }

    #[test]
    fn view_snapshots_render_fixture_state() {
        let mut model = TuiModel::new("aistatus", sample_state());
        model.next();
        let rendered = render_to_string(&model, 100, 30);

        assert!(rendered.contains("Primary ChatGPT"));
        assert!(rendered.contains("provider=codex_protocol"));
        assert!(rendered.contains("health=healthy"));
        assert!(rendered.contains("age=5m"));
        assert!(rendered.contains("5h: 42.5%"));
        assert!(rendered.contains("weekly: 15.0%"));
        assert!(rendered.contains("refresh interval: 300s"));
        assert!(rendered.contains("auth mode: browser"));
        assert!(rendered.contains("protocol compatibility: compatible (schema v2)"));
        assert!(rendered.contains("weekly summary: Weekly | 15.0% used"));
    }

    #[test]
    fn view_snapshots_toggle_help_and_navigation() {
        let mut model = TuiModel::new("aistatus", sample_state());
        let initial = model
            .selected_profile()
            .map(|profile| profile.profile.id.clone());
        model.toggle_help();
        model.next();
        let rendered = render_to_string(&model, 100, 30);

        assert!(rendered.contains(
            "Help: j/k or arrows move | r refresh | a account view | d diagnostics view | ? help | q quit"
        ));
        assert_ne!(
            model
                .selected_profile()
                .map(|profile| profile.profile.id.clone()),
            initial
        );
    }

    #[test]
    fn diagnostics_view_shows_refresh_feedback() {
        let loaded = load_fixture(include_str!("../tests/fixtures/sample-quotas.json"))
            .expect("fixture should parse");
        let mut model =
            TuiModel::new("aistatus", loaded.state).with_refresh_command(loaded.refresh_command);
        model.show_diagnostics_view();
        model.refresh_selected();
        let rendered = render_to_string(&model, 100, 30);

        assert!(rendered.contains("view: diagnostics"));
        assert!(rendered.contains("last refresh feedback: auth_required: acct-broken"));
        assert!(rendered.contains("protocol compatibility: compatible (schema v2)"));
        assert!(
            rendered.contains("last error: authentication failure: empty browser session payload")
        );
    }

    #[test]
    fn fixture_loader_parses_refresh_state() {
        let loaded = load_fixture(include_str!("../tests/fixtures/sample-quotas.json"))
            .expect("fixture should parse");
        assert_eq!(loaded.state.profiles.len(), 3);
        assert_eq!(
            loaded.refresh_command.and_then(|command| command.fixtures),
            Some("sample-quotas".into())
        );
    }

    #[test]
    fn leave_alternate_screen_emits_escape_sequence() {
        let mut buffer = Vec::new();

        leave_alternate_screen(&mut buffer).expect("leave screen should write escape code");

        assert!(!buffer.is_empty());
    }
}
