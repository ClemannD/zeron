//! Agent visuals: self-contained HTML pages an agent shows inline in its reply
//! through Zeron's `show_visual` MCP tool (charts, tables, diagrams, mockups).
//!
//! GPUI has no DOM, so a page renders in the same engine as the browser pane.
//! On Linux that engine already renders offscreen and GPUI composites its
//! frames, so an inline page scrolls, clips, and sits under the glass composer
//! like any other transcript row. macOS and Windows host pages as native child
//! views, which would draw over the composer the transcript scrolls under, so
//! there a visual is a card that opens the page in a browser tab.
//!
//! The browser path only loads http(s), so pages are served from a private
//! loopback server: one unguessable token per page. The server also carries
//! the page's two messages back to Zeron, its content height and the links the
//! reader clicks, as same-origin requests, so no platform needs a script bridge.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, px,
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use zeron_proto::{VISUAL_MAX_HEIGHT, VISUAL_MIN_HEIGHT};

use crate::markdown::render::LinkUi;
use crate::state::EngineHandle;
use crate::theme::Theme;

/// What a served page reports back.
#[derive(Debug, Clone, PartialEq)]
pub enum VisualSignal {
    /// The page's content height, with the page's report sequence: reports
    /// travel as separate requests, so an older one can arrive last.
    Height {
        height: u32,
        seq: u64,
    },
    Open(String),
}

#[derive(Default)]
struct Pages {
    pages: HashMap<String, (Arc<str>, UnboundedSender<VisualSignal>)>,
    theme_css: Arc<str>,
}

struct VisualServer {
    port: u16,
    pages: Arc<Mutex<Pages>>,
}

/// A page served for as long as this handle lives.
pub struct ServedPage {
    token: String,
    url: String,
}

impl Drop for ServedPage {
    fn drop(&mut self) {
        if let Some(server) = server() {
            lock(&server.pages).pages.remove(&self.token);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn server() -> Option<&'static VisualServer> {
    static SERVER: OnceLock<Option<VisualServer>> = OnceLock::new();
    SERVER
        .get_or_init(|| match VisualServer::start() {
            Ok(server) => Some(server),
            Err(error) => {
                tracing::warn!(%error, "visual server unavailable");
                None
            }
        })
        .as_ref()
}

impl VisualServer {
    fn start() -> std::io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let port = listener.local_addr()?.port();
        let pages = Arc::new(Mutex::new(Pages::default()));
        let shared = pages.clone();
        std::thread::Builder::new()
            .name("visual-server".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let pages = shared.clone();
                    let _ = std::thread::Builder::new()
                        .name("visual-request".into())
                        .spawn(move || serve(stream, port, &pages));
                }
            })?;
        Ok(Self { port, pages })
    }
}

fn serve(mut stream: TcpStream, port: u16, pages: &Mutex<Pages>) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut reader = BufReader::new(&stream);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).is_err() {
        return;
    }
    // Headers end at a blank line; only Host matters. Requiring our own
    // authority keeps a DNS-rebound site from reaching the pages.
    let mut host_ok = false;
    let mut header_bytes = 0;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return,
            Ok(n) => header_bytes += n,
        }
        if header_bytes > 16 * 1024 {
            return;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("host")
        {
            host_ok = value.trim() == format!("127.0.0.1:{port}");
        }
    }
    let response = respond(&request_line, host_ok, pages);
    let _ = stream.write_all(&response);
}

fn respond(request_line: &str, host_ok: bool, pages: &Mutex<Pages>) -> Vec<u8> {
    let mut parts = request_line.split_whitespace();
    let (Some("GET"), Some(target)) = (parts.next(), parts.next()) else {
        return reply(405, "text/plain", "");
    };
    if !host_ok {
        return reply(403, "text/plain", "");
    }
    let Ok(url) = url::Url::parse(&format!("http://visual{target}")) else {
        return reply(400, "text/plain", "");
    };
    let mut segments = url.path().trim_start_matches('/').split('/');
    let (Some("v"), Some(token), rest) = (segments.next(), segments.next(), segments.next()) else {
        return reply(404, "text/plain", "");
    };
    let pages = lock(pages);
    let Some((html, signals)) = pages.pages.get(token) else {
        return reply(404, "text/plain", "");
    };
    let query = |key: &str| {
        url.query_pairs()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.into_owned())
    };
    match rest.unwrap_or_default() {
        "" => reply(
            200,
            "text/html; charset=utf-8",
            &inject_bootstrap(html, &pages.theme_css),
        ),
        "size" => {
            if let Some(height) = query("h").and_then(|h| h.parse::<f64>().ok())
                && height.is_finite()
                && height > 0.0
            {
                let seq = query("s").and_then(|s| s.parse().ok()).unwrap_or(0);
                let _ = signals.send(VisualSignal::Height {
                    height: height.ceil() as u32,
                    seq,
                });
            }
            reply(204, "text/plain", "")
        }
        "open" => {
            if let Some(link) = query("u")
                && url::Url::parse(&link).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
            {
                let _ = signals.send(VisualSignal::Open(link));
            }
            reply(204, "text/plain", "")
        }
        _ => reply(404, "text/plain", ""),
    }
}

