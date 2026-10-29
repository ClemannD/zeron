//! Project tabs across the titlebar (fork feature, off by default; View →
//! Show → Project tabs).
//!
//! One tab per SPACE — a device + folder — so parallel clones of one
//! repository each get their own tab; they are never grouped by repository
//! the way the sidebar's project filter groups them. Sessions outside any
//! project share a "No project" tab ([`NO_PROJECT_TAB`]).
//!
//! A tab IS the sidebar filter (`UiSettings.space_filter`), matched exactly
//! while tabs are on ([`AppState::exact_space_filter`]): a worktree session
//! keeps its origin space's id, so it lands under that folder's tab. Opening
//! a session from anywhere (notification, palette, link) switches to its tab.
//! Switching tabs lands on the session last viewed there, else its newest,
//! else the new-session canvas homed on that project.
//!
//! Closing a tab only hides it; the menu at the strip's end lists every
//! project to bring one back. Tabs drag to reorder. ⌘1…⌘8 jump to tabs and
//! ⌘9 to the last one (Chrome's rule); Next/Previous project tab cycle them.

use std::collections::HashMap;
use std::process::Command;
use std::time::Instant;

use super::*;
use crate::settings::{ProjectTabMeta, ProjectTabWidth};
use crate::state::NO_PROJECT_TAB;

/// The tab chips' corner radius.
const PROJECT_TAB_RADIUS: f32 = 8.0;
/// Air above and below the tab chips within the band. The tab itself sits
/// under the top gutter, so its contents and hover chip are offset to match.
const PROJECT_TAB_CHIP_INSET: f32 = 5.0;
/// How often tab folders' checked-out branches are re-read.
const PROJECT_TAB_BRANCH_POLL: Duration = Duration::from_secs(5);
const PROJECT_TAB_HOVER_DWELL: Duration = Duration::from_millis(550);
const PROJECT_TAB_HOVER_WARM: Duration = Duration::from_millis(400);
const PROJECT_TAB_HOVER_WIDTH: f32 = 320.0;

/// Dwell state for the project-tab card. It deliberately follows the session
/// hover-card mechanics: wait while sweeping across tabs, then stay warm when
/// comparing neighbouring projects.
#[derive(Default)]
pub(super) struct ProjectTabHoverCard {
    shown: Option<String>,
    pending: Option<(String, Task<()>)>,
    hidden_at: Option<Instant>,
}

impl ProjectTabHoverCard {
    fn shown(&self) -> Option<&str> {
        self.shown.as_deref()
    }

    fn enter(&mut self, key: &str, now: Instant) -> bool {
        if self.shown() == Some(key) {
            return false;
        }
        let warm = self.shown.is_some()
            || self
                .hidden_at
                .is_some_and(|hidden| now.duration_since(hidden) < PROJECT_TAB_HOVER_WARM);
        if warm {
            self.pending = None;
            self.shown = Some(key.to_owned());
        }
        warm
    }

    fn leave(&mut self, key: &str, now: Instant) -> bool {
        if self.pending.as_ref().is_some_and(|(pending, _)| pending == key) {
            self.pending = None;
        }
        if self.shown() != Some(key) {
            return false;
        }
        self.shown = None;
        self.hidden_at = Some(now);
        true
    }

    fn settle(&mut self, key: &str) -> bool {
        if self
            .pending
            .as_ref()
            .is_none_or(|(pending, _)| pending != key)
        {
            return false;
        }
        self.pending = None;
        self.shown = Some(key.to_owned());
        true
    }

    pub(super) fn dismiss(&mut self) -> bool {
        self.pending = None;
        self.hidden_at = None;
        self.shown.take().is_some()
    }
}

/// Drag payload for reordering project tabs.
#[derive(Clone)]
struct ProjectTabDrag {
    from: usize,
    title: SharedString,
}

/// The visible tabs in strip order: the saved order (keys that still exist,
/// first occurrence wins), then keys new to the strip — in neither list — in
/// `known` order. Pure.
fn visible_project_tabs(order: &[String], hidden: &[String], known: &[String]) -> Vec<String> {
    let mut tabs: Vec<String> = Vec::new();
    for key in order {
        if known.contains(key) && !tabs.contains(key) {
            tabs.push(key.clone());
        }
    }
    for key in known {
        if !tabs.contains(key) && !hidden.contains(key) {
            tabs.push(key.clone());
        }
    }
    tabs
}

/// First enable: a tab for every key with live sessions, the rest start
/// hidden. With no sessions anywhere every project gets a tab ("No project"
/// stays hidden). Returns `(order, hidden)`. Pure.
fn seed_project_tabs(
    known: &[String],
    busy: &std::collections::HashSet<String>,
) -> (Vec<String>, Vec<String>) {
    if busy.is_empty() {
        return known.iter().cloned().partition(|key| key != NO_PROJECT_TAB);
    }
    known.iter().cloned().partition(|key| busy.contains(key))
}

/// Hide `key` from the visible `tabs`. Returns the new `(order, hidden)` and
/// the tab that takes over if `key` was active: its right neighbour, else
/// its left. Pure.
fn hide_project_tab(
    tabs: &[String],
    hidden: &[String],
    key: &str,
) -> (Vec<String>, Vec<String>, Option<String>) {
    let at = tabs.iter().position(|tab| tab == key);
    let order: Vec<String> = tabs.iter().filter(|tab| *tab != key).cloned().collect();
    let mut hidden: Vec<String> = hidden.iter().filter(|tab| *tab != key).cloned().collect();
    hidden.push(key.to_string());
    let neighbour = at.and_then(|at| order.get(at).or_else(|| order.last()).cloned());
    (order, hidden, neighbour)
}

/// The tab a jump slot opens: slots in order, except that the last slot
/// always opens the last tab (Chrome's ⌘9). Pure.
fn project_tab_for_slot(tabs: &[String], slot: usize) -> Option<&String> {
    if slot + 1 == JUMP_SLOTS {
        tabs.last()
    } else {
        tabs.get(slot)
    }
}

/// A tab's title: the space's display name, plus its parent folder when
/// another space shares that name (sibling clones such as `a/zeron` and
/// `b/zeron` must stay tellable apart). Pure.
fn project_tab_title(space: &zeron_proto::Space, spaces: &[zeron_proto::Space]) -> String {
    let name = space.display_name();
    let shared = spaces
        .iter()
        .any(|other| other.id != space.id && other.display_name().eq_ignore_ascii_case(name));
    if !shared {
        return name.to_string();
    }
    let parent = std::path::Path::new(space.path.trim_end_matches(['/', '\\']))
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|parent| parent.to_str());
    match parent {
        Some(parent) => format!("{name} · {parent}"),
        None => name.to_string(),
    }
}

/// The branch a `HEAD` file names: the short branch name, else (detached)
/// the abbreviated commit. Pure.
fn parse_git_head(head: &str) -> Option<String> {
    let head = head.trim();
    if let Some(reference) = head.strip_prefix("ref:") {
        let reference = reference.trim();
        let branch = reference.strip_prefix("refs/heads/").unwrap_or(reference);
        return (!branch.is_empty()).then(|| branch.to_string());
    }
    (head.len() >= 7 && head.chars().all(|c| c.is_ascii_hexdigit())).then(|| head[..7].to_string())
}

/// The git directory of the checkout holding `path`: `.git` itself, or the
/// directory a linked worktree's `.git` file points at.
fn git_dir(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let canonical = path.canonicalize().ok()?;
    let top = canonical
        .ancestors()
        .find(|dir| dir.join(".git").exists())?;
    let dot_git = top.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let contents = std::fs::read_to_string(&dot_git).ok()?;
    let target = contents
        .lines()
        .find_map(|line| line.strip_prefix("gitdir:"))?
        .trim();
    let target = std::path::Path::new(target);
    Some(if target.is_absolute() {
        target.to_path_buf()
    } else {
        top.join(target)
    })
}

/// The branch checked out in the folder at `path` (local folders only).
fn read_git_branch(path: &std::path::Path) -> Option<String> {
    parse_git_head(&std::fs::read_to_string(git_dir(path)?.join("HEAD")).ok()?)
}

fn reset_progress_message(completed: &[String], active: Option<&str>) -> String {
    let mut lines = vec!["Returning to the default branch".to_string()];
    lines.extend(completed.iter().map(|step| format!("✓ {step}")));
    if let Some(step) = active {
        lines.push(format!("• {step}…"));
    }
    lines.join("\n")
}

fn add_numstat_line(line: &[u8], additions: &mut u32, deletions: &mut u32) {
    let mut fields = line.splitn(3, |byte| *byte == b'\t');
    let adds = fields.next().and_then(|value| std::str::from_utf8(value).ok());
    let dels = fields.next().and_then(|value| std::str::from_utf8(value).ok());
    if let (Some(adds), Some(dels)) = (adds, dels) {
        *additions = (*additions).saturating_add(adds.parse().unwrap_or(0));
        *deletions = (*deletions).saturating_add(dels.parse().unwrap_or(0));
    }
}