fn reply(status: u16, content_type: &str, body: &str) -> Vec<u8> {
    let reason = match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        405 => "Method Not Allowed",
        _ => "Not Found",
    };
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

/// Serve `html` until the returned handle drops. Signals from the page arrive
/// on the receiver.
fn serve_page(html: String) -> Option<(ServedPage, UnboundedReceiver<VisualSignal>)> {
    let server = server()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let (tx, rx) = unbounded_channel();
    lock(&server.pages)
        .pages
        .insert(token.clone(), (html.into(), tx));
    let url = format!("http://127.0.0.1:{}/v/{token}/", server.port);
    Some((ServedPage { token, url }, rx))
}

/// Pages fetch the theme when they load, so a theme change reloads them.
fn publish_theme(css: &Arc<str>) {
    if let Some(server) = server() {
        let mut pages = lock(&server.pages);
        if pages.theme_css != *css {
            pages.theme_css = css.clone();
        }
    }
}

fn css_color(color: gpui::Hsla) -> String {
    let rgba = gpui::Rgba::from(color);
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    if rgba.a >= 0.999 {
        format!(
            "#{:02x}{:02x}{:02x}",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b)
        )
    } else {
        format!(
            "rgba({},{},{},{:.3})",
            channel(rgba.r),
            channel(rgba.g),
            channel(rgba.b),
            rgba.a.clamp(0.0, 1.0)
        )
    }
}

fn css_font(family: &str, fallback: &str) -> String {
    format!(
        "\"{}\", {fallback}",
        family.replace(['"', '\\', ';', '<', '>'], "")
    )
}

/// The theme a page styles against, as `:root` custom properties. The names
/// are the ones `show_visual`'s description promises agents.
pub fn theme_css(theme: &Theme) -> String {
    let chart = [
        theme.accent,
        theme.syntax.function,
        theme.syntax.string,
        theme.syntax.keyword,
        theme.syntax.number,
        theme.syntax.type_name,
    ];
    let mut variables = vec![
        ("--background", css_color(theme.glass())),
        ("--foreground", css_color(theme.text)),
        ("--muted-foreground", css_color(theme.text_muted)),
        ("--faint-foreground", css_color(theme.text_faint)),
        ("--card", css_color(theme.surface_card)),
        ("--card-foreground", css_color(theme.text)),
        ("--border", css_color(theme.border)),
        ("--accent", css_color(theme.accent_strong)),
        ("--accent-foreground", css_color(theme.on_accent)),
        ("--danger", css_color(theme.danger)),
        ("--warning", css_color(theme.warning)),
        ("--success", css_color(theme.success)),
        ("--code-background", css_color(theme.code_wash)),
        ("--code-foreground", css_color(theme.code_text)),
        ("--radius", "10px".to_owned()),
        (
            "--font-sans",
            css_font(
                &theme.font_sans,
                "-apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif",
            ),
        ),
        (
            "--font-mono",
            css_font(
                &theme.font_mono,
                "Menlo, Consolas, \"Liberation Mono\", monospace",
            ),
        ),
    ];
    let chart_names = [
        "--chart-1",
        "--chart-2",
        "--chart-3",
        "--chart-4",
        "--chart-5",
        "--chart-6",
    ];
    variables.extend(
        chart_names
            .into_iter()
            .zip(chart.into_iter().map(css_color)),
    );
    let scheme = if theme.appearance.is_light() {
        "light"
    } else {
        "dark"
    };
    let mut css = format!(":root{{color-scheme:{scheme};");
    for (name, value) in variables {
        css.push_str(name);
        css.push(':');
        css.push_str(&value);
        css.push(';');
    }
    css.push('}');
    css.push_str(BASE_CSS);
    css
}

// The page is transparent: the chat shows through wherever it paints no
// background of its own. The frame scrolls nothing; a scrollbar inside a reply
// reads as a box within the chat, so it stays hidden.
const BASE_CSS: &str = "html{background:transparent;color:var(--foreground);\
font-family:var(--font-sans);font-size:14px;line-height:1.5;-webkit-font-smoothing:antialiased;\
scrollbar-width:none}html::-webkit-scrollbar{display:none}body{margin:0}\
code,kbd,pre,samp{font-family:var(--font-mono)}";

// Reports the content height (measured as the frame needs it: the document's
// own height unless it overflows) and hands link clicks to Zeron, both as
// same-origin requests to the page's own server path.
const BOOTSTRAP_SCRIPT: &str = r##"(function(){var h=0,n=0;function z(){var r=document.documentElement,v=Math.ceil(r.scrollHeight>r.clientHeight?r.scrollHeight:r.getBoundingClientRect().height);if(!v||v===h)return;h=v;fetch("size?h="+v+"&s="+(++n),{cache:"no-store"}).catch(function(){});}if(window.ResizeObserver){var o=new ResizeObserver(z);o.observe(document.documentElement);document.addEventListener("DOMContentLoaded",function(){if(document.body)o.observe(document.body);});}document.addEventListener("DOMContentLoaded",z);window.addEventListener("load",z);document.addEventListener("click",function(e){var l=e.composedPath().find(function(t){return t&&t.matches&&t.matches("a[href]");}),u;if(!l)return;try{u=new URL(l.getAttribute("href"),document.baseURI);}catch(x){return;}if(!/^https?:$/.test(u.protocol)||u.href.split("#")[0]===location.href.split("#")[0])return;e.preventDefault();fetch("open?u="+encodeURIComponent(u.href),{cache:"no-store"}).catch(function(){});},true);})();"##;

/// Put the theme and bootstrap at the start of the document head, so the
/// page's own styles and scripts come after (and override) them.
fn inject_bootstrap(html: &str, theme_css: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut markup = String::new();
    if !lower.contains("<meta charset") {
        markup.push_str("<meta charset=\"utf-8\">");
    }
    if !lower.contains("name=\"viewport\"") {
        markup.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">");
    }
    markup.push_str("<style id=\"zeron-theme\">");
    markup.push_str(theme_css);
    markup.push_str("</style><script>");
    markup.push_str(BOOTSTRAP_SCRIPT);
    markup.push_str("</script>");
    let tag_end = |open: &str| {
        let start = lower.find(open)?;
        let next = lower.as_bytes().get(start + open.len())?;
        (*next == b'>' || next.is_ascii_whitespace())
            .then(|| lower[start..].find('>').map(|end| start + end + 1))
            .flatten()
    };
    if let Some(at) = tag_end("<head") {
        format!("{}{markup}{}", &html[..at], &html[at..])
    } else if let Some(at) = tag_end("<html") {
        format!("{}<head>{markup}</head>{}", &html[..at], &html[at..])
    } else {
        format!("<!doctype html><head>{markup}</head>{html}")
    }
}

fn clamp_height(height: u32) -> f32 {
    height.clamp(VISUAL_MIN_HEIGHT, VISUAL_MAX_HEIGHT) as f32
}

enum Load {
    Loading,
    Ready(ServedPage),
    Failed,
}

/// One inline visual in a transcript.
pub struct VisualFrame {
    id: SharedString,
    title: SharedString,
    /// The agent's height, until the page reports its own.
    height: u32,
    content_height: Option<u32>,
    height_seq: u64,
    load: Load,
    signals: Option<UnboundedReceiver<VisualSignal>>,
    link_ui: Option<LinkUi>,
    theme_css: Arc<str>,
    #[cfg(target_os = "linux")]
    surface: Option<gpui::Entity<crate::browser::BrowserSurface>>,
    #[cfg(target_os = "linux")]
    _surface_events: Option<gpui::Subscription>,
    /// When this frame last rendered. Pages offscreen for a while release
    /// their engine page and reload when scrolled back.
    last_rendered: std::time::Instant,
    _tasks: Vec<gpui::Task<()>>,
}