/// The checkout's working-tree line totals against HEAD, including untracked
/// text files. Called from the background executor alongside the branch read.
fn read_git_change_totals(path: &std::path::Path) -> Option<(u32, u32)> {
    let output = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["diff", "--numstat", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let mut additions = 0u32;
    let mut deletions = 0u32;
    for line in output.stdout.split(|byte| *byte == b'\n') {
        add_numstat_line(line, &mut additions, &mut deletions);
    }

    // `git diff` omits untracked files. Count their text lines as additions.
    let untracked = Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["ls-files", "--others", "--exclude-standard", "-z"])
        .output()
        .ok()?;
    if !untracked.status.success() {
        return None;
    }
    for relative in untracked
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(relative).ok()?;
        let contents = std::fs::read(path.join(relative)).ok()?;
        if let Ok(contents) = std::str::from_utf8(&contents) {
            additions = additions.saturating_add(
                contents.lines().count().min(u32::MAX as usize) as u32,
            );
        }
    }
    Some((additions, deletions))
}

/// For a folder this device cannot read: the branch of its newest session
/// working in the folder itself (not a worktree). Pure.
fn latest_session_branch(chats: &[zeron_proto::Chat], space: &zeron_proto::Space) -> Option<String> {
    let root = space.path.trim_end_matches(['/', '\\']);
    chats
        .iter()
        .filter(|chat| !chat.archived && chat.space_id.as_deref() == Some(space.id.as_str()))
        .filter_map(|chat| {
            let source = chat.source_context.as_ref()?;
            (source.repo_root.trim_end_matches(['/', '\\']) == root).then(|| {
                (
                    chat.last_message_at.unwrap_or(chat.created_at),
                    source.branch.clone(),
                )
            })
        })
        .max_by_key(|(at, _)| *at)
        .map(|(_, branch)| branch)
        .filter(|branch| !branch.trim().is_empty())
}

/// Whether a tab has working sessions or an urgent finished/waiting state.
#[derive(Default)]
struct ProjectTabActivity {
    working: usize,
    attention: Option<zeron_proto::ChatIndicator>,
}

/// Urgency of an attention state, for picking a tab's dot.
fn attention_rank(status: zeron_proto::ChatIndicator) -> u8 {
    match status {
        zeron_proto::ChatIndicator::AwaitingInput => 3,
        zeron_proto::ChatIndicator::Errored => 2,
        zeron_proto::ChatIndicator::Completed => 1,
        _ => 0,
    }
}

/// One tab as drawn: key, project name, branch, PR summary, and local change totals.
struct ProjectTabRow {
    key: String,
    space: Option<zeron_proto::Space>,
    title: SharedString,
    detail: SharedString,
    branch: Option<SharedString>,
    pull_request: Option<zeron_proto::ChangeRequestSummary>,
    changes: Option<(u32, u32)>,
}

/// The project-tab bar's two layers: the strip of tabs, and the band painted
/// behind the titlebar with the active tab's chip.
pub(super) struct ProjectTabBar {
    pub strip: AnyElement,
    pub band: AnyElement,
}

impl Shell {
    fn project_tab_height(&self) -> f32 {
        if self.settings.project_tabs.compact {
            Theme::TITLEBAR_HEIGHT - Theme::TITLEBAR_TOP_PAD
        } else {
            52.0
        }
    }

    /// Reserve both rows and the top gutter so larger tabs never clip.
    pub(super) fn project_tab_header_height(&self) -> f32 {
        self.project_tab_height() + Theme::TITLEBAR_TOP_PAD
    }

    pub(super) fn set_project_tabs_compact(&mut self, compact: bool, cx: &mut Context<Self>) {
        self.settings.project_tabs.compact = compact;
        self.project_tab_drag = None;
        self.dismiss_project_tab_card(cx);
        self.schedule_save(cx);
        cx.notify();
    }

    fn hover_project_tab_card(&mut self, key: &str, hovered: bool, cx: &mut Context<Self>) {
        let now = Instant::now();
        if !hovered {
            if self.project_tab_hover_card.leave(key, now) {
                cx.notify();
            }
            return;
        }
        if cx.has_active_drag() || self.project_tab_menu.is_open() {
            return;
        }
        if self.project_tab_hover_card.enter(key, now) {
            cx.notify();
            return;
        }
        if self.project_tab_hover_card.shown() == Some(key) {
            return;
        }
        let key = key.to_owned();
        let task = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                cx.background_executor().timer(PROJECT_TAB_HOVER_DWELL).await;
                let _ = this.update(cx, |this, cx| {
                    if this.project_tab_hover_card.settle(&key) {
                        cx.notify();
                    }
                });
            }
        });
        self.project_tab_hover_card.pending = Some((key, task));
        cx.notify();
    }

    pub(super) fn dismiss_project_tab_card(&mut self, cx: &mut Context<Self>) {
        if self.project_tab_hover_card.dismiss() {
            cx.notify();
        }
    }

    /// Detail card for a titlebar project tab. The tab already carries the
    /// essentials at a glance; this is the quiet place for their names and
    /// the PR state that the small icon alone cannot explain.
    #[allow(clippy::too_many_arguments)]
    fn render_project_tab_hover_card(
        &self,
        key: &str,
        title: SharedString,
        detail: SharedString,
        branch: Option<SharedString>,
        pull_request: Option<zeron_proto::ChangeRequestSummary>,
        changes: Option<(u32, u32)>,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if self.project_tab_hover_card.shown() != Some(key) {
            return None;
        }

        let theme = Theme::of(cx).for_popup();
        let muted = theme.text_muted;
        let meta_row = |glyph: AnyElement, label: SharedString| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.0))
                .min_w_0()
                .h(px(18.0))
                .child(
                    div()
                        .w(px(14.0))
                        .flex_none()
                        .flex()
                        .justify_center()
                        .child(glyph),
                )
                .child(div().flex_1().min_w_0().truncate().child(label))
        };
        let quiet_glyph = |path: &'static str| {
            icon(path)
                .size(px(13.0))
                .text_color(muted.opacity(0.8))
                .into_any_element()
        };

        let header = div()
            .flex()
            .flex_col()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(crate::typography::ui_rems(13.0))
                    .line_height(px(18.0))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(theme.text)
                    .line_clamp(2)
                    .child(title),
            )
            .child(
                div()
                    .text_size(crate::typography::ui_rems(11.0))
                    .line_height(px(15.0))
                    .text_color(muted)
                    .line_clamp(2)
                    .child(detail),
            );

        let pr: AnyElement = pull_request
            .map(|summary| {
                let state = crate::change_requests::ChangeRequestTabState::from_summary(&summary);
                let color = state.color(&theme);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(meta_row(
                        icon(state.icon())
                            .size(px(13.0))
                            .text_color(color)
                            .into_any_element(),
                        format!("#{} · {}", summary.number, state.label(&summary)).into(),
                    ))
                    .child(
                        div()
                            .ml(px(21.0))
                            .min_w_0()
                            .truncate()
                            .text_size(crate::typography::ui_rems(11.0))
                            .line_height(px(15.0))
                            .text_color(muted)
                            .child(summary.title.replace(['\r', '\n'], " ")),
                    )
                    .into_any_element()
            })
            .unwrap_or_else(|| {
                meta_row(
                    quiet_glyph(icons::PULL_REQUEST),
                    "No pull request for this branch".into(),
                )
                .text_color(muted)
                .into_any_element()
            });
        let changes = changes.filter(|(adds, dels)| adds + dels > 0).map(|(adds, dels)| {
            meta_row(
                quiet_glyph(icons::FILE_TREE),
                format!("+{adds}  −{dels}").into(),
            )
            .font_family(theme.font_mono.clone())
            .text_color(muted)
        });
        let card = div()
            .w(px(PROJECT_TAB_HOVER_WIDTH))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .rounded(px(popover::CARD_RADIUS))
            .overflow_hidden()
            .border_1()
            .border_color(theme.border)
            .bg(popover::surface_bg(&theme))
            .when(!theme.is_frost(), |card| card.shadow_lg())
            .child(header)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .text_size(crate::typography::ui_rems(12.0))
                    .text_color(muted)
                    .children(branch.map(|branch| meta_row(quiet_glyph(icons::GIT_BRANCH), branch)))
                    .child(pr)
                    .children(changes),
            );
        let card = div()
            .debug_selector({
                let key = key.to_owned();
                move || format!("project-tab-hover-card-{key}")
            })
            .child(crate::frost::frosted(
                popover::CARD_RADIUS,
                crate::frost::MENU_BLUR,
                card,
            ));
        let card = if self.reduced_motion {
            card.into_any_element()
        } else {
            card.with_animation(
                SharedString::from(format!("project-tab-hover-card-{key}")),
                motion::MENU_IN.animation(),
                |el, t| el.relative().opacity(t).top(px(-4.0 * (1.0 - t))),
            )
            .into_any_element()
        };
        Some(
            div()
                .absolute()
                .top_full()
                .left_0()
                .child(
                    gpui::deferred(
                        gpui::anchored()
                            .anchor(gpui::Anchor::TopLeft)
                            .snap_to_window_with_margin(px(8.0))
                            .child(div().mt(px(6.0)).child(card)),
                    )
                    .with_priority(1),
                )
                .into_any_element(),
        )
    }

    /// The tab key a session lives under.
    pub(super) fn chat_project_tab(chat: &zeron_proto::Chat) -> String {
        chat.space_id
            .clone()
            .unwrap_or_else(|| NO_PROJECT_TAB.to_string())
    }

    /// Every key a tab can name: spaces in display order, then "No project".
    fn project_tab_known(&self, cx: &App) -> Vec<String> {
        let state = self.state.read(cx);
        let mut keys: Vec<String> = state
            .spaces_sorted()
            .into_iter()
            .map(|space| space.id.clone())
            .collect();
        keys.push(NO_PROJECT_TAB.to_string());
        keys
    }

    /// The visible tabs, in strip order.
    pub(super) fn project_tab_keys(&self, cx: &App) -> Vec<String> {
        let tabs = &self.settings.project_tabs;
        visible_project_tabs(&tabs.order, &tabs.hidden, &self.project_tab_known(cx))
    }

    /// Un-hide `key` (it rejoins the end of the strip). Whether it changed.
    fn reveal_project_tab(&mut self, key: &str) -> bool {
        let hidden = &mut self.settings.project_tabs.hidden;
        let before = hidden.len();
        hidden.retain(|tab| tab != key);
        before != hidden.len()
    }

    /// The session a tab opens on: the one last viewed there while it is
    /// still listed, else the tab's most recently active session.
    pub(super) fn project_tab_landing_in(
        &self,
        state: &AppState,
        key: Option<&str>,
    ) -> Option<String> {
        let chats = state.sidebar_chats(Utc::now(), key);
        key.and_then(|key| self.settings.project_tabs.last_chat.get(key))
            .filter(|id| chats.iter().any(|(_, chat)| &chat.id == *id))
            .cloned()
            .or_else(|| chats.first().map(|(_, chat)| chat.id.clone()))
    }

    /// Turn project tabs on or off (View → Show → Project tabs).
    pub(super) fn set_project_tabs_enabled(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.settings.project_tabs.enabled == enabled {
            return;
        }
        self.settings.project_tabs.enabled = enabled;
        self.state
            .update(cx, |state, _| state.exact_space_filter = enabled);
        self.close_project_tabs_menu(cx);
        if enabled {
            let tabs = &self.settings.project_tabs;
            if tabs.order.is_empty() && tabs.hidden.is_empty() {
                let known = self.project_tab_known(cx);
                let busy: std::collections::HashSet<String> = self
                    .state
                    .read(cx)
                    .overview_chats(Utc::now())
                    .into_iter()
                    .map(|(_, chat)| Self::chat_project_tab(chat))
                    .collect();
                let (order, hidden) = seed_project_tabs(&known, &busy);
                self.settings.project_tabs.order = order;
                self.settings.project_tabs.hidden = hidden;
            }
            // Tabs replace grouping the sidebar by project.
            if self.settings.sidebar_organization == SidebarOrganization::ByProject {
                self.settings.sidebar_organization = SidebarOrganization::InOneList;
            }
            let selected = self
                .state
                .read(cx)
                .selected_chat_row()
                .map(Self::chat_project_tab);
            let tabs = self.project_tab_keys(cx);
            let target = selected.or_else(|| {
                self.settings
                    .space_filter
                    .clone()
                    .filter(|filter| tabs.contains(filter))
                    .or_else(|| tabs.first().cloned())
            });
            if let Some(key) = target {
                self.reveal_project_tab(&key);
                self.set_space_filter(Some(key), cx);
            }
        } else if self.settings.space_filter.as_deref() == Some(NO_PROJECT_TAB) {
            self.set_space_filter(None, cx);
        }
        self.sync_change_request_visibility(cx);
        self.project_tab_branch_checked = None;
        self.schedule_save(cx);
        cx.notify();
    }

    /// Switch to a tab (revealing it if hidden) and land on its session.
    pub(super) fn activate_project_tab(&mut self, key: String, cx: &mut Context<Self>) {
        self.close_project_tabs_menu(cx);
        if self.reveal_project_tab(&key) {
            self.schedule_save(cx);
        }
        if self.settings.space_filter.as_deref() == Some(key.as_str()) {
            cx.notify();
            return;
        }
        self.set_space_filter(Some(key.clone()), cx);
        let landing = self.project_tab_landing_in(self.state.read(cx), Some(key.as_str()));
        match landing {
            Some(chat_id) => self.open_chat(chat_id, cx),
            // `open_new_session` homes the canvas on the filter's project
            // (or project-less under "No project").
            None => self.open_new_session(None, cx),
        }
    }

    /// Close (hide) a tab. The active tab hands over to its neighbour; with
    /// no tab left the sidebar lists everything.
    fn close_project_tab(&mut self, key: String, cx: &mut Context<Self>) {
        let tabs = self.project_tab_keys(cx);
        let (order, hidden, neighbour) =
            hide_project_tab(&tabs, &self.settings.project_tabs.hidden, &key);
        self.settings.project_tabs.order = order;
        self.settings.project_tabs.hidden = hidden;
        if self.settings.space_filter.as_deref() == Some(key.as_str()) {
            match neighbour {
                Some(next) => self.activate_project_tab(next, cx),
                None => {
                    self.set_space_filter(None, cx);
                    self.open_new_session(None, cx);
                }
            }
        }
        self.schedule_save(cx);
        cx.notify();
    }

    /// ⌘1…⌘9 while tabs are on.
    pub(super) fn jump_to_project_tab(&mut self, slot: usize, cx: &mut Context<Self>) {
        let tabs = self.project_tab_keys(cx);
        if let Some(key) = project_tab_for_slot(&tabs, slot).cloned() {
            self.activate_project_tab(key, cx);
        }
    }

    /// Next/Previous project tab, wrapping at both ends.
    pub(super) fn cycle_project_tab(&mut self, forward: bool, cx: &mut Context<Self>) {
        if !self.settings.project_tabs.enabled {
            return;
        }
        let tabs = self.project_tab_keys(cx);
        if let Some(key) =
            super::tabs::cycle_target(&tabs, self.settings.space_filter.as_deref(), forward)
        {
            self.activate_project_tab(key, cx);
        }
    }

    /// The project-less canvas lives under "No project".
    pub(super) fn show_no_project_tab(&mut self, cx: &mut Context<Self>) {
        if self.settings.space_filter.as_deref() == Some(NO_PROJECT_TAB) {
            return;
        }
        self.cancel_pinned_session_drag(cx);
        self.reveal_project_tab(NO_PROJECT_TAB);
        // Not `set_space_filter`: re-selecting "no project" there would reset
        // the canvas's device pick.
        self.settings.space_filter = Some(NO_PROJECT_TAB.to_string());
        self.schedule_save(cx);
        cx.notify();
    }

    /// Keep the active tab on the open session's project (revealing a hidden
    /// tab when a session opens there), remember each tab's last session,
    /// and keep some tab active once spaces have synced. Runs on every state
    /// frame; writes only on change.
    pub(super) fn sync_project_tab(&mut self, cx: &mut Context<Self>) {
        if !self.settings.project_tabs.enabled {
            return;
        }
        if !self.state.read(cx).exact_space_filter {
            self.state
                .update(cx, |state, _| state.exact_space_filter = true);
        }
        let (selected, spaces_synced) = {
            let state = self.state.read(cx);
            (
                state
                    .selected_chat_row()
                    .filter(|chat| !chat.archived)
                    .map(|chat| (chat.id.clone(), Self::chat_project_tab(chat))),
                state.spaces_synced,
            )
        };
        match selected {
            Some((chat_id, key)) => {
                if self.settings.space_filter.as_deref() != Some(key.as_str()) {
                    self.reveal_project_tab(&key);
                    self.set_space_filter(Some(key.clone()), cx);
                }
                if self.settings.project_tabs.last_chat.get(&key) != Some(&chat_id) {
                    self.settings.project_tabs.last_chat.insert(key, chat_id);
                    self.schedule_save(cx);
                }
            }
            None if spaces_synced => {
                let tabs = self.project_tab_keys(cx);
                let valid = self
                    .settings
                    .space_filter
                    .as_ref()
                    .is_some_and(|filter| tabs.contains(filter));
                if !valid && let Some(first) = tabs.first().cloned() {
                    self.set_space_filter(Some(first), cx);
                }
            }
            None => {}
        }
    }

    fn reorder_project_tabs(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let mut tabs = self.project_tab_keys(cx);
        if from == to || from >= tabs.len() || to >= tabs.len() {
            return;
        }
        let key = tabs.remove(from);
        tabs.insert(to, key);
        self.settings.project_tabs.order = tabs;
        self.schedule_save(cx);
        cx.notify();
    }

    fn update_project_tab_drag_over(&mut self, from: usize, over: usize, cx: &mut Context<Self>) {
        match &mut self.project_tab_drag {
            Some(drag) if drag.over != over => {
                drag.prev_over = drag.over;
                drag.over = over;
                drag.epoch += 1;
                cx.notify();
            }
            Some(_) => {}
            None => {
                self.project_tab_drag = Some(RightTabDragState {
                    from,
                    over,
                    epoch: 0,
                    prev_over: from,
                });
                cx.notify();
            }
        }
    }

    fn finish_project_tab_drag(&mut self, from: usize, cx: &mut Context<Self>) {
        let to = self.project_tab_drag.take().map_or(from, |drag| drag.over);
        self.reorder_project_tabs(from, to, cx);
        cx.notify();
    }

    fn open_project_tab_menu(
        &mut self,
        key: String,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        self.close_project_tabs_menu(cx);
        self.project_tab_menu.open((key, position));
        cx.notify();
    }

    pub(super) fn close_project_tab_menu(&mut self, cx: &mut Context<Self>) {
        if self.project_tab_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.project_tab_menu);
            cx.notify();
        }
    }

    /// Return this space's checkout to the repository's default branch. The
    /// engine resolves the remote default (rather than assuming `main`),
    /// stashes uncommitted and untracked files, then fetches and rebases.
    fn reset_project_tab_to_default_branch(&mut self, key: String, cx: &mut Context<Self>) {
        if self.project_tab_reset_task.is_some() {
            return;
        }
        let (space, engine, target) = {
            let state = self.state.read(cx);
            let Some(space) = state.space_row(&key).cloned() else {
                self.show_notice(
                    crate::toast::ToastKind::Error,
                    "This tab has no workspace",
                    cx,
                );
                return;
            };
            let Some(engine) = state.engine().cloned() else {
                self.show_notice(crate::toast::ToastKind::Error, "Engine not connected", cx);
                return;
            };
            let target = (state.local_device_id.as_deref() != Some(space.device_id.as_str()))
                .then(|| space.device_id.clone());
            (space, engine, target)
        };
        let mut params = serde_json::Map::new();
        params.insert("repoPath".into(), serde_json::Value::String(space.path));
        if let Some(target) = target {
            params.insert("targetDeviceId".into(), serde_json::Value::String(target));
        }
        let toast = crate::toast::pending(
            cx,
            reset_progress_message(&[], Some("Starting return to the default branch")),
        );
        self.project_tab_reset_task = Some(cx.spawn(async move |this, cx| {
            let result = engine
                .client()
                .subscribe(methods::RESET_REPO_TO_BASE, serde_json::Value::Object(params))
                .await;
            let Ok(mut progress) = result else {
                let error = result.err().expect("failed reset subscription");
                this.update(cx, |this, cx| {
                    this.project_tab_reset_task = None;
                    crate::toast::finish(
                        cx,
                        toast,
                        crate::toast::ToastKind::Error,
                        format!("Could not start return to the default branch: {error}"),
                    );
                })
                .ok();
                return;
            };
            let mut completed = Vec::<String>::new();
            let mut active: Option<String> = None;
            let mut finished = false;
            while let Some(event) = progress.recv().await {
                let kind = event.get("kind").and_then(serde_json::Value::as_str);
                match kind {
                    Some("progress") => {
                        let message = event
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Working")
                            .to_string();
                        if event
                            .get("complete")
                            .and_then(serde_json::Value::as_bool)
                            .unwrap_or(false)
                        {
                            completed.push(message);
                            active = None;
                        } else {
                            active = Some(message);
                        }
                        let message = reset_progress_message(&completed, active.as_deref());
                        this.update(cx, |_, cx| crate::toast::update(cx, toast, message))
                            .ok();
                    }
                    Some("complete") => {
                        let branch = event
                            .get("branch")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("default branch");
                        completed.push(format!("Returned to {branch}"));
                        let message = reset_progress_message(&completed, None);
                        this.update(cx, |this, cx| {
                            this.project_tab_reset_task = None;
                            this.project_tab_branch_checked = None;
                            crate::toast::finish(cx, toast, crate::toast::ToastKind::Success, message);
                            cx.notify();
                        })
                        .ok();
                        finished = true;
                        break;
                    }
                    Some("error") => {
                        let error = event
                            .get("message")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("Unknown error");
                        let failed = active.as_deref().unwrap_or("Returning to the default branch");
                        completed.push(format!("✕ {failed}: {error}"));
                        let message = completed.join("\n");
                        this.update(cx, |this, cx| {
                            this.project_tab_reset_task = None;
                            crate::toast::finish(cx, toast, crate::toast::ToastKind::Error, message);
                            cx.notify();
                        })
                        .ok();
                        finished = true;
                        break;
                    }
                    _ => {}
                }
            }
            if !finished {
                this.update(cx, |this, cx| {
                    this.project_tab_reset_task = None;
                    crate::toast::finish(
                        cx,
                        toast,
                        crate::toast::ToastKind::Error,
                        "The return-to-default-branch progress stream ended unexpectedly",
                    );
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// The tab's right-click menu, appended to the shell's overlay list.
    pub(super) fn render_project_tab_overlays(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let Some((key, position)) = self.project_tab_menu.get().cloned() else {
            return Vec::new();
        };
        let theme = Theme::of(cx).for_popup();
        let closing = self.project_tab_menu.closing_since();
        let path = self
            .state
            .read(cx)
            .space_row(&key)
            .map(|space| space.path.clone());
        let menu = popover::popover_card(&theme)
            .w(px(250.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_project_tab_menu(cx)))
            .flex()
            .flex_col()
            .when_some(path, |menu, path| {
                menu.child(
                    popover::menu_row(&theme, false, "project-tab-menu-copy-path")
                        .id("project-tab-menu-copy-path")
                        .debug_selector(|| "project-tab-menu-copy-path".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_project_tab_menu(cx);
                            cx.write_to_clipboard(ClipboardItem::new_string(path.clone()));
                        }))
                        .child(icon(icons::COPY).size(px(14.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Copy path")),
                )
            })
            .when(key != NO_PROJECT_TAB, |menu| {
                let reset_key = key.clone();
                menu.child(
                    popover::menu_row(&theme, false, "project-tab-menu-reset-default")
                        .id("project-tab-menu-reset-default")
                        .debug_selector(|| "project-tab-menu-reset-default".into())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.close_project_tab_menu(cx);
                            this.reset_project_tab_to_default_branch(reset_key.clone(), cx);
                        }))
                        .child(icon(icons::GIT_BRANCH).size(px(14.0)).text_color(theme.text_muted))
                        .child(SharedString::from("Return to default branch")),
                )
            })
            .child(
                popover::menu_row(&theme, false, "project-tab-menu-close")
                    .id("project-tab-menu-close")
                    .debug_selector(|| "project-tab-menu-close".into())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_project_tab_menu(cx);
                        this.close_project_tab(key.clone(), cx);
                    }))
                    .child(icon(icons::CLOSE).size(px(14.0)).text_color(theme.text_muted))
                    .child(SharedString::from("Close tab")),
            )
            .into_any_element();
        vec![popover::menu_at("project-tab-context-menu", position, menu, closing)]
    }

    pub(super) fn close_project_tabs_menu(&mut self, cx: &mut Context<Self>) {
        if self.project_tabs_menu.begin_close() {
            popover::reap_popup(cx, |shell: &mut Self| &mut shell.project_tabs_menu);
        }
        cx.notify();
    }

    fn project_tab_rows(&self, keys: &[String], cx: &App) -> Vec<ProjectTabRow> {
        let state = self.state.read(cx);
        let meta = self.settings.project_tabs.meta;
        let now = Utc::now();
        keys.iter()
            .map(|key| match state.space_row(key) {
                Some(space) => {
                    let (device_tag, _) = state.space_device_tag(space, now);
                    ProjectTabRow {
                        key: key.clone(),
                        space: Some(space.clone()),
                        title: project_tab_title(space, &state.spaces).into(),
                        detail: format!("{} {device_tag}", space.path).into(),
                        branch: meta
                            .branch
                            .then(|| state.space_branches.get(&space.id))
                            .flatten()
                            .map(|branch| branch.clone().into()),
                        pull_request: meta
                            .pull_request
                            .then(|| state.change_request_for_space(space).cloned())
                            .flatten(),
                        changes: self.project_tab_changes.get(&space.id).copied(),
                    }
                }
                None => ProjectTabRow {
                    key: key.clone(),
                    space: None,
                    title: "No project".into(),
                    detail: "Sessions outside any project".into(),
                    branch: None,
                    pull_request: None,
                    changes: None,
                },
            })
            .collect()
    }

    /// Per-tab session activity, from the same rows the sidebar lists.
    fn project_tab_activity(&self, cx: &App) -> HashMap<String, ProjectTabActivity> {
        let mut activity: HashMap<String, ProjectTabActivity> = HashMap::new();
        for (status, chat) in self.state.read(cx).overview_chats(Utc::now()) {
            let tab = activity.entry(Self::chat_project_tab(chat)).or_default();
            if status == zeron_proto::ChatIndicator::Working {
                tab.working += 1;
            } else if attention_rank(status) > tab.attention.map_or(0, attention_rank) {
                tab.attention = Some(status);
            }
        }
        activity
    }

    /// Whether pull request watches should run: the sidebar shows PRs, or
    /// project tabs do.
    pub(super) fn change_requests_wanted(&self) -> bool {
        self.settings.sidebar_show_pull_request
            || (self.settings.project_tabs.enabled && self.settings.project_tabs.meta.pull_request)
    }

    pub(super) fn sync_change_request_visibility(&self, cx: &mut Context<Self>) {
        let visible = self.change_requests_wanted();
        self.state
            .update(cx, |state, cx| state.set_change_requests_visible(visible, cx));
    }

    /// Re-read visible tabs' checked-out branches and local changes at most
    /// every [`PROJECT_TAB_BRANCH_POLL`]: filesystem work runs off the render
    /// thread, and remote branches come from the newest session in that space.
    fn refresh_project_tab_branches(&mut self, keys: &[String], cx: &mut Context<Self>) {
        if self.project_tab_branch_task.is_some()
            || self
                .project_tab_branch_checked
                .is_some_and(|checked| checked.elapsed() < PROJECT_TAB_BRANCH_POLL)
        {
            return;
        }
        self.project_tab_branch_checked = Some(std::time::Instant::now());
        let (local, mut branches) = {
            let state = self.state.read(cx);
            let mut local = Vec::new();
            let mut branches = HashMap::new();
            for space in keys.iter().filter_map(|key| state.space_row(key)) {
                if state.local_device_id.as_deref() == Some(space.device_id.as_str()) {
                    local.push((space.id.clone(), space.path.clone()));
                } else if let Some(branch) = latest_session_branch(&state.chats, space) {
                    branches.insert(space.id.clone(), branch);
                }
            }
            (local, branches)
        };
        self.project_tab_branch_task = Some(cx.spawn(async move |this, cx| {
            let local_ids = local.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>();
            let (read, changes) = cx
                .background_executor()
                .spawn(async move {
                    let mut branches = Vec::new();
                    let mut changes = HashMap::new();
                    for (id, path) in local {
                        let path = std::path::Path::new(&path);
                        if let Some(branch) = read_git_branch(path) {
                            branches.push((id.clone(), branch));
                        }
                        if let Some(totals) = read_git_change_totals(path) {
                            changes.insert(id, totals);
                        }
                    }
                    (branches, changes)
                })
                .await;
            branches.extend(read);
            this.update(cx, |this, cx| {
                this.project_tab_branch_task = None;
                this.project_tab_changes
                    .retain(|space, _| !local_ids.contains(space));
                this.project_tab_changes.extend(changes);
                this.state
                    .update(cx, |state, cx| state.set_space_branches(branches, cx));
            })
            .ok();
        }));
    }

    /// Keep checkout diff snapshots for remote devices represented by visible
    /// project tabs, so their third-row totals reflect the host workspace too.
    fn refresh_project_tab_remote_diff_watches(&mut self, keys: &[String], cx: &mut Context<Self>) {
        let (engine, devices) = {
            let state = self.state.read(cx);
            let devices = keys
                .iter()
                .filter_map(|key| state.space_row(key))
                .filter(|space| state.local_device_id.as_deref() != Some(space.device_id.as_str()))
                .map(|space| space.device_id.clone())
                .collect::<std::collections::HashSet<_>>();
            (state.engine().cloned(), devices)
        };
        self.project_tab_remote_diff_tasks
            .retain(|device, _| devices.contains(device));
        self.project_tab_remote_diffs
            .retain(|device, _| devices.contains(device));
        let Some(engine) = engine else {
            return;
        };

        for device in devices {
            if self.project_tab_remote_diff_tasks.contains_key(&device) {
                continue;
            }
            let engine = engine.clone();
            let target = device.clone();
            let task = cx.spawn(async move |this, cx| loop {
                let params = serde_json::json!({ "targetDeviceId": target.clone() });
                if let Ok(mut rx) = engine
                    .client()
                    .subscribe(methods::WATCH_CHECKOUT_DIFFS, params)
                    .await
                {
                    while let Some(value) = rx.recv().await {
                        let target = target.clone();
                        let alive = this.update(cx, |this, cx| {
                            let changed = crate::changes::apply_diff_frame(
                                this.project_tab_remote_diffs.entry(target.clone()).or_default(),
                                value,
                            );
                            if changed {
                                this.update_project_tab_remote_change_totals(&target, cx);
                            }
                        });
                        if alive.is_err() {
                            return;
                        }
                    }
                }
                cx.background_executor().timer(Duration::from_secs(2)).await;
            });
            self.project_tab_remote_diff_tasks
                .insert(device, task);
        }
    }

    fn update_project_tab_remote_change_totals(&mut self, device: &str, cx: &mut Context<Self>) {
        let diffs = self
            .project_tab_remote_diffs
            .get(device)
            .cloned()
            .unwrap_or_default();
        let updates = {
            let state = self.state.read(cx);
            state
                .spaces
                .iter()
                .filter(|space| space.device_id == device)
                .map(|space| {
                    let totals = state
                        .chats
                        .iter()
                        .filter(|chat| {
                            !chat.archived
                                && chat.device_id == device
                                && chat.space_id.as_deref() == Some(space.id.as_str())
                        })
                        .filter_map(|chat| {
                            let diff = crate::changes::resolve_diff(&diffs, chat)?;
                            Some((
                                chat.last_message_at.unwrap_or(chat.created_at),
                                (diff.additions, diff.deletions),
                            ))
                        })
                        .max_by_key(|(at, _)| *at)
                        .map(|(_, totals)| totals);
                    (space.id.clone(), totals)
                })
                .collect::<Vec<_>>()
        };
        let mut changed = false;
        for (space, totals) in updates {
            match totals {
                Some(totals) => {
                    changed |= self.project_tab_changes.insert(space, totals) != Some(totals);
                }
                None => changed |= self.project_tab_changes.remove(&space).is_some(),
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// The tab strip (plus, at its end, the all-projects menu button) and the
    /// band behind it: a tinted strip with a hairline along its bottom edge and
    /// the active tab's chip.
    pub(super) fn render_project_tab_bar(&mut self, cx: &mut Context<Self>) -> ProjectTabBar {
        let theme = Theme::of(cx).clone();
        // Heal drag state if the pointer was released outside the strip.
        if self.project_tab_drag.is_some() && !cx.has_active_drag() {
            self.project_tab_drag = None;
        }
        let keys = self.project_tab_keys(cx);
        self.refresh_project_tab_branches(&keys, cx);
        self.refresh_project_tab_remote_diff_watches(&keys, cx);
        let rows = self.project_tab_rows(&keys, cx);
        let activity = self.project_tab_activity(cx);
        let count = rows.len();
        let tab_width = self.settings.project_tabs.width.pixels();
        let active = self.settings.space_filter.clone();
        let active_ix = rows
            .iter()
            .position(|row| active.as_deref() == Some(row.key.as_str()));
        let drag = self
            .project_tab_drag
            .as_ref()
            .map(|d| (d.from, d.over, d.epoch, d.prev_over));
        // Where the active tab sits right now, following a drag in progress.
        let active_slot = active_ix.map(|ix| match drag {
            Some((from, over, ..)) if from == ix => over,
            Some((from, over, ..)) => {
                (ix as f32 + crate::terminal::panel::slide_offset(ix, from, over)) as usize
            }
            None => ix,
        });
        let profile = self.active_sidebar_pin_profile_key(cx);

        let scroll_for_drag = self.project_tab_scroll.clone();
        let mut strip = div()
            .id("project-tab-strip")
            .h_full()
            .flex()
            .flex_row()
            .items_end()
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.project_tab_scroll)
            // See the right pane's strip: Windows caption hit-testing would
            // otherwise claim tab clicks.
            .when(cfg!(target_os = "windows"), |strip| strip.occlude())
            .on_drag_move::<ProjectTabDrag>(cx.listener(
                move |this, event: &gpui::DragMoveEvent<ProjectTabDrag>, _, cx| {
                    let from = event.drag(cx).from;
                    let rel_x = f32::from(event.event.position.x)
                        - f32::from(event.bounds.left())
                        - f32::from(scroll_for_drag.offset().x);
                    let over = crate::terminal::panel::drop_index(rel_x, tab_width, count);
                    this.update_project_tab_drag_over(from, over, cx);
                },
            ))
            .on_drop::<ProjectTabDrag>(cx.listener(|this, payload: &ProjectTabDrag, _, cx| {
                this.finish_project_tab_drag(payload.from, cx);
            }));

        for (ix, row) in rows.into_iter().enumerate() {
            let is_active = active_ix == Some(ix);
            let tab_activity = activity.get(&row.key);
            // Separators hide where they touch the active tab's chip.
            let separator = ix + 1 < count && !is_active && active_ix != Some(ix + 1);
            let leading = self.render_project_tab_icon(
                &row,
                tab_activity,
                is_active,
                profile.clone(),
                &theme,
                cx,
            );
            let tab = self.render_project_tab(ix, row, leading, is_active, separator, &theme, cx);

            // Siblings slide aside while one drags; the dragged tab leaves a
            // spacer (the ghost carries it).
            let wrapped: AnyElement = match drag {
                Some((from, over, epoch, prev_over)) if ix != from => {
                    let target = crate::terminal::panel::slide_offset(ix, from, over) * tab_width;
                    let start =
                        crate::terminal::panel::slide_offset(ix, from, prev_over) * tab_width;
                    div()
                        .relative()
                        .child(tab.with_animation(
                            ("project-tab-slide", (ix as u64) | ((epoch as u64) << 32)),
                            TAB_SLIDE.animation(),
                            move |el, t| el.left(px(motion::lerp(start, target, t))),
                        ))
                        .into_any_element()
                }
                Some((from, ..)) if ix == from => div()
                    .w(px(tab_width))
                    .h(px(self.project_tab_height()))
                    .flex_none()
                    .into_any_element(),
                _ => tab.into_any_element(),
            };
            strip = strip.child(wrapped);
        }

        let menu_fade = "project-tabs-menu-fade";
        let mut menu_button = div()
            .id("project-tabs-menu")
            .debug_selector(|| "project-tabs-menu".into())
            .size(px(24.0))
            .flex_none()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.0))
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label("All projects")
            .bg(if self.project_tabs_menu.is_open() {
                crate::theme::wash(0.11)
            } else {
                motion::hover_blend(menu_fade, crate::theme::wash(0.0), crate::theme::wash(0.11))
            })
            .on_hover(motion::hover_listener(menu_fade))
            .block_mouse_except_scroll()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, _| {
                    window.prevent_default();
                    this.project_tabs_menu.note_trigger_press();
                }),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                cx.stop_propagation();
                if this.project_tabs_menu.take_press_was_open() {
                    this.close_project_tabs_menu(cx);
                } else {
                    this.project_tabs_menu.open(());
                    cx.notify();
                }
            }))
            .tooltip(crate::settings::widgets::text_tooltip("All projects"))
            .child(
                icon(icons::ALT_ARROW_DOWN)
                    .size(px(13.0))
                    .text_color(theme.text_muted),
            );
        if self.project_tabs_menu.get().is_some() {
            let closing = self.project_tabs_menu.closing_since();
            let menu = self.render_project_tabs_menu(&theme, &keys, cx);
            menu_button = menu_button.relative().child(popover::anchored_menu_below_end(
                "project-tabs-menu-card",
                menu,
                closing,
            ));
        }

        // The strip's on-screen bounds, measured at prepaint for the band's
        // chip (painted later in the same frame).
        let region = std::rc::Rc::new(std::cell::Cell::new(None::<gpui::Bounds<Pixels>>));
        let measure = region.clone();
        let strip = div()
            .id("project-tabs")
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(6.0))
            // A wheel over the tabs scrolls the strip, never the transcript
            // under the titlebar.
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .relative()
                    .overflow_hidden()
                    .child(
                        gpui::canvas(
                            move |bounds, _, _| measure.set(Some(bounds)),
                            |_, _, _, _| {},
                        )
                        .absolute()
                        .size_full(),
                    )
                    .child(crate::surface_chrome::tab_strip_region(
                        strip,
                        &self.project_tab_scroll,
                        &theme,
                    )),
            )
            .child(menu_button)
            .into_any_element();

        let scroll = self.project_tab_scroll.clone();
        // Blur quiets image detail; a theme-colored tint lifts dark wallpaper
        // in light mode instead of darkening it behind dark labels.
        let on_canvas = self.state.read(cx).selected_chat_row().is_none();
        let dark = theme.appearance.is_dark();
        let tint = theme.surface.alpha(if dark { 0.55 } else { 0.52 });
        let frost = theme.is_frost();
        // The band runs unbroken, in a chat as on the canvas, and the active
        // tab is a closed chip within it, in the hover chip's shape.
        let chip = ProjectTabChip {
            fill: theme.wash(if dark { 0.1 } else { 0.08 }),
            border: theme.wash(if dark { 0.14 } else { 0.1 }),
        };
        // On the canvas the band's own tint fades out below it.
        let shade = on_canvas.then(|| tint.opacity(0.5));
        let line = theme.border;
        let band = gpui::canvas(
            |_, _, _| {},
            move |bounds, _, window, _| {
                let chip_span = active_slot.and_then(|slot| {
                    let region = region.get()?;
                    let (x0, x1) =
                        project_tab_chip_span(region, slot, scroll.offset().x, tab_width)?;
                    // The span follows the scrolled tab and can run off the
                    // strip. Clip to the strip or the selection paints over
                    // the sidebar (the tab text is already clipped).
                    Some((project_tab_chip_mask(region, bounds), x0, x1))
                });
                window.paint_layer(bounds, |window| {
                    if frost {
                        window.paint_backdrop_blur(bounds, gpui::Corners::all(px(0.0)), px(24.0));
                    }
                    if let Some(shade) = shade {
                        paint_project_tab_shade(bounds, shade, window);
                    }
                    paint_project_tab_band(bounds, tint, line, window);
                    if let Some((mask, x0, x1)) = chip_span {
                        window.with_content_mask(
                            Some(gpui::ContentMask { bounds: mask }),
                            |window| paint_project_tab_chip(bounds, x0, x1, chip, window),
                        );
                    }
                });
            },
        )
        .size_full()
        .into_any_element();
        ProjectTabBar { strip, band }
    }

    /// A tab's leading glyph: the spinner while any of its sessions works,
    /// else the folder's own icon (home for "No project"), marked with a dot
    /// while a session waits on you.
    fn render_project_tab_icon(
        &self,
        row: &ProjectTabRow,
        activity: Option<&ProjectTabActivity>,
        active: bool,
        profile: Option<String>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if activity.is_some_and(|activity| activity.working > 0) {
            return loaders::mini_glyph_spinner(
                format!("project-tab-spinner-{}", row.key),
                2.0,
                theme.glyph,
                cx.entity_id(),
                cx,
            )
            .into_any_element();
        }
        let art = match &row.space {
            Some(space) => self
                .project_icons
                .render_exact(&self.state, space, profile, active, cx),
            None => icon(icons::HOME)
                .size(px(14.0))
                .text_color(theme.text_muted)
                .into_any_element(),
        };
        let dot = activity.and_then(|activity| activity.attention).map(|status| match status {
            zeron_proto::ChatIndicator::AwaitingInput => theme.warning,
            zeron_proto::ChatIndicator::Errored => theme.danger,
            _ => theme.accent,
        });
        div()
            .size(px(16.0))
            .relative()
            .flex()
            .items_center()
            .justify_center()
            .child(art)
            .when_some(dot, |el, color| {
                el.child(
                    div()
                        .absolute()
                        .top(px(-2.0))
                        .right(px(-2.0))
                        .size(px(7.0))
                        .rounded_full()
                        .border_1()
                        .border_color(theme.surface)
                        .bg(color),
                )
            })
            .into_any_element()
    }

    /// Compact tabs put metadata beside the name; larger tabs put it below
    /// and show working-tree totals on the active tab.
    /// Inactive tabs wash on hover; the band paints the active tab's chip.
    #[allow(clippy::too_many_arguments)]
    fn render_project_tab(
        &self,
        ix: usize,
        row: ProjectTabRow,
        leading: AnyElement,
        active: bool,
        separator: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let ProjectTabRow {
            key,
            title,
            detail,
            branch,
            pull_request,
            changes,
            ..
        } = row;
        let group: SharedString = format!("project-tab-{ix}").into();
        let muted = if theme.appearance.is_light() {
            theme.text.opacity(0.85)
        } else {
            theme.text_muted
        };
        let status = pull_request
            .as_ref()
            .map(|summary| crate::change_requests::ChangeRequestTabState::from_summary(summary));
        let status_color = status.map(|status| status.color(theme));
        let compact = self.settings.project_tabs.compact;
        let branch_label = branch.clone().map(|branch| {
            div()
                .debug_selector(move || format!("project-tab-branch-{ix}"))
                .when(compact, |el| el.max_w(px(60.0)))
                .when(!compact, |el| el.flex_1())
                .min_w_0()
                .truncate()
                .text_size(crate::typography::ui_rems(10.5))
                .line_height(px(14.0))
                .text_color(if active || theme.appearance.is_light() {
                    muted
                } else {
                    muted.opacity(0.78)
                })
                .child(branch)
        });
        let pr_icon = status.zip(status_color).map(|(status, color)| {
            div()
                .debug_selector(move || format!("project-tab-pr-state-{ix}"))
                .flex_none()
                .flex()
                .items_center()
                .child(icon(status.icon()).size(px(12.0)).text_color(color))
        });
        let title_label = div()
            .debug_selector(move || format!("project-tab-title-{ix}"))
            .flex_1()
            .min_w_0()
            .truncate()
            .text_size(crate::typography::ui_rems(12.0))
            .line_height(px(14.0))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(theme.text)
            .child(title.clone());
        let content: AnyElement = if compact {
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .items_center()
                .gap(px(8.0))
                .child(div().flex_none().child(leading))
                .child(title_label)
                .children(branch_label)
                .children(pr_icon)
                .into_any_element()
        } else {
            let change_totals = changes
                .filter(|&(adds, dels)| active && (adds > 0 || dels > 0))
                .map(|(adds, dels)| {
                    div()
                        .debug_selector(move || format!("project-tab-changes-{ix}"))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .font_family(theme.font_mono.clone())
                        .text_size(crate::typography::ui_rems(10.0))
                        .line_height(px(12.0))
                        .child(
                            div()
                                .text_color(crate::changes::add_color(theme))
                                .child(format!("+{adds}")),
                        )
                        .child(
                            div()
                                .text_color(crate::changes::del_color(theme))
                                .child(format!("−{dels}")),
                        )
                });
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(3.0))
                .child(
                    div()
                        .h(px(16.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap(px(5.0))
                        .child(div().flex_none().child(leading))
                        .child(title_label)
                        .children(change_totals),
                )
                .when(branch_label.is_some() || pr_icon.is_some(), |el| {
                    el.child(
                        div()
                            .h(px(14.0))
                            .flex_none()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(px(4.0))
                            .when(branch_label.is_some(), |el| {
                                el.child(
                                    icon(icons::GIT_BRANCH)
                                        .size(px(10.0))
                                        .flex_none()
                                        .text_color(muted),
                                )
                            })
                            .children(branch_label)
                            .children(pr_icon),
                    )
                })
                .into_any_element()
        };
        let menu_key = key.clone();
        let middle_key = key.clone();
        let click_key = key.clone();
        let hover_key = key.clone();
        let hover_card = self.render_project_tab_hover_card(
            &key,
            title.clone(),
            detail,
            branch.clone(),
            pull_request.clone(),
            changes,
            cx,
        );
        div()
            .id(("project-tab", ix))
            .debug_selector(move || format!("project-tab-{ix}"))
            .group(group.clone())
            .relative()
            .w(px(self.settings.project_tabs.width.pixels()))
            .h(px(self.project_tab_height()))
            .flex_none()
            .pl(px(14.0))
            .pr(px(14.0))
            // The tab sits under the top gutter; this centres its contents
            // in the band rather than in the tab.
            .pb(px(Theme::TITLEBAR_TOP_PAD))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(8.0))
            .cursor_pointer()
            .role(gpui::Role::Button)
            .aria_label(title.clone())
            // Keep the titlebar's drag region from claiming the tab, while
            // wheel events still scroll the strip (see `surface_chrome::tab_chip`).
            .block_mouse_except_scroll()
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.hover_project_tab_card(&hover_key, *hovered, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.prevent_default();
                    this.dismiss_project_tab_card(cx);
                }),
            )
            .when(!active, |tab| {
                tab.child(
                    div()
                        .absolute()
                        .top(px(PROJECT_TAB_CHIP_INSET - Theme::TITLEBAR_TOP_PAD))
                        .bottom(px(PROJECT_TAB_CHIP_INSET))
                        .left(px(4.0))
                        .right(px(4.0))
                        .rounded(px(8.0))
                        .group_hover(group.clone(), |style| style.bg(crate::theme::wash(0.07))),
                )
            })
            .when(separator, |tab| {
                tab.child(
                    div()
                        .absolute()
                        .right_0()
                        .top(px(11.0))
                        .bottom(px(15.0))
                        .w(px(1.0))
                        .bg(theme.border),
                )
            })
            .child(content)
            .children(hover_card)
            .on_click(cx.listener(move |this, _, _, cx| {
                cx.stop_propagation();
                this.dismiss_project_tab_card(cx);
                this.activate_project_tab(click_key.clone(), cx);
            }))
            // Right-click opens the tab's menu.
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    window.prevent_default();
                    cx.stop_propagation();
                    this.dismiss_project_tab_card(cx);
                    this.open_project_tab_menu(menu_key.clone(), event.position, cx);
                }),
            )
            // Middle-click closes, like every tab strip.
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(move |this, _, _, cx| {
                    this.dismiss_project_tab_card(cx);
                    this.close_project_tab(middle_key.clone(), cx);
                }),
            )
            .on_drag(
                ProjectTabDrag {
                    from: ix,
                    title: title.clone(),
                },
                |payload, _point, _, cx| {
                    let title = payload.title.clone();
                    cx.stop_propagation();
                    // A drag starts from the tab, so its detail card should
                    // not remain over the drop targets.
                    cx.new(|_| SurfaceTabGhost { title })
                },
            )
            // Tabs tile the strip and block its own drop target (see the
            // right pane's strip), so each tab receives the drop too.
            .on_drop::<ProjectTabDrag>(cx.listener(|this, payload: &ProjectTabDrag, _, cx| {
                this.finish_project_tab_drag(payload.from, cx);
            }))
    }

    /// Every project, alphabetically, then "No project": a check marks the
    /// open tabs, and picking any row switches to it (reopening a hidden
    /// one). Below, the toggles for each tab's metadata.
    fn render_project_tabs_menu(
        &mut self,
        theme: &Theme,
        visible: &[String],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = &theme.for_popup();
        let known = self.project_tab_known(cx);
        let rows = self.project_tab_rows(&known, cx);
        let local_device = self.state.read(cx).local_device_id.clone();
        let profile = self.active_sidebar_pin_profile_key(cx);
        let active = self.settings.space_filter.clone();

        let mut list = div()
            .id("project-tabs-menu-list")
            .flex()
            .flex_col()
            .gap(px(2.0))
            .max_h(px(360.0))
            .overflow_y_scroll();
        for (ix, row) in rows.into_iter().enumerate() {
            let open = visible.contains(&row.key);
            let fade_key = format!("project-tabs-menu-row-{ix}");
            let art = match &row.space {
                Some(space) => div()
                    .size(px(14.0))
                    .flex_none()
                    .child(self.project_icons.render_exact(
                        &self.state,
                        space,
                        profile.clone(),
                        false,
                        cx,
                    ))
                    .into_any_element(),
                None => icon(icons::HOME)
                    .size(px(14.0))
                    .flex_none()
                    .text_color(theme.text_muted)
                    .into_any_element(),
            };
            // Name the host only for projects on another device.
            let device = row
                .space
                .as_ref()
                .filter(|space| local_device.as_deref() != Some(space.device_id.as_str()))
                .map(|space| {
                    let (tag, _) = self.state.read(cx).space_device_tag(space, Utc::now());
                    SharedString::from(tag)
                });
            let key = row.key.clone();
            list = list.child(
                popover::menu_row(theme, active.as_deref() == Some(row.key.as_str()), fade_key)
                    .id(("project-tabs-menu-row", ix))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.activate_project_tab(key.clone(), cx);
                    }))
                    .child(art)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .when(!open, |el| el.text_color(theme.text_muted))
                            .child(row.title),
                    )
                    .when_some(device, |el, device| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(crate::typography::ui_rems(11.0))
                                .text_color(theme.text_muted)
                                .child(device),
                        )
                    })
                    .child(div().w(px(14.0)).flex_none().when(open, |el| {
                        el.child(
                            icon(icons::CHECK)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        )
                    })),
            );
        }

        let meta = self.settings.project_tabs.meta;
        let detail_rows: [(&'static str, &'static str, bool, fn(&mut ProjectTabMeta)); 2] = [
            ("project-tabs-meta-branch", "Branch", meta.branch, |m| {
                m.branch = !m.branch
            }),
            (
                "project-tabs-meta-pr",
                "Pull request",
                meta.pull_request,
                |m| m.pull_request = !m.pull_request,
            ),
        ];
        let mut details = div().flex().flex_col().gap(px(2.0));
        for (id, label, on, toggle) in detail_rows {
            details = details.child(
                popover::menu_row(theme, false, id)
                    .id(id)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        toggle(&mut this.settings.project_tabs.meta);
                        this.sync_change_request_visibility(cx);
                        this.project_tab_branch_checked = None;
                        this.schedule_save(cx);
                        cx.notify();
                    }))
                    .child(div().flex_1().child(SharedString::from(label)))
                    .child(div().w(px(14.0)).flex_none().when(on, |el| {
                        el.child(
                            icon(icons::CHECK)
                                .size(px(12.0))
                                .text_color(theme.text_muted),
                        )
                    })),
            );
        }

        popover::popover_card(theme)
            .w(px(280.0))
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_project_tabs_menu(cx)))
            .child(popover::menu_heading(theme, "Projects"))
            .child(list)
            .child(popover::menu_separator())
            .child(
                popover::menu_row(theme, false, "project-tabs-menu-add")
                    .id("project-tabs-menu-add")
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.stop_propagation();
                        this.close_project_tabs_menu(cx);
                        this.open_add_space(cx);
                    }))
                    .child(
                        icon(icons::PLUS)
                            .size(px(13.0))
                            .text_color(theme.text_muted),
                    )
                    .child(SharedString::from("Add project…")),
            )
            .child(popover::menu_separator())
            .child(popover::menu_heading(theme, "Tab details"))
            .child(details)
            .into_any_element()
    }
}