impl VisualFrame {
    pub fn new(
        id: SharedString,
        title: String,
        height: u32,
        path: String,
        engine: Option<EngineHandle>,
        target_device_id: Option<String>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut tasks = Vec::new();
        let load = match engine {
            Some(engine) => {
                tasks.push(cx.spawn(async move |this, cx| {
                    let page = crate::attachments::read_attachment_bytes(
                        &engine,
                        cx.background_executor(),
                        target_device_id.as_deref(),
                        &path,
                        None,
                    )
                    .await
                    .and_then(|(_, _, bytes)| String::from_utf8(bytes).ok())
                    .and_then(serve_page);
                    let _ = this.update(cx, |this, cx| {
                        match page {
                            Some((page, signals)) => {
                                this.load = Load::Ready(page);
                                this.signals = Some(signals);
                            }
                            None => this.load = Load::Failed,
                        }
                        cx.notify();
                    });
                }));
                Load::Loading
            }
            None => Load::Failed,
        };
        Self {
            id,
            title: title.into(),
            height,
            content_height: None,
            height_seq: 0,
            load,
            signals: None,
            link_ui: None,
            theme_css: Arc::from(""),
            #[cfg(target_os = "linux")]
            surface: None,
            #[cfg(target_os = "linux")]
            _surface_events: None,
            last_rendered: std::time::Instant::now(),
            _tasks: tasks,
        }
    }

    pub fn set_link_ui(&mut self, link_ui: Option<LinkUi>) {
        self.link_ui = link_ui;
    }

    pub fn idle_for(&self) -> Duration {
        self.last_rendered.elapsed()
    }

    /// Give the engine page back; the next render loads it again.
    pub fn release_page(&mut self, cx: &mut Context<Self>) {
        #[cfg(target_os = "linux")]
        if let Some(surface) = self.surface.take() {
            tracing::debug!(visual = %self.id, "visual offscreen; released its page");
            self._surface_events = None;
            surface.update(cx, |surface, cx| surface.close(cx));
        }
        #[cfg(not(target_os = "linux"))]
        let _ = cx;
    }

    pub fn has_page(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.surface.is_some()
        }
        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn url(&self) -> Option<&str> {
        match &self.load {
            Load::Ready(page) => Some(&page.url),
            _ => None,
        }
    }

    fn frame_height(&self) -> f32 {
        clamp_height(self.content_height.unwrap_or(self.height))
    }

    fn open_link(
        &self,
        url: String,
        action: crate::markdown::links::LinkAction,
        window: &mut Window,
        cx: &mut App,
    ) {
        crate::markdown::render::activate_link(
            crate::markdown::links::LinkTarget::new(&url, &url),
            action,
            self.link_ui.as_ref(),
            window,
            cx,
        );
    }

    fn on_signal(&mut self, signal: VisualSignal, window: &mut Window, cx: &mut Context<Self>) {
        match signal {
            VisualSignal::Height { height, seq } => {
                if seq <= self.height_seq {
                    return;
                }
                self.height_seq = seq;
                if self.content_height != Some(height) {
                    self.content_height = Some(height);
                    cx.notify();
                }
            }
            VisualSignal::Open(url) => {
                self.open_link(url, crate::markdown::links::LinkAction::Primary, window, cx);
            }
        }
    }