/// A soft shade falling off below the band so it sits on the wallpaper
/// rather than ending in a hard cut.
fn paint_project_tab_shade(bounds: gpui::Bounds<Pixels>, shade: gpui::Hsla, window: &mut Window) {
    const DEPTH: f32 = 28.0;
    let below = gpui::Bounds::new(
        gpui::point(bounds.left(), bounds.bottom()),
        gpui::size(bounds.size.width, px(DEPTH)),
    );
    window.paint_quad(gpui::fill(
        below,
        gpui::linear_gradient(
            180.0,
            gpui::linear_color_stop(shade, 0.0),
            gpui::linear_color_stop(shade.opacity(0.0), 1.0),
        ),
    ));
}

/// The active chip's horizontal span in window coordinates. `scroll_x` is
/// the strip's scroll offset (negative once the strip has moved). `None`
/// when the chip has left the strip entirely. A chip only partly in view
/// still returns its full span — paint clips that to the strip, so the
/// rounded selection cannot draw over the sidebar.
fn project_tab_chip_span(
    region: gpui::Bounds<Pixels>,
    slot: usize,
    scroll_x: Pixels,
    tab_width: f32,
) -> Option<(Pixels, Pixels)> {
    let x0 = region.left() + px(slot as f32 * tab_width) + scroll_x;
    let x1 = x0 + px(tab_width);
    (x1 > region.left() && x0 < region.right()).then_some((x0, x1))
}