    fn listen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mut signals) = self.signals.take() else {
            return;
        };
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Some(signal) = signals.recv().await {
                if this
                    .update_in(cx, |this, window, cx| this.on_signal(signal, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    #[cfg(target_os = "linux")]
    fn ensure_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui::AppContext as _;
        let Some(url) = self.url().map(str::to_owned) else {
            return;
        };
        if let Some(surface) = &self.surface {
            if self.theme_css != current_theme_css(cx) {
                self.theme_css = current_theme_css(cx);
                // A reload restarts the page's report sequence.
                self.height_seq = 0;
                surface.update(cx, |surface, cx| surface.navigate(&url, window, cx));
            }
            return;
        }
        self.theme_css = current_theme_css(cx);
        self.height_seq = 0;
        tracing::debug!(visual = %self.id, "loading visual page");
        let context = visual_browser_context();
        let surface = cx.new(|cx| {
            let mut surface = crate::browser::BrowserSurface::new(context, false, window, cx);
            surface.set_inline();
            surface.set_presentation(crate::browser::model::Presentation::Live, cx);
            surface.navigate(&url, window, cx);
            surface
        });
        // A page that opens a new window (target=_blank, window.open) asks
        // for a tab; send it where chat links go.
        self._surface_events = Some(cx.subscribe_in(
            &surface,
            window,
            |this, _, event: &crate::browser::BrowserEvent, window, cx| {
                if let crate::browser::BrowserEvent::NewTab(Some(url)) = event {
                    this.open_link(
                        url.clone(),
                        crate::markdown::links::LinkAction::Primary,
                        window,
                        cx,
                    );
                }
            },
        ));
        self.surface = Some(surface);
    }
}

/// Visuals share one ephemeral browser context: a page never sees the
/// browser pane's cookies, and every visual uses the same engine helper.
#[cfg(target_os = "linux")]
fn visual_browser_context() -> crate::browser::BrowserContext {
    static CONTEXT: OnceLock<crate::browser::BrowserContext> = OnceLock::new();
    CONTEXT.get_or_init(Default::default).clone()
}

/// The active theme as page CSS. The server's copy is replaced only when the
/// theme changes, so pages load with the theme the app shows.
fn current_theme_css(cx: &App) -> Arc<str> {
    thread_local! {
        static CACHE: std::cell::RefCell<Option<(u64, Arc<str>)>> = const { std::cell::RefCell::new(None) };
    }
    let css = theme_css(Theme::of(cx));
    let hash = {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        css.hash(&mut hasher);
        hasher.finish()
    };
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some((cached, css)) = cache.as_ref()
            && *cached == hash
        {
            return css.clone();
        }
        let css: Arc<str> = css.into();
        publish_theme(&css);
        *cache = Some((hash, css.clone()));
        css
    })
}

impl Render for VisualFrame {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.last_rendered = std::time::Instant::now();
        // The page reads the theme when it loads.
        let _ = current_theme_css(cx);
        self.listen(window, cx);
        #[cfg(target_os = "linux")]
        self.ensure_page(window, cx);
        let theme = Theme::of(cx).clone();
        let id = self.id.clone();
        let failed = matches!(self.load, Load::Failed);
        let url = self.url().map(str::to_owned);

        #[cfg(target_os = "linux")]
        if let Some(surface) = self.surface.clone() {
            let open_url = url.clone();
            return div()
                .id(SharedString::from(format!("{id}-visual")))
                .group("visual")
                .relative()
                .w_full()
                .h(px(self.frame_height()))
                .role(gpui::Role::Group)
                .aria_label(self.title.clone())
                .child(surface)
                .when_some(open_url, |el, url| {
                    el.child(
                        div()
                            .absolute()
                            .top(px(8.0))
                            .right(px(8.0))
                            .invisible()
                            .group_hover("visual", |style| style.visible())
                            .child(open_button(&id, url, &theme, cx)),
                    )
                })
                .into_any_element();
        }

        visual_card(&id, &self.title, failed, url, &theme, cx)
    }
}

fn open_button(
    id: &SharedString,
    url: String,
    theme: &Theme,
    cx: &mut Context<VisualFrame>,
) -> AnyElement {
    div()
        .id(SharedString::from(format!("{id}-visual-open")))
        .size(px(24.0))
        .rounded(px(6.0))
        .flex()
        .items_center()
        .justify_center()
        .cursor_pointer()
        .bg(theme.surface_raised)
        .border_1()
        .border_color(theme.border)
        .hover(|style| style.bg(theme.surface_raised_hover))
        .role(gpui::Role::Button)
        .aria_label("Open in a browser tab")
        .tooltip(crate::settings::widgets::text_tooltip(
            "Open in a browser tab",
        ))
        .on_click(cx.listener(move |this, _, window, cx| {
            cx.stop_propagation();
            this.open_link(
                url.clone(),
                crate::markdown::links::LinkAction::Internal,
                window,
                cx,
            );
        }))
        .child(
            crate::icons::icon(crate::icons::EXPAND_ARROWS)
                .size(px(12.0))
                .text_color(theme.text_muted),
        )
        .into_any_element()
}