/// Where the chip may show: the tab strip across, the band down. Horizontal
/// edges are the overlap of the strip and the band, so a scrolled chip stops
/// at the strip (right of the sidebar) and never past the band.
fn project_tab_chip_mask(
    region: gpui::Bounds<Pixels>,
    band: gpui::Bounds<Pixels>,
) -> gpui::Bounds<Pixels> {
    let horizontal = region.intersect(&band);
    gpui::Bounds::from_corners(
        gpui::point(horizontal.left(), band.top()),
        gpui::point(horizontal.right(), band.bottom()),
    )
}

/// The active tab's chip on the new-session canvas.
#[derive(Clone, Copy)]
struct ProjectTabChip {
    fill: gpui::Hsla,
    border: gpui::Hsla,
}

/// Paint the active tab's chip, spanning `x0..x1` within the band `bounds`,
/// inset like the inactive tabs' hover chip and centred vertically in it.
fn paint_project_tab_chip(
    bounds: gpui::Bounds<Pixels>,
    x0: Pixels,
    x1: Pixels,
    chip: ProjectTabChip,
    window: &mut Window,
) {
    let rect = gpui::Bounds::from_corners(
        gpui::point(x0 + px(4.0), bounds.top() + px(PROJECT_TAB_CHIP_INSET)),
        gpui::point(x1 - px(4.0), bounds.bottom() - px(PROJECT_TAB_CHIP_INSET)),
    );
    window.paint_quad(gpui::quad(
        rect,
        px(PROJECT_TAB_RADIUS),
        chip.fill,
        px(1.0),
        chip.border,
        gpui::BorderStyle::default(),
    ));
}

/// Paint the tab bar's band into `bounds`: a tinted strip with a hairline
/// along its bottom edge.
fn paint_project_tab_band(
    bounds: gpui::Bounds<Pixels>,
    tint: gpui::Hsla,
    line: gpui::Hsla,
    window: &mut Window,
) {
    window.paint_quad(gpui::fill(bounds, tint));
    let edge = bounds.bottom() - px(0.5);
    let mut stroke = gpui::PathBuilder::stroke(px(1.0));
    stroke.move_to(gpui::point(bounds.left(), edge));
    stroke.line_to(gpui::point(bounds.right(), edge));
    if let Ok(path) = stroke.build() {
        window.paint_path(path, line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn saved_order_leads_and_new_projects_join_the_end() {
        let known = keys(&["a", "b", "c", "d", NO_PROJECT_TAB]);
        let tabs = visible_project_tabs(
            &keys(&["c", "gone", "a", "c"]),
            &keys(&["b", NO_PROJECT_TAB]),
            &known,
        );
        assert_eq!(tabs, keys(&["c", "a", "d"]));
    }

    #[test]
    fn seeding_opens_busy_projects_only() {
        let known = keys(&["a", "b", "c", NO_PROJECT_TAB]);
        let busy = std::collections::HashSet::from(["b".to_string(), NO_PROJECT_TAB.to_string()]);
        let (order, hidden) = seed_project_tabs(&known, &busy);
        assert_eq!(order, keys(&["b", NO_PROJECT_TAB]));
        assert_eq!(hidden, keys(&["a", "c"]));
    }

    #[test]
    fn seeding_without_sessions_opens_every_project() {
        let known = keys(&["a", "b", NO_PROJECT_TAB]);
        let (order, hidden) = seed_project_tabs(&known, &Default::default());
        assert_eq!(order, keys(&["a", "b"]));
        assert_eq!(hidden, keys(&[NO_PROJECT_TAB]));
    }

    #[test]
    fn hiding_hands_over_to_the_right_then_the_left() {
        let tabs = keys(&["a", "b", "c"]);
        let (order, hidden, next) = hide_project_tab(&tabs, &keys(&["z"]), "b");
        assert_eq!(order, keys(&["a", "c"]));
        assert_eq!(hidden, keys(&["z", "b"]));
        assert_eq!(next.as_deref(), Some("c"));
        let (_, _, next) = hide_project_tab(&tabs, &[], "c");
        assert_eq!(next.as_deref(), Some("b"));
        let (order, _, next) = hide_project_tab(&keys(&["a"]), &[], "a");
        assert!(order.is_empty());
        assert_eq!(next, None);
    }

    #[test]
    fn the_last_jump_slot_opens_the_last_tab() {
        let tabs = keys(&["a", "b", "c"]);
        assert_eq!(
            project_tab_for_slot(&tabs, 0).map(String::as_str),
            Some("a")
        );
        assert_eq!(
            project_tab_for_slot(&tabs, 2).map(String::as_str),
            Some("c")
        );
        assert_eq!(project_tab_for_slot(&tabs, 3), None);
        assert_eq!(
            project_tab_for_slot(&tabs, JUMP_SLOTS - 1).map(String::as_str),
            Some("c")
        );
    }

    fn space(id: &str, path: &str) -> zeron_proto::Space {
        zeron_proto::Space {
            id: id.into(),
            device_id: "device".into(),
            path: path.into(),
            name: None,
            git_detected: true,
            git_checked_at: None,
            checkout_id: None,
            repository_id: Some("commit:same".into()),
            github_repo: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn head_files_name_branches_or_short_commits() {
        assert_eq!(parse_git_head("ref: refs/heads/main\n").as_deref(), Some("main"));
        assert_eq!(
            parse_git_head("ref: refs/heads/feature/tabs").as_deref(),
            Some("feature/tabs")
        );
        assert_eq!(
            parse_git_head("0123456789abcdef0123456789abcdef01234567\n").as_deref(),
            Some("0123456")
        );
        assert_eq!(parse_git_head("garbage"), None);
        assert_eq!(parse_git_head(""), None);
    }

    #[test]
    fn branches_are_read_from_checkouts_and_linked_worktrees() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git/worktrees/wt")).unwrap();
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::write(repo.join(".git/HEAD"), "ref: refs/heads/main\n").unwrap();
        assert_eq!(read_git_branch(&repo).as_deref(), Some("main"));
        // A folder inside the checkout reads the checkout's branch.
        assert_eq!(read_git_branch(&repo.join("src")).as_deref(), Some("main"));
        // A linked worktree's `.git` file points at its own HEAD.
        let worktree = temp.path().join("wt");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", repo.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        std::fs::write(repo.join(".git/worktrees/wt/HEAD"), "ref: refs/heads/tabs\n").unwrap();
        assert_eq!(read_git_branch(&worktree).as_deref(), Some("tabs"));
        assert_eq!(read_git_branch(&temp.path().join("missing")), None);
    }

    #[test]
    fn remote_folders_take_their_newest_root_session_branch() {
        let folder = space("s", "/remote/app");
        let chat = |id: &str, root: &str, branch: &str, minutes: i64| -> zeron_proto::Chat {
            serde_json::from_value(serde_json::json!({
                "id": id, "deviceId": "device", "spaceId": "s", "archived": false,
                "createdAt": Utc::now() - chrono::Duration::minutes(minutes),
                "sourceContext": {
                    "checkoutId": "c", "repoRoot": root, "cwd": root, "branch": branch,
                    "observedAt": Utc::now(),
                },
            }))
            .unwrap()
        };
        let chats = vec![
            chat("old", "/remote/app", "main", 30),
            chat("new", "/remote/app/", "feature", 5),
            // Newest, but in a worktree: not the folder's own checkout.
            chat("wt", "/remote/.worktrees/app-x", "other", 1),
        ];
        assert_eq!(latest_session_branch(&chats, &folder).as_deref(), Some("feature"));
        assert_eq!(latest_session_branch(&[], &folder), None);
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> gpui::Bounds<Pixels> {
        gpui::Bounds::from_corners(gpui::point(px(x0), px(y0)), gpui::point(px(x1), px(y1)))
    }

    #[test]
    fn a_scrolled_chip_is_masked_to_the_strip() {
        // Sidebar seam at 256; the strip is inset to the right of it.
        for width in ProjectTabWidth::ALL {
            let tab_width = width.pixels();
            let band = rect(256.0, 0.0, 1400.0, 56.0);
            let region = rect(280.0, 4.0, 900.0, 56.0);
            let mask = project_tab_chip_mask(region, band);
            assert_eq!(mask.left(), region.left());
            assert_eq!(mask.right(), region.right());
            assert_eq!(mask.top(), band.top());
            assert_eq!(mask.bottom(), band.bottom());

            // Scrolled by most of a tab: the span crosses the sidebar, and the
            // mask still starts at the strip.
            let (x0, x1) =
                project_tab_chip_span(region, 0, px(24.0 - tab_width), tab_width).unwrap();
            assert!(x0 < band.left());
            assert!(x1 > mask.left() && x0 < mask.left());

            assert!(project_tab_chip_span(region, 0, px(-tab_width), tab_width).is_none());
            let (x0, x1) = project_tab_chip_span(region, 0, px(0.0), tab_width).unwrap();
            assert_eq!(x0, region.left());
            assert_eq!(x1, region.left() + px(tab_width));
        }
    }

    #[test]
    fn clones_sharing_a_folder_name_add_their_parent() {
        let spaces = vec![
            space("1", "/code/a/zeron"),
            space("2", "/code/b/zeron/"),
            space("3", "/code/notes"),
        ];
        assert_eq!(project_tab_title(&spaces[0], &spaces), "zeron · a");
        assert_eq!(project_tab_title(&spaces[1], &spaces), "zeron · b");
        assert_eq!(project_tab_title(&spaces[2], &spaces), "notes");
    }
}