/// Loading, failure, and the macOS/Windows presentation: the page's title
/// with an action that opens it in a browser tab.
fn visual_card(
    id: &SharedString,
    title: &SharedString,
    failed: bool,
    url: Option<String>,
    theme: &Theme,
    cx: &mut Context<VisualFrame>,
) -> AnyElement {
    let status: SharedString = if failed {
        "Visual unavailable".into()
    } else if url.is_none() || cfg!(target_os = "linux") {
        "Loading visual…".into()
    } else {
        "Open to view this visual".into()
    };
    div()
        .id(SharedString::from(format!("{id}-visual-card")))
        .w_full()
        .flex()
        .items_center()
        .gap(px(10.0))
        .px(px(12.0))
        .py(px(10.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(theme.border)
        .bg(crate::theme::ink(0.045))
        .child(
            crate::icons::icon(crate::icons::GLOBE)
                .size(px(14.0))
                .flex_none()
                .text_color(theme.text_muted),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .flex()
                .flex_col()
                .child(
                    div()
                        .truncate()
                        .text_size(crate::typography::ui_rems(13.0))
                        .text_color(theme.text)
                        .child(title.clone()),
                )
                .child(
                    div()
                        .text_size(crate::typography::ui_rems(11.0))
                        .text_color(theme.text_muted)
                        .child(status),
                ),
        )
        .when_some(url.filter(|_| !cfg!(target_os = "linux")), |el, url| {
            el.child(open_button(id, url, theme, cx))
        })
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bootstrap_lands_at_the_start_of_head() {
        let page = inject_bootstrap(
            "<!doctype html><html><head><title>x</title></head><body>hi</body></html>",
            ":root{}",
        );
        let head = page.find("<head>").unwrap() + "<head>".len();
        assert!(page[head..].starts_with("<meta charset=\"utf-8\">"));
        assert!(page.find("zeron-theme").unwrap() < page.find("<title>").unwrap());
    }

    #[test]
    fn bootstrap_wraps_fragments_and_skips_lookalike_tags() {
        let page = inject_bootstrap("<header>chart</header>", ":root{}");
        assert!(page.starts_with("<!doctype html><head><meta charset"));
        assert!(page.ends_with("<header>chart</header>"));
        let page = inject_bootstrap("<html lang=\"en\"><body>x</body></html>", ":root{}");
        assert!(page.starts_with("<html lang=\"en\"><head><meta charset"));
    }

    #[test]
    fn bootstrap_keeps_the_page_charset_and_viewport() {
        let page = inject_bootstrap(
            "<head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=400\"></head>",
            "",
        );
        assert_eq!(page.matches("charset").count(), 1);
        assert_eq!(page.matches("name=\"viewport\"").count(), 1);
    }

    #[test]
    fn server_serves_pages_and_relays_signals_for_its_own_host_only() {
        let pages = Mutex::new(Pages::default());
        let (tx, mut rx) = unbounded_channel();
        lock(&pages)
            .pages
            .insert("tok".into(), (Arc::from("<p>hi</p>"), tx));
        let ok = String::from_utf8(respond("GET /v/tok/ HTTP/1.1", true, &pages)).unwrap();
        assert!(ok.starts_with("HTTP/1.1 200"));
        assert!(ok.contains("<p>hi</p>") && ok.contains("zeron-theme"));
        let forbidden = String::from_utf8(respond("GET /v/tok/ HTTP/1.1", false, &pages)).unwrap();
        assert!(forbidden.starts_with("HTTP/1.1 403"));
        let missing = String::from_utf8(respond("GET /v/nope/ HTTP/1.1", true, &pages)).unwrap();
        assert!(missing.starts_with("HTTP/1.1 404"));

        respond("GET /v/tok/size?h=412.4&s=3 HTTP/1.1", true, &pages);
        assert_eq!(
            rx.try_recv().unwrap(),
            VisualSignal::Height {
                height: 413,
                seq: 3
            }
        );
        respond(
            "GET /v/tok/open?u=https%3A%2F%2Fexample.com%2Fa HTTP/1.1",
            true,
            &pages,
        );
        assert_eq!(
            rx.try_recv().unwrap(),
            VisualSignal::Open("https://example.com/a".into())
        );
        respond(
            "GET /v/tok/open?u=javascript%3Aalert(1) HTTP/1.1",
            true,
            &pages,
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn theme_css_defines_every_promised_variable() {
        let css = theme_css(&Theme::dark());
        for name in [
            "--background",
            "--foreground",
            "--muted-foreground",
            "--faint-foreground",
            "--card",
            "--card-foreground",
            "--border",
            "--accent",
            "--accent-foreground",
            "--danger",
            "--warning",
            "--success",
            "--code-background",
            "--code-foreground",
            "--chart-6",
            "--radius",
            "--font-sans",
            "--font-mono",
        ] {
            assert!(css.contains(&format!("{name}:")), "{name}");
        }
    }
}
