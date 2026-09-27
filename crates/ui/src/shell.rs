//! The main window: owns the model, services and widgets, and routes Win32 messages.

use crate::chat::{ChatView, Ctx, Hit, LinkTarget};
use crate::editor::Editor;
use crate::gfx::{Gfx, Painter, Rect, rgba, with_alpha};
use crate::lists::{NickList, Sidebar, SidebarButton};
use crate::overlay::{ConfirmAction, Overlay, OverlayClick, draw_field};
use crate::text::{self, Brushes, Text};
use crate::theme::Theme;
use crate::win::{self, MenuItem, Tray};
use schwaetz_core::services::{HistoryStore, ScriptHost};
use schwaetz_core::{
    App, BufferId, BufferKind, Config, ConnState, Effect, LineKind, NetworkConfig, NetworkKind, NotifyLevel, Paths,
};
use schwaetz_net::{NetCommand, NetEvent, NetHandle};
use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::{AtomicIsize, Ordering};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, ScreenToClient};
const PBT_APMRESUMEAUTOMATIC: u32 = 0x12;
const PBT_APMRESUMESUSPEND: u32 = 0x07;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::Ime::{
    CANDIDATEFORM, CFS_CANDIDATEPOS, CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext,
    ImmSetCandidateWindow, ImmSetCompositionWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, PCWSTR, w};

pub const WM_APP_NET: u32 = WM_APP + 1;
pub const WM_APP_MEDIA: u32 = WM_APP + 3;
/// Background work (Twitch API) finished on its worker thread.
pub const WM_APP_LIVE: u32 = WM_APP + 4;

/// A button-like control pressed with the left mouse button (acts on release, like Windows
/// buttons, and not at all if the pointer was dragged off it).
#[derive(Clone, Debug, PartialEq)]
enum Pressed {
    Overlay(crate::overlay::OverlayTarget),
    NetworkSettings(BufferId),
    Sidebar(SidebarButton),
    ReplyClose,
    JumpPill,
    OpenStream,
    /// A row of the emote completion list.
    Completion(usize),
    Chat(Hit),
}

/// The emote list shown while typing `:word` in a Twitch channel.
struct EmoteCompletion {
    /// Byte offset of the `:` in the input; the text up to the caret is replaced on accept.
    colon: usize,
    query: String,
    items: Vec<schwaetz_core::emotes::EmoteEntry>,
    selected: usize,
    scroll: f32,
    /// Where the list was drawn (for the mouse).
    rect: Rect,
}

/// Height of one emote row in the completion list.
const COMPLETION_ROW: f32 = 32.0;

enum WorkerResult {
    Live(schwaetz_core::helix::LiveResult),
    Emotes(schwaetz_core::helix::EmoteResult),
    Auth(schwaetz_net::NetworkId, schwaetz_core::twitch_auth::AuthRequest, schwaetz_core::twitch_auth::AuthResponse),
}
/// `dwData` tag for WM_COPYDATA messages carrying input for the running instance.
pub const COPYDATA_MAGIC: usize = 0x5357_4158;
const TIMER_TICK: usize = 1;
const TIMER_CARET: usize = 2;
const TIMER_PAINT: usize = 3;
const TIMER_ANIM: usize = 4;
/// Next frame of a hover/press/switch transition.
const TIMER_UI_ANIM: usize = 5;
const SIDEBAR_MIN: f32 = 160.0;
const TOPIC_H: f32 = 56.0;
const NICKLIST_W: f32 = 210.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Drag {
    None,
    Chat,
    Input,
    Splitter,
    /// A channel row pressed in the sidebar; it becomes a rearranging drag once the pointer has
    /// moved a few pixels (`row_drag_from`).
    Row(BufferId),
}

pub struct Services {
    pub history: Option<Box<dyn HistoryStore>>,
    pub scripts: Option<Box<dyn ScriptHost>>,
}

pub struct Ui {
    hwnd: HWND,
    gfx: Option<Gfx>,
    text: Text,
    theme: Theme,
    brushes: Brushes,
    pub app: App,
    net: NetHandle,
    services: Services,
    paths: Paths,
    sidebar: Sidebar,
    chat: ChatView,
    nicklist: NickList,
    input: Editor,
    overlay: Option<Overlay>,
    sidebar_w: f32,
    show_nicklist: bool,
    topic_rect: Rect,
    input_rect: Rect,
    typing_rect: Rect,
    win_rect: Rect,
    scale: f32,
    mica: bool,
    drag: Drag,
    caret_on: bool,
    last_click: (u32, f32, f32, u32),
    tray: Tray,
    notify_buffer: Option<BufferId>,
    high_surrogate: Option<u16>,
    last_typing_sent: i64,
    shown_buffer: Option<BufferId>,
    nicklist_gen: (Option<BufferId>, u64),
    quitting: bool,
    media: schwaetz_media::Media,
    images: std::rc::Rc<std::cell::RefCell<crate::images::ImageStore>>,
    preview_pending: std::collections::HashSet<String>,
    /// Results of background work (Twitch API calls); workers post `WM_APP_LIVE`.
    workers: (std::sync::mpsc::Sender<WorkerResult>, std::sync::mpsc::Receiver<WorkerResult>),
    active_window: bool,
    /// Buffer to reselect once it exists again after startup (network, buffer).
    restore_active: Option<(String, String)>,
    form: Option<Box<crate::form::Form>>,
    background_update: bool,
    paint_pending: bool,
    anim_start: std::time::Instant,
    a11y_window: (i64, u32),
    /// Message being replied to (shown above the input).
    reply: Option<ReplyTarget>,
    reply_rect: Rect,
    mouse_tracking: bool,
    /// Hovered inline emote: (code, image url, bounds).
    tooltip: Option<(String, String, Rect)>,
    /// Button-like control under a pressed left button; it acts on release if still under it.
    pressed: Option<Pressed>,
    /// Hover/press/switch transitions of buttons.
    pub(crate) anims: crate::anim::Anims,
    /// "Open stream" button in the topic bar (Twitch channels), as last drawn.
    stream_btn: Option<Rect>,
    /// Sidebar row under a pressed middle button (acts on release over the same row).
    middle_pressed: Option<BufferId>,
    /// Pointer height where a channel row was pressed (a drag starts a few pixels away).
    row_drag_from: f32,
    /// Emote completion list for a `:word` being typed.
    completion: Option<EmoteCompletion>,
    /// Byte offset of the `:` whose completion was dismissed with Esc (stays closed for it).
    completion_dismissed: Option<usize>,
    /// What the completion was last computed for: (`:` offset, query, emote data generation).
    /// Emote lists arrive asynchronously; a newer generation recomputes even with no key press.
    completion_for: Option<(usize, String, u64)>,
}

thread_local! {
    static UI: RefCell<Option<Box<Ui>>> = const { RefCell::new(None) };
}

/// Creates the window and runs the message loop until the app exits.
pub fn run(config: Config, paths: Paths, services: Services, startup_notes: Vec<String>) -> windows::core::Result<()> {
    unsafe {
        let _ = windows::Win32::UI::HiDpi::SetProcessDpiAwarenessContext(
            windows::Win32::UI::HiDpi::DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
        );
    }
    let hinst: HINSTANCE = unsafe { windows::Win32::System::LibraryLoader::GetModuleHandleW(None)?.into() };
    let class = w!("schwaetz.main");
    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(wndproc),
        hInstance: hinst,
        hIcon: win::app_icon(false),
        hIconSm: win::app_icon(true),
        hCursor: unsafe { LoadCursorW(None, IDC_ARROW)? },
        lpszClassName: class,
        ..Default::default()
    };
    unsafe { RegisterClassExW(&wc) };
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_NOREDIRECTIONBITMAP,
            class,
            w!("schwätz"),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(hinst),
            None,
        )?
    };
    let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
    let scale = dpi / 96.0;
    unsafe {
        let _ =
            SetWindowPos(hwnd, None, 0, 0, (1180.0 * scale) as i32, (760.0 * scale) as i32, SWP_NOMOVE | SWP_NOZORDER);
    }

    let hwnd_shared = Arc::new(AtomicIsize::new(hwnd.0 as isize));
    let media_target = hwnd_shared.clone();
    let media = schwaetz_media::Media::start(Some(paths.cache_dir.clone()), 2, move || {
        let h = HWND(media_target.load(Ordering::Relaxed) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(h), WM_APP_MEDIA, WPARAM(0), LPARAM(0));
        }
    });
    let wake_target = hwnd_shared.clone();
    let net = NetHandle::start(move || {
        let h = HWND(wake_target.load(Ordering::Relaxed) as *mut _);
        unsafe {
            let _ = PostMessageW(Some(h), WM_APP_NET, WPARAM(0), LPARAM(0));
        }
    })
    .map_err(|e| windows::core::Error::new(E_FAIL, e.to_string()))?;

    let mut gfx = Gfx::new(hwnd)?;
    gfx.set_dpi(dpi);
    let text = Text::new(
        gfx.dwrite.clone(),
        &config.appearance.font,
        config.appearance.font_size,
        &config.appearance.ui_font,
    )?;
    let mut services = services;
    let mut app = App::new(config);
    if let Some(h) = services.history.take() {
        app.set_history(h);
    }
    // New lines feed scripts and screen-reader announcements.
    app.track_new_lines = true;
    for note in startup_notes {
        let sb = app.status_buffer;
        app.print(sb, LineKind::Status, "", &note);
    }
    let mut ui = Box::new(Ui {
        hwnd,
        gfx: Some(gfx),
        text,
        theme: Theme::dark(),
        brushes: Brushes::default(),
        app,
        net,
        services,
        paths,
        sidebar: Sidebar::default(),
        chat: ChatView::default(),
        nicklist: NickList::default(),
        input: Editor::default(),
        overlay: None,
        sidebar_w: 230.0,
        show_nicklist: false,
        topic_rect: Rect::default(),
        input_rect: Rect::default(),
        typing_rect: Rect::default(),
        win_rect: Rect::default(),
        scale,
        mica: false,
        drag: Drag::None,
        caret_on: true,
        last_click: (0, 0.0, 0.0, 0),
        tray: Tray::new(hwnd),
        notify_buffer: None,
        high_surrogate: None,
        last_typing_sent: 0,
        shown_buffer: None,
        nicklist_gen: (None, 0),
        quitting: false,
        media,
        images: Default::default(),
        preview_pending: Default::default(),
        workers: std::sync::mpsc::channel(),
        active_window: true,
        restore_active: None,
        form: None,
        background_update: false,
        paint_pending: false,
        anim_start: std::time::Instant::now(),
        a11y_window: (0, 0),
        reply: None,
        reply_rect: Rect::default(),
        mouse_tracking: false,
        tooltip: None,
        pressed: None,
        anims: Default::default(),
        stream_btn: None,
        middle_pressed: None,
        row_drag_from: 0.0,
        completion: None,
        completion_dismissed: None,
        completion_for: None,
    });
    let session = crate::session::Session::load(&ui.paths.session_file());
    if let Some(w) = session.sidebar_width {
        ui.sidebar_w = w;
    }
    ui.restore_active = session.active.clone();
    let placed = session.apply_window(hwnd);
    ui.apply_appearance();
    ui.tray.add();
    ui.welcome();
    // Load scripts before connecting so they see the first events of every network.
    if let Some(h) = ui.services.scripts.as_mut() {
        h.tick(&mut ui.app, now());
    }
    ui.app.connect_auto();
    ui.after_update();
    UI.with(|c| *c.borrow_mut() = Some(ui));
    unsafe {
        SetTimer(Some(hwnd), TIMER_TICK, 1000, None);
        SetTimer(Some(hwnd), TIMER_CARET, GetCaretBlinkTime().max(300), None);
        if !placed {
            let _ = ShowWindow(hwnd, SW_SHOWDEFAULT);
        }
    }

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // Drop the UI (and with it the network thread, which sends QUITs) after the loop.
    // The history store flushes pending writes when the model is dropped.
    let ui = UI.with(|c| c.borrow_mut().take());
    drop(ui);
    Ok(())
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    // Screen readers ask for the automation tree; answer without touching UI state.
    if msg == WM_GETOBJECT && lp.0 as i32 == windows::Win32::UI::Accessibility::UiaRootObjectId {
        let root = crate::a11y::Root::provider(hwnd);
        return unsafe { windows::Win32::UI::Accessibility::UiaReturnRawElementProvider(hwnd, wp, lp, &root) };
    }
    let handled = UI.with(|cell| match cell.try_borrow_mut() {
        Ok(mut guard) => guard.as_mut().and_then(|ui| ui.handle(msg, wp, lp)),
        // Re-entered from a nested modal loop (context menu): default processing.
        Err(_) => None,
    });
    match handled {
        Some(r) => r,
        None => {
            if msg == WM_DESTROY {
                unsafe { PostQuitMessage(0) };
                return LRESULT(0);
            }
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
    }
}

fn now() -> i64 {
    schwaetz_core::time::now_ms()
}

impl Ui {
    fn invalidate(&self) {
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    fn welcome(&mut self) {
        let sb = self.app.status_buffer;
        let lines = [
            format!("Welcome to schwätz {}.", env!("CARGO_PKG_VERSION")),
            "Connect with /connect irc.libera.chat (or a configured network name), then /join #channel.".to_owned(),
            "Ctrl+J quick switcher · Alt+1…9 switch buffers · Alt+A next activity · Ctrl+W close · /help lists commands.".to_owned(),
            format!("Settings live in {} — edit with /set section.key value.", self.paths.config_file().display()),
        ];
        if self.app.networks.is_empty() {
            for l in lines {
                self.app.print(sb, LineKind::Status, "", &l);
            }
        } else {
            self.app.print(sb, LineKind::Status, "", &lines[0]);
        }
    }

    /// Re-reads theme, fonts and window chrome from the config.
    fn apply_appearance(&mut self) {
        let a = &self.app.config.appearance;
        let dark = match a.theme.as_str() {
            "light" => false,
            "dark" => true,
            "system" => win::is_system_dark(),
            name => {
                let path = self.paths.themes_dir().join(format!("{name}.toml"));
                match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| Theme::from_toml(&t)) {
                    Ok(t) => {
                        self.theme = t;
                        self.finish_appearance();
                        return;
                    }
                    Err(e) => {
                        let sb = self.app.status_buffer;
                        self.app.print(sb, LineKind::Error, "", &format!("Theme {name}: {e}"));
                        win::is_system_dark()
                    }
                }
            }
        };
        self.theme = if dark { Theme::dark() } else { Theme::light() };
        self.finish_appearance();
    }

    fn finish_appearance(&mut self) {
        let a = &self.app.config.appearance;
        let _ = self.text.reconfigure(&a.font, a.font_size, &a.ui_font);
        if let Some(g) = self.gfx.as_mut() {
            g.set_gpu(a.gpu_acceleration);
        }
        self.app.dark_theme = self.theme.dark;
        win::set_dark_titlebar(self.hwnd, self.theme.dark);
        win::allow_dark_menus(self.theme.dark);
        self.mica = win::enable_mica(self.hwnd, a.mica);
        self.chat.style_gen += 1;
        self.invalidate();
    }

    fn client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe {
            let _ = GetClientRect(self.hwnd, &mut r);
        }
        ((r.right - r.left).max(1) as u32, (r.bottom - r.top).max(1) as u32)
    }

    fn layout(&mut self) {
        let (pw, ph) = self.client_size();
        let (w, h) = (pw as f32 / self.scale, ph as f32 / self.scale);
        self.win_rect = Rect::new(0.0, 0.0, w, h);
        self.sidebar_w = self.sidebar_w.clamp(SIDEBAR_MIN, (w * 0.4).max(SIDEBAR_MIN));
        let sw = self.sidebar_w;
        self.sidebar.rect = Rect::new(0.0, 0.0, sw, h);
        let active = self.app.active_buffer();
        let is_channel = active.kind == BufferKind::Channel;
        self.show_nicklist = is_channel && self.app.config.appearance.show_nicklist && w - sw > 640.0;
        let nick_w = if self.show_nicklist { NICKLIST_W } else { 0.0 };
        let main_w = w - sw;
        self.topic_rect = Rect::new(sw, 0.0, main_w, TOPIC_H);
        // Input grows with its content (up to ~8 lines).
        let lh = self.text.fonts.line_height;
        let content = self.input.content_height().max(lh);
        let input_h = (content + 22.0).clamp(lh + 22.0, lh * 8.0 + 22.0);
        let typing_h = 20.0;
        let chat_w = main_w - nick_w;
        self.input_rect = Rect::new(sw + 14.0, h - input_h - 14.0, chat_w - 28.0, input_h);
        // The "replying to" bar sits directly above the input.
        let replying = self.reply.as_ref().is_some_and(|r| r.buffer == self.app.active);
        self.reply_rect = if replying {
            Rect::new(sw + 14.0, self.input_rect.y - 34.0, chat_w - 28.0, 30.0)
        } else {
            Rect::new(sw + 14.0, self.input_rect.y, chat_w - 28.0, 0.0)
        };
        self.typing_rect = Rect::new(sw + 16.0, self.reply_rect.y - typing_h, chat_w - 32.0, typing_h);
        let chat_top = TOPIC_H;
        self.chat.rect = Rect::new(sw, chat_top, chat_w, self.typing_rect.y - chat_top);
        self.nicklist.rect = Rect::new(w - nick_w, chat_top, nick_w, h - chat_top);
    }

    #[allow(clippy::too_many_arguments)]
    fn chat_ctx<'a>(
        text: &'a Text,
        theme: &'a Theme,
        brushes: &'a mut Brushes,
        dc: &'a windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
        dgen: u64,
        app: &'a App,
        images: &'a std::rc::Rc<std::cell::RefCell<crate::images::ImageStore>>,
        anims: &'a crate::anim::Anims,
    ) -> Ctx<'a> {
        let cfg = &app.config;
        let a = &cfg.appearance;
        let net_previews = app.network_of(app.active).is_some_and(|n| n.cfg.previews);
        Ctx {
            text,
            anims,
            theme,
            brushes,
            dc,
            gfx_gen: dgen,
            ts_format: &a.timestamp_format,
            nick_column: a.nick_column,
            nick_column_auto: a.nick_column_auto,
            nick_column_chars: a.nick_column_width,
            colors: a.show_mirc_colors,
            colored_nicks: a.colored_nicks,
            twitch_colors: app.network_of(app.active).is_some_and(|n| n.cfg.twitch_colors),
            images,
            previews: cfg.previews.enabled && net_previews,
            preview_auto: cfg.previews.auto_load,
            allow_hosts: &cfg.previews.allow_hosts,
            replies: app.can_reply(app.active),
        }
    }

    fn paint(&mut self) {
        self.layout();
        self.sync_active();
        let (pw, ph) = self.client_size();
        let Some(mut gfx) = self.gfx.take() else { return };
        let dgen = gfx.generation;
        let r = gfx.frame(pw, ph, |dc| self.draw(dc, dgen));
        self.gfx = Some(gfx);
        if self.anims.moving() {
            // A button transition is running: next frame at ~60 fps.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_UI_ANIM, 16, None);
            }
        }
        if self.active_window && self.images.borrow().animating.get() {
            // Keep animated emotes moving (~25 fps) while visible and focused.
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_ANIM, 40, None);
            }
        }
        self.dispatch_media();
        if let Err(e) = r {
            tracing::error!("render failed: {e}");
        }
        if self.chat.wants_older {
            self.chat.wants_older = false;
            let id = self.app.active;
            self.app.request_older(id);
            self.after_update();
        }
    }

    /// Keeps widgets in sync with the active buffer.
    fn sync_active(&mut self) {
        let id = self.app.active;
        if self.shown_buffer != Some(id) {
            // Save the draft of the buffer we're leaving and restore the new one's.
            if let Some(prev) = self.shown_buffer {
                let draft = self.input.text().to_owned();
                if let Some(b) = self.app.buffer_mut(prev) {
                    b.input.draft = draft;
                    b.input.pos = None;
                }
            }
            let (marker, draft) =
                self.app.buffer(id).map(|b| (b.read_marker, b.input.draft.clone())).unwrap_or_default();
            self.chat.set_buffer(id, marker);
            self.input.set_text(&draft, None);
            self.shown_buffer = Some(id);
            self.nicklist_gen = (None, 0);
            self.nicklist.scroll = 0.0;
            self.update_title();
        }
        let dgen = self.app.buffer(id).map_or(0, |b| b.generation);
        if self.nicklist_gen != (Some(id), dgen) && self.show_nicklist {
            self.nicklist.refresh(&self.app, id);
            self.nicklist_gen = (Some(id), dgen);
        }
    }

    fn update_title(&self) {
        let (t, _) = self.app.topic_for(self.app.active);
        let title = HSTRING::from(format!("{t} — schwätz"));
        unsafe {
            let _ = SetWindowTextW(self.hwnd, &title);
        }
    }

    fn draw(&mut self, dc: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, dgen: u64) {
        // The reply bar belongs to one buffer; re-layout when switching in or out of it.
        if self.reply.as_ref().is_some_and(|r| r.buffer == self.app.active) != (self.reply_rect.h > 0.0) {
            self.layout();
        }
        self.anims.begin_frame();
        if self.form.is_none() {
            self.anims.forget_dialog();
        }
        let p = Painter::new(dc);
        let th = self.theme.clone();
        if self.mica {
            p.clear(rgba(0, 0, 0, 0.0));
            p.fill(self.sidebar.rect, th.backdrop);
        } else {
            p.clear(th.backdrop_opaque);
        }
        {
            let mut store = self.images.borrow_mut();
            if self.active_window {
                store.clock = self.anim_start.elapsed().as_millis() as u64;
            }
            store.animating.set(false);
            store.realize(dc, dgen);
        }
        self.sidebar.render(&p, &self.text, &th, &self.app, &self.anims);

        // Topic bar.
        let tr = self.topic_rect;
        p.fill(tr, th.topic_bg);
        p.line(tr.x, tr.bottom() - 0.5, tr.right(), tr.bottom() - 0.5, th.border, 1.0);
        let parts = self.app.topic_parts(self.app.active);
        let title = parts.title.clone();
        let f = &self.text.fonts;
        // Twitch channels: "Open stream" at the right end (with a red dot while live).
        self.stream_btn = None;
        let mut text_w = tr.w - 40.0;
        if self.app.twitch_stream_url(self.app.active).is_some() {
            let live = self.app.active_buffer().stream.as_ref().is_some_and(|s| s.live);
            let label = self.text.layout("Open stream", &f.ui_semibold, 200.0, 20.0);
            let lw = text::metrics(&label).width;
            let icon = self.text.layout("\u{E768}", &f.icons, 20.0, 20.0);
            let lead = if live { 14.0 } else { 0.0 };
            let w = 14.0 + lead + 18.0 + lw + 14.0;
            let r = Rect::new(tr.right() - 16.0 - w, tr.y + (tr.h - 32.0) / 2.0, w, 32.0);
            let c = crate::anim::Control::OpenStream;
            let face = crate::anim::button_face(&p, r, 6.0, th.badge_bg, &th, self.anims.hover(c), self.anims.press(c));
            let mut x = face.x + 14.0;
            if live {
                p.circle(x + 4.0, face.y + face.h / 2.0, 4.0, th.live);
                x += lead;
            }
            let im = text::metrics(&icon);
            p.text(&icon, x, face.y + (face.h - im.height) / 2.0, th.text);
            p.text(&label, x + 18.0, face.y + (face.h - text::metrics(&label).height) / 2.0, th.text);
            self.stream_btn = Some(r);
            text_w -= w + 16.0;
        }
        let tl = self.text.layout(&title, &f.title, text_w, 30.0);
        p.text(&tl, tr.x + 18.0, tr.y + 9.0, th.text);
        self.draw_topic_line(&p, &th, &parts, tr.x + 18.0, tr.y + 32.0, text_w);

        // Chat.
        let id = self.app.active;
        {
            let Ui { ref mut chat, ref text, ref mut brushes, ref app, ref images, ref anims, .. } = *self;
            let mut ctx = Ui::chat_ctx(text, &th, brushes, dc, dgen, app, images, anims);
            if let Some(b) = app.buffer(id) {
                chat.render(&p, &mut ctx, b);
            }
        }

        // Typing indicator.
        let typing = self.app.buffer(id).map(|b| b.typing_nicks(now()).join(", ")).unwrap_or_default();
        let chat_bottom = Rect::new(
            self.chat.rect.x,
            self.chat.rect.bottom(),
            self.chat.rect.w,
            self.win_rect.h - self.chat.rect.bottom(),
        );
        p.fill(chat_bottom, th.chat_bg);
        if !typing.is_empty() {
            let verb = if typing.contains(',') { "are" } else { "is" };
            let l = self.text.layout(&format!("{typing} {verb} typing…"), &f.ui_small, self.typing_rect.w, 20.0);
            p.text(&l, self.typing_rect.x, self.typing_rect.y + 2.0, th.text_dim);
        }

        // Pending reply.
        let replying = self.reply.as_ref().filter(|r| r.buffer == id && self.reply_rect.h > 0.0);
        if let Some(r) = replying {
            let rr = self.reply_rect;
            p.fill_round(rr, 8.0, th.panel_bg);
            p.fill(Rect::new(rr.x + 8.0, rr.y + 7.0, 3.0, rr.h - 14.0), th.accent);
            let head = self.text.layout(&format!("↩  Replying to {}", r.nick), &f.ui_semibold, rr.w - 60.0, 20.0);
            let hw = text::metrics(&head).width;
            p.text(&head, rr.x + 20.0, rr.y + 7.0, th.accent);
            let ex = self.text.layout(&r.excerpt, &f.ui, (rr.w - hw - 72.0).max(1.0), 20.0);
            p.text(&ex, rr.x + 32.0 + hw, rr.y + 7.0, th.text_dim);
            let cr = self.reply_close_rect();
            let rc = crate::anim::Control::ReplyClose;
            let (h, pr) = (self.anims.hover(rc), self.anims.press(rc));
            if h + pr > 0.0 {
                p.fill_round(cr.inset(pr * 1.5, pr * 1.5), 6.0, with_alpha(th.text, 0.08 * h + 0.08 * pr));
            }
            let x = self.text.layout("✕", &f.ui, cr.w, cr.h);
            let xw = text::metrics(&x).width;
            p.text(&x, cr.x + (cr.w - xw) / 2.0, cr.y + 4.0, crate::anim::mix(th.text_dim, th.text, h));
        }

        // Input box.
        let ir = self.input_rect;
        p.fill_round(ir, 10.0, th.input_bg);
        p.stroke_round(ir, 10.0, th.border, 1.0);
        let inner = ir.inset(14.0, 11.0);
        let layout = self.input.layout(&self.text, &f.chat, inner.w).clone();
        let content_h = text::metrics(&layout).height;
        let (cx, cy, ch) = self.input.caret();
        let scroll_y = (cy + ch - inner.h).max(0.0).min((content_h - inner.h).max(0.0));
        p.clip(inner.inset(-2.0, 0.0));
        if self.input.is_empty() {
            let b = self.app.active_buffer();
            let reply_nick = replying.map(|r| r.nick.clone());
            let ph = match b.kind {
                _ if let Some(n) = reply_nick => format!("Reply to {n}"),
                BufferKind::Channel | BufferKind::Query => format!("Message {}", b.name),
                _ => "Type a command, e.g. /connect irc.libera.chat".to_owned(),
            };
            let l = self.text.layout(&ph, &f.chat, inner.w, 40.0);
            p.text(&l, inner.x, inner.y, th.text_dim);
        }
        for (sx, sy, sw, sh) in self.input.selection_rects() {
            p.fill(Rect::new(inner.x + sx, inner.y + sy - scroll_y, sw, sh), th.selection);
        }
        p.text(&layout, inner.x, inner.y - scroll_y, th.text);
        if self.caret_on && self.overlay.is_none() && self.active_window {
            p.fill(Rect::new(inner.x + cx, inner.y + cy - scroll_y, 1.5, ch), th.accent);
        }
        p.unclip();
        self.set_ime_pos(inner.x + cx, inner.y + cy - scroll_y + ch);

        if self.show_nicklist {
            self.nicklist.render(&p, &self.text, &th, self.app.config.appearance.colored_nicks);
        }
        // Sidebar edge.
        p.line(self.sidebar_w - 0.5, 0.0, self.sidebar_w - 0.5, self.win_rect.h, th.border, 1.0);

        // Emote completion, rising from the `:` in the input.
        if let Some(mut c) = self.completion.take() {
            let map = text::U16Map::new(self.input.text());
            let (colon_x, _, _) = text::caret_pos(&layout, map.to_u16(c.colon as u32));
            self.draw_completion(&p, &th, &mut c, inner.x + colon_x);
            self.completion = Some(c);
        }

        if let Some((code, url, anchor)) =
            self.tooltip.as_ref().filter(|_| self.overlay.is_none() && self.form.is_none())
        {
            self.draw_emote_tip(&p, &th, code, url, *anchor);
        }
        if let Some(f) = self.form.as_mut() {
            if let crate::form::FormKind::Network { original } = &f.kind {
                let net = original.as_deref().and_then(|n| self.app.network_by_name(n));
                let (caption, note) = twitch_account_button(&self.app, net);
                f.set_button("twitch_signin", caption, &note);
            }
            f.render(&p, &self.text, &th, self.win_rect, self.caret_on, &self.anims);
        }
        // Overlays (confirmations) sit on top of an open dialog.
        if let Some(o) = self.overlay.as_mut() {
            o.render(&p, &self.text, &th, &self.app, self.win_rect, self.caret_on, &self.anims);
        }
        let _ = draw_field;
        let _ = with_alpha;
    }

    /// Tooltip above a hovered emote: an enlarged image and its code.
    fn draw_emote_tip(&self, p: &Painter, th: &Theme, code: &str, url: &str, anchor: Rect) {
        let f = &self.text.fonts;
        let l = self.text.layout(code, &f.ui_semibold, 400.0, 20.0);
        let tw = text::metrics(&l).width;
        let img = self.images.borrow().bitmap(url);
        let (iw, ih) = match &img {
            Some(b) => {
                let s = unsafe { b.GetSize() };
                let h = 56.0f32;
                ((h * s.width / s.height.max(1.0)).min(168.0), h)
            }
            None => (0.0, 0.0),
        };
        let w = tw.max(iw) + 20.0;
        let h = ih + if ih > 0.0 { 8.0 } else { 0.0 } + 18.0 + 14.0;
        let x = (anchor.x + anchor.w / 2.0 - w / 2.0).clamp(4.0, (self.win_rect.w - w - 4.0).max(4.0));
        // Above the emote unless that leaves the chat area.
        let y = if anchor.y - h - 6.0 >= self.chat.rect.y { anchor.y - h - 6.0 } else { anchor.bottom() + 6.0 };
        let r = Rect::new(x, y, w, h);
        p.fill_round(r, 8.0, th.panel_bg);
        p.stroke_round(r, 8.0, th.border, 1.0);
        if let Some(b) = img {
            p.bitmap(&b.cast().unwrap(), Rect::new(x + (w - iw) / 2.0, y + 7.0, iw, ih), 1.0);
        }
        p.text(&l, x + (w - tw) / 2.0, y + h - 25.0, th.text);
    }

    fn set_ime_pos(&self, x: f32, y: f32) {
        unsafe {
            let himc = ImmGetContext(self.hwnd);
            if himc.is_invalid() {
                return;
            }
            let pt = POINT { x: (x * self.scale) as i32, y: (y * self.scale) as i32 };
            let cf = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: POINT { x: pt.x, y: pt.y - (self.text.fonts.line_height * self.scale) as i32 },
                ..Default::default()
            };
            let _ = ImmSetCompositionWindow(himc, &cf);
            let cand = CANDIDATEFORM { dwIndex: 0, dwStyle: CFS_CANDIDATEPOS, ptCurrentPos: pt, ..Default::default() };
            let _ = ImmSetCandidateWindow(himc, &cand);
            let _ = ImmReleaseContext(self.hwnd, himc);
        }
    }

    // ----- model plumbing ------------------------------------------------------------------------

    fn process_net(&mut self) {
        let events: Vec<NetEvent> = self.net.drain().collect();
        if events.is_empty() {
            return;
        }
        let t = now();
        for ev in events {
            if let (Some(host), NetEvent::Line { id, msg }) = (self.services.scripts.as_mut(), &ev) {
                let name = self.app.network(*id).map(|n| n.display_name().to_owned()).unwrap_or_default();
                host.on_raw(&mut self.app, &name, msg);
            }
            self.app.on_net_event(ev, t);
        }
        self.background_update = true;
        self.after_update();
        self.background_update = false;
    }

    /// Forwards network commands, runs effects and schedules a repaint.
    fn after_update(&mut self) {
        if self.restore_active.is_some() {
            self.check_restore_active();
        }
        // Emote lists arrive asynchronously: a `:word` typed before they did gets its list now.
        if self.form.is_none() && self.overlay.is_none() {
            self.refresh_completion();
        }
        for _ in 0..4 {
            let lines = self.app.take_new_lines();
            if !lines.is_empty() {
                if let Some(host) = self.services.scripts.as_mut() {
                    host.on_lines(&mut self.app, &lines);
                }
                self.announce(&lines);
            }
            for cmd in self.app.take_net_commands() {
                self.net.send(cmd);
            }
            let effects = self.app.take_effects();
            if effects.is_empty() {
                break;
            }
            for e in effects {
                self.effect(e);
            }
        }
        let d = self.app.take_dirty();
        if d.sidebar || d.topic {
            self.update_title();
        }
        if d.lines || d.sidebar || d.nicklist || d.topic || d.input {
            if d.nicklist {
                self.nicklist_gen = (None, 0);
            }
            if self.background_update {
                self.invalidate_soon();
            } else {
                self.invalidate();
            }
        }
    }

    /// Coalesces repaints caused by network traffic to at most ~20 per second.
    fn invalidate_soon(&mut self) {
        if !self.paint_pending {
            self.paint_pending = true;
            unsafe {
                SetTimer(Some(self.hwnd), TIMER_PAINT, 50, None);
            }
        }
    }

    fn effect(&mut self, e: Effect) {
        match e {
            Effect::Notify { title, body, buffer } => {
                self.notify_buffer = Some(buffer);
                self.tray.notify(&title, &body);
            }
            Effect::FlashTaskbar => unsafe {
                let fi = FLASHWINFO {
                    cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                    hwnd: self.hwnd,
                    dwFlags: FLASHW_TRAY | FLASHW_TIMERNOFG,
                    uCount: 3,
                    dwTimeout: 0,
                };
                let _ = FlashWindowEx(&fi);
            },
            Effect::ChannelList(net) => {
                match self.overlay.as_mut() {
                    Some(o @ Overlay::ChannelList { .. }) => o.refresh(&self.app),
                    _ => {
                        let mut o = Overlay::channel_list(net);
                        o.refresh(&self.app);
                        self.overlay = Some(o);
                    }
                }
                self.invalidate();
            }
            Effect::ZncNetworks { network, names } => {
                let existing: Vec<String> =
                    self.app.networks.values().filter_map(|n| n.cfg.znc_network.clone()).collect();
                let names: Vec<String> = names.into_iter().filter(|n| !existing.contains(n)).collect();
                if names.is_empty() {
                    let sb = self.app.networks[&network].server_buffer;
                    self.app.print(sb, LineKind::Status, "", "All ZNC networks are already configured.");
                } else {
                    self.overlay = Some(Overlay::Confirm {
                        title: "Add ZNC networks?".into(),
                        body: format!(
                            "Your bouncer has {} more network(s): {}. Add them as connections using the same login?",
                            names.len(),
                            names.join(", ")
                        ),
                        yes: "Add networks".into(),
                        action: ConfirmAction::AddZncNetworks { net: network, names },
                    });
                }
            }
            Effect::BouncerNetworks(net) => {
                let Some(n) = self.app.network(net) else { return };
                let sb = n.server_buffer;
                let list: Vec<String> = n
                    .bouncer_networks
                    .iter()
                    .map(|(id, attrs)| {
                        let name = attrs.iter().find(|(k, _)| k == "name").map(|(_, v)| v.as_str()).unwrap_or("?");
                        let state = attrs.iter().find(|(k, _)| k == "state").map(|(_, v)| v.as_str()).unwrap_or("?");
                        format!("{name} (id {id}, {state})")
                    })
                    .collect();
                self.app.print(sb, LineKind::Status, "", &format!("Bouncer networks: {}", list.join(", ")));
            }
            Effect::ConfirmPaste { buffer, text, lines } => {
                let preview: String = text.lines().take(3).collect::<Vec<_>>().join(" ⏎ ");
                self.overlay = Some(Overlay::Confirm {
                    title: format!("Send {lines} lines?"),
                    body: format!(
                        "You are about to send {lines} lines: “{}…”",
                        preview.chars().take(160).collect::<String>()
                    ),
                    yes: "Send".into(),
                    action: ConfirmAction::Paste { buffer, text },
                });
                self.invalidate();
            }
            Effect::SaveConfig => {
                if let Err(e) = self.app.config.save(&self.paths.config_file()) {
                    let sb = self.app.status_buffer;
                    self.app.print(sb, LineKind::Error, "", &format!("Could not save settings: {e}"));
                }
            }
            Effect::ConfigChanged => self.apply_appearance(),
            Effect::OpenUrl(u) => win::open_url(&u),
            Effect::TwitchLive(req) => {
                self.run_worker("twitch-live", move || WorkerResult::Live(schwaetz_core::helix::check(req)))
            }
            Effect::TwitchEmotes(req) => {
                self.run_worker("twitch-emotes", move || WorkerResult::Emotes(schwaetz_core::helix::fetch_emotes(req)))
            }
            Effect::TwitchAuth { network, request } => self.run_worker("twitch-auth", move || {
                let response = schwaetz_core::twitch_auth::execute(&request);
                WorkerResult::Auth(network, request, response)
            }),
            Effect::ReloadScripts => {
                if let Some(h) = self.services.scripts.as_mut() {
                    h.reload(&mut self.app);
                } else {
                    let id = self.app.active;
                    self.app.print(id, LineKind::Error, "", "Scripting is not available in this build.");
                }
            }
            Effect::Quit => self.begin_quit(),
            Effect::OpenSettings => {
                self.open_settings();
                self.invalidate();
            }
            Effect::OpenNetwork(name) => {
                let cfg = name.and_then(|n| self.app.config.network(&n).cloned());
                self.form = Some(Box::new(crate::form::Form::network(cfg.as_ref())));
                self.invalidate();
            }
        }
    }

    fn save_session(&self) {
        let mut s = crate::session::Session { sidebar_width: Some(self.sidebar_w), ..Default::default() };
        s.capture_window(self.hwnd);
        let b = self.app.active_buffer();
        if let Some(net) = b.network.and_then(|n| self.app.network(n)) {
            s.active = Some((net.display_name().to_owned(), b.name.clone()));
        }
        s.save(&self.paths.session_file());
    }

    /// Reselects the buffer that was active on exit once it has been recreated.
    fn check_restore_active(&mut self) {
        let Some((net, buf)) = self.restore_active.clone() else { return };
        let Some(id) = self.app.network_by_name(&net) else {
            self.restore_active = None;
            return;
        };
        let target = if buf.eq_ignore_ascii_case(&net) {
            self.app.network(id).map(|n| n.server_buffer)
        } else {
            self.app.find_buffer(id, &buf)
        };
        if let Some(b) = target {
            self.restore_active = None;
            self.app.switch_to(b);
        }
    }

    fn begin_quit(&mut self) {
        if self.quitting {
            return;
        }
        self.quitting = true;
        self.save_session();
        self.app.quit_all(None);
        for cmd in self.app.take_net_commands() {
            self.net.send(cmd);
        }
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }

    // ----- input ---------------------------------------------------------------------------------

    fn submit(&mut self) {
        let text = self.input.text().to_owned();
        if text.trim().is_empty() {
            return;
        }
        let id = self.app.active;
        self.input.clear();
        if let Some(b) = self.app.buffer_mut(id) {
            b.input.draft.clear();
        }
        let reply = self.reply.take_if(|r| r.buffer == id);
        match reply {
            // Commands keep their usual meaning; the reply stays pending for the next message.
            Some(r) if text.starts_with('/') && !text.starts_with("//") => {
                self.reply = Some(r);
                self.run_input(id, &text);
            }
            Some(r) => {
                let consumed = match self.services.scripts.as_mut() {
                    Some(h) => h.on_input(&mut self.app, id, &text),
                    None => false,
                };
                if !consumed {
                    let body = text.strip_prefix('/').filter(|t| t.starts_with('/')).unwrap_or(&text);
                    self.app.reply(id, &r.msgid, body);
                }
            }
            None => self.run_input(id, &text),
        }
        if self.last_typing_sent > 0 {
            self.last_typing_sent = 0;
        }
        self.chat.scroll_to_bottom();
        self.after_update();
    }

    /// Asks before removing a network (from its dialog or the sidebar menu).
    fn confirm_remove_network(&mut self, name: String) {
        let connected = self
            .app
            .network_by_name(&name)
            .and_then(|id| self.app.network(id))
            .is_some_and(|n| n.conn != ConnState::Disconnected);
        let mut body =
            String::from("This deletes its settings, its join list and the passwords and tokens stored for it.");
        if connected {
            body.push_str(" It will be disconnected.");
        }
        body.push_str(" This cannot be undone.");
        self.overlay = Some(Overlay::Confirm {
            title: format!("Remove network \"{name}\"?"),
            body,
            yes: "Remove network".into(),
            action: ConfirmAction::RemoveNetwork { name },
        });
        self.invalidate();
    }

    /// Removes a network: disconnects it, forgets its buffers and settings, signs out of Twitch
    /// (revoking the token) and deletes its stored secrets.
    fn remove_network(&mut self, name: &str) {
        use schwaetz_core::secrets::{self, SecretKind};
        if let Some(id) = self.app.network_by_name(name) {
            self.app.twitch_sign_out(id);
            self.app.remove_network(id);
        }
        for kind in [
            SecretKind::Sasl,
            SecretKind::ServerPassword,
            SecretKind::TwitchToken,
            SecretKind::TwitchApi,
            SecretKind::TwitchOAuth,
        ] {
            secrets::delete(name, kind);
        }
        self.app.config.networks.retain(|c| c.name != name);
        let _ = self.app.config.save(&self.paths.config_file());
        // The dialog of the removed network (if open) goes away with it.
        if matches!(self.form.as_deref().map(|f| &f.kind), Some(crate::form::FormKind::Network { original: Some(n) }) if n == name)
        {
            self.form = None;
        }
        self.after_update();
    }

    /// Middle-click on a sidebar row, like on a browser tab: closes the channel right away (leaving
    /// it first if joined).
    fn middle_click_buffer(&mut self, id: BufferId) {
        let Some(b) = self.app.buffer(id) else { return };
        if b.kind != BufferKind::Channel {
            return;
        }
        self.app.input(id, "/close");
        self.after_update();
        self.invalidate();
    }

    /// The topic bar's second line: `lead` and `tail` (room modes, live status, game, viewers) always
    /// show; only `body` (topic or stream title) is shortened with an ellipsis to fit `width`.
    fn draw_topic_line(&self, p: &Painter, th: &Theme, parts: &schwaetz_core::TopicParts, x: f32, y: f32, width: f32) {
        let f = &self.text.fonts;
        let clean = |s: &str| schwaetz_proto::format::strip(s).replace(['\n', '\r'], " ");
        let (mut lead, body, tail) = (clean(&parts.lead), clean(&parts.body), clean(&parts.tail));
        let measure = |s: &str| {
            if s.is_empty() {
                0.0
            } else {
                text::metrics(&self.text.layout(s, &f.ui, 10_000.0, 20.0)).widthIncludingTrailingWhitespace
            }
        };
        let tail_w = measure(&tail);
        // What fits of the body: all or part of it (shortened with an ellipsis), a lone
        // ellipsis, or nothing, in which case the dash that introduced it goes too.
        let room = width - measure(&lead) - tail_w;
        let body_shown = if body.is_empty() {
            None
        } else if room >= 28.0 {
            Some(body.as_str())
        } else if room >= measure("…") {
            Some("…")
        } else {
            if let Some(stripped) = lead.strip_suffix(" — ") {
                lead = stripped.to_owned();
            }
            None
        };
        let mut cx = x;
        if !lead.is_empty() {
            // The fixed parts win; if even they do not fit, the lead is shortened too.
            let l = self.text.layout(&lead, &f.ui, (width - tail_w).max(1.0), 20.0);
            p.text(&l, cx, y, th.text_dim);
            cx += text::metrics(&l).widthIncludingTrailingWhitespace.min(width - tail_w);
        }
        if let Some(b) = body_shown {
            let room = width - (cx - x) - tail_w;
            let l = self.text.layout(b, &f.ui, room.max(1.0), 20.0);
            p.text(&l, cx, y, th.text_dim);
            cx += text::metrics(&l).width.min(room.max(0.0));
        }
        if !tail.is_empty() {
            let l = self.text.layout(&tail, &f.ui, width, 20.0);
            p.text(&l, cx, y, th.text_dim);
        }
    }

    /// Opens the settings dialog (the Scripts page lists the script host's files).
    fn open_settings(&mut self) {
        let dir = self.paths.scripts_dir().display().to_string();
        let infos = self.services.scripts.as_ref().map(|h| h.list(&self.app));
        let scripts = infos.as_deref().map(|i| (i, dir.as_str()));
        self.form = Some(Box::new(crate::form::Form::settings(&self.app.config, scripts)));
    }

    /// Runs blocking work (HTTPS) on its own thread; the result comes back through `WM_APP_LIVE`.
    fn run_worker(&self, name: &str, job: impl FnOnce() -> WorkerResult + Send + 'static) {
        let tx = self.workers.0.clone();
        let hwnd = self.hwnd.0 as isize;
        let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
            let _ = tx.send(job());
            unsafe {
                let _ = PostMessageW(Some(HWND(hwnd as *mut _)), WM_APP_LIVE, WPARAM(0), LPARAM(0));
            }
        });
    }

    /// Sends image/preview requests collected while laying out lines.
    fn dispatch_media(&mut self) {
        let wants = std::mem::take(&mut self.images.borrow_mut().wants);
        for (url, preview) in wants {
            if self.media.is_pending(&url) {
                continue;
            }
            let kind = if preview {
                self.preview_pending.insert(url.clone());
                schwaetz_media::Kind::Preview { max_dim: (420.0 * self.scale) as u32 }
            } else {
                schwaetz_media::Kind::Image { max_dim: (420.0 * self.scale) as u32 }
            };
            self.media.request(&url, kind);
        }
    }

    fn process_media(&mut self) {
        let results = self.media.drain();
        if results.is_empty() {
            return;
        }
        let mut failures = Vec::new();
        {
            let mut store = self.images.borrow_mut();
            for r in results {
                match r {
                    schwaetz_media::MediaResult::Image { url, width, height, frames } => {
                        if self.preview_pending.remove(&url) {
                            store.previews.insert(url.clone(), crate::images::Preview::Image);
                        }
                        store.insert_frames(url, width, height, frames);
                    }
                    schwaetz_media::MediaResult::Page { url, meta } => {
                        self.preview_pending.remove(&url);
                        store.previews.insert(url, crate::images::Preview::Page(meta));
                    }
                    schwaetz_media::MediaResult::Failed { url, error } => {
                        if self.app.rawlog {
                            failures.push(format!("media: {url}: {error}"));
                        }
                        if self.preview_pending.remove(&url) {
                            store.previews.insert(url.clone(), crate::images::Preview::Failed);
                        }
                        store.insert_failed(url);
                    }
                }
            }
        }
        if !failures.is_empty() {
            let b = self.app.ensure_special("raw log");
            for f in failures {
                self.app.print(b, LineKind::Error, "", &f);
            }
        }
        // Image sizes change line heights: re-layout.
        self.chat.invalidate_styles();
        self.invalidate();
    }

    /// Input from the user (typed or forwarded): scripts first, then the model.
    fn run_input(&mut self, buffer: BufferId, text: &str) {
        let consumed = match self.services.scripts.as_mut() {
            Some(h) => h.on_input(&mut self.app, buffer, text),
            None => false,
        };
        if !consumed {
            self.app.input(buffer, text);
        }
    }

    /// Arms a reply to a chat line of the active buffer (from its hover button).
    fn start_reply(&mut self, line: u64) {
        let id = self.app.active;
        let Some(l) = self.app.buffer(id).and_then(|b| b.lines.iter().find(|l| l.id == line)) else { return };
        let Some(msgid) = l.msgid() else { return };
        let text: String = schwaetz_proto::format::strip(&l.text).split_whitespace().collect::<Vec<_>>().join(" ");
        let excerpt =
            if text.chars().count() > 120 { text.chars().take(117).chain("…".chars()).collect() } else { text };
        self.reply =
            Some(ReplyTarget { buffer: id, msgid: msgid.to_owned(), nick: l.display_nick().to_owned(), excerpt });
        self.layout();
        self.invalidate();
        unsafe {
            let _ = SetFocus(Some(self.hwnd));
        }
    }

    fn reply_close_rect(&self) -> Rect {
        let r = self.reply_rect;
        Rect::new(r.right() - 30.0, r.y + 3.0, 24.0, 24.0)
    }

    fn typed(&mut self) {
        let t = now();
        let text = self.input.text();
        if text.is_empty() || text.starts_with('/') {
            if self.last_typing_sent > 0 {
                self.last_typing_sent = 0;
                let id = self.app.active;
                self.app.typing(id, "done");
                self.after_update();
            }
            return;
        }
        if t - self.last_typing_sent > 3000 {
            self.last_typing_sent = t;
            let id = self.app.active;
            self.app.typing(id, "active");
            self.after_update();
        }
    }

    fn switch_relative(&mut self, delta: i32) {
        let order = self.app.sidebar_order();
        if order.is_empty() {
            return;
        }
        let pos = order.iter().position(|b| *b == self.app.active).unwrap_or(0) as i32;
        let next = order[((pos + delta).rem_euclid(order.len() as i32)) as usize];
        self.switch_to(next);
    }

    fn switch_to(&mut self, id: BufferId) {
        self.app.switch_to(id);
        self.after_update();
    }

    fn next_activity(&mut self) {
        let order = self.app.sidebar_order();
        let best = order
            .iter()
            .filter(|id| **id != self.app.active)
            .filter_map(|id| self.app.buffer(*id).map(|b| (b.activity, *id)))
            .filter(|(a, _)| *a > schwaetz_core::Activity::Events)
            .max_by_key(|(a, _)| *a);
        if let Some((_, id)) = best {
            self.switch_to(id);
        }
    }

    /// Puts a finished chat selection on the clipboard (`general.copy_on_select`).
    fn copy_on_select(&mut self) {
        if !self.app.config.general.copy_on_select || self.chat.selection.is_none_or(|(a, b)| a == b) {
            return;
        }
        let b = self.app.active_buffer();
        if let Some(t) = self.chat.selected_text(b, &self.app.config.appearance.timestamp_format) {
            win::set_clipboard(self.hwnd, &t);
        }
    }

    fn copy(&mut self) -> bool {
        if self.input.has_selection() {
            win::set_clipboard(self.hwnd, self.input.selected_text());
            return true;
        }
        let b = self.app.active_buffer();
        if let Some(t) = self.chat.selected_text(b, &self.app.config.appearance.timestamp_format) {
            win::set_clipboard(self.hwnd, &t);
            return true;
        }
        false
    }

    fn chat_scroll(&mut self, dy: f32) {
        let Some(mut gfx) = self.gfx.take() else { return };
        // Scrolling needs line heights, which need a device context for brushes.
        let dgen = gfx.generation;
        let (pw, ph) = self.client_size();
        let _ = gfx.frame(pw, ph, |dc| {
            let id = self.app.active;
            let Ui { ref mut chat, ref text, ref mut brushes, ref app, ref theme, ref images, ref anims, .. } = *self;
            let th = theme.clone();
            let mut ctx = Ui::chat_ctx(text, &th, brushes, dc, dgen, app, images, anims);
            if let Some(b) = app.buffer(id) {
                chat.scroll(&mut ctx, b, dy);
            }
            self.draw(dc, dgen);
        });
        self.gfx = Some(gfx);
        if self.chat.wants_older {
            self.chat.wants_older = false;
            let id = self.app.active;
            self.app.request_older(id);
            self.after_update();
        }
    }

    fn form_action(&mut self, action: crate::form::FormAction) {
        use crate::form::{FormAction, FormKind};
        let Some(form) = self.form.as_mut() else { return };
        match action {
            FormAction::None => {}
            FormAction::Cancel => self.form = None,
            FormAction::Save => match form.kind.clone() {
                FormKind::Settings => {
                    let mut c = self.app.config.clone();
                    match form.apply_settings(&mut c) {
                        Err(e) => form.set_error(e),
                        Ok(()) => {
                            self.app.config = c;
                            self.app.apply_config();
                            if let Err(e) = self.app.config.save(&self.paths.config_file()) {
                                form.error = Some(format!("Could not save: {e}"));
                                return;
                            }
                            // Scripts switched on or off start or stop now.
                            if let Some(h) = self.services.scripts.as_mut() {
                                h.refresh(&mut self.app);
                            }
                            self.form = None;
                            self.after_update();
                        }
                    }
                }
                FormKind::Network { original } => {
                    let base = original.as_deref().and_then(|o| self.app.config.network(o).cloned());
                    match form.to_network(base.as_ref()) {
                        Err(e) => form.set_error(e),
                        Ok((cfg, secrets)) => {
                            use schwaetz_core::secrets::{SecretKind, get, set};
                            if let Some(old) = original.as_deref().filter(|o| *o != cfg.name) {
                                // Renamed: carry stored secrets over.
                                for k in [
                                    SecretKind::Sasl,
                                    SecretKind::ServerPassword,
                                    SecretKind::TwitchToken,
                                    SecretKind::TwitchApi,
                                    SecretKind::TwitchOAuth,
                                ] {
                                    if let Some(v) = get(old, k) {
                                        set(&cfg.name, k, &v);
                                    }
                                }
                            }
                            for (k, v) in &secrets {
                                set(&cfg.name, *k, v);
                            }
                            let id = self.app.upsert_network(original.as_deref(), cfg);
                            // Drawn lines may depend on network settings (Twitch name colors).
                            self.chat.invalidate_styles();
                            if let Some(n) = self.app.network(id) {
                                let sb = n.server_buffer;
                                self.app.switch_to(sb);
                            }
                            self.form = None;
                            self.after_update();
                        }
                    }
                }
            },
            FormAction::Button("twitch_signin") => {
                let net = match &form.kind {
                    FormKind::Network { original: Some(name) } => self.app.network_by_name(name),
                    _ => None,
                };
                let state = net.and_then(|n| self.app.twitch_auth(n)).map(|a| (a.signing_in(), a.login().is_some()));
                match (net, state) {
                    (Some(net), Some((true, _))) => self.app.twitch_cancel_sign_in(net),
                    (Some(net), Some((_, true))) => self.app.twitch_sign_out(net),
                    (Some(net), Some(_)) => self.app.twitch_sign_in(net),
                    _ => form.set_error("Save this network with the type Twitch chat first.".into()),
                }
                self.after_update();
            }
            FormAction::Button("scripts_folder") => {
                let dir = self.paths.scripts_dir();
                let _ = std::fs::create_dir_all(&dir);
                win::open_folder(&dir);
            }
            FormAction::Button("scripts_reload") => {
                if let Some(h) = self.services.scripts.as_mut() {
                    h.reload(&mut self.app);
                    form.set_scripts(&h.list(&self.app));
                    self.anims.forget_dialog();
                }
                self.after_update();
            }
            FormAction::Button("scripts_examples") => {
                if let Some(h) = self.services.scripts.as_mut() {
                    match h.install_examples() {
                        Ok(added) => {
                            // New examples start switched off; the user decides what runs.
                            for name in &added {
                                if self.app.config.scripts.is_enabled(name) {
                                    self.app.config.scripts.disabled.push(name.clone());
                                }
                            }
                            if !added.is_empty() {
                                let _ = self.app.config.save(&self.paths.config_file());
                            }
                            h.refresh(&mut self.app);
                            form.set_scripts(&h.list(&self.app));
                            self.anims.forget_dialog();
                            let note = match added.len() {
                                0 => "All examples are already there".to_owned(),
                                n => format!("Added {n} example(s), switched off"),
                            };
                            form.set_button("scripts_examples", "Add example scripts", &note);
                        }
                        Err(e) => form.set_error(format!("Could not add the examples: {e}")),
                    }
                }
            }
            FormAction::ScriptSwitch { name, enabled } => {
                // Script switches apply (and are saved) right away; the row shows the result.
                let disabled = &mut self.app.config.scripts.disabled;
                disabled.retain(|n| !n.eq_ignore_ascii_case(&name));
                if !enabled {
                    disabled.push(name);
                }
                if let Err(e) = self.app.config.save(&self.paths.config_file()) {
                    form.set_error(format!("Could not save: {e}"));
                }
                if let Some(h) = self.services.scripts.as_mut() {
                    h.refresh(&mut self.app);
                    form.set_scripts(&h.list(&self.app));
                }
                self.after_update();
            }
            FormAction::Button(_) => {}
            FormAction::Delete => {
                if let FormKind::Network { original: Some(name) } = form.kind.clone() {
                    self.confirm_remove_network(name);
                }
            }
        }
        self.invalidate();
    }

    fn key_down(&mut self, vk: u16) -> bool {
        if vk == VK_ESCAPE.0 && self.sidebar.drag.is_some() {
            // Cancel rearranging a channel.
            self.sidebar.drag = None;
            self.drag = Drag::None;
            self.invalidate();
            return true;
        }
        let ctrl = win::key_down(VK_CONTROL.0);
        let shift = win::key_down(VK_SHIFT.0);
        let alt = win::key_down(VK_MENU.0);
        let v = VIRTUAL_KEY(vk);

        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            let action = f.key(v, ctrl, shift, self.hwnd);
            self.form_action(action);
            self.caret_on = true;
            self.invalidate();
            return true;
        }
        if ctrl && vk == 0xBC {
            // Ctrl+, opens the settings.
            self.open_settings();
            self.invalidate();
            return true;
        }

        if self.overlay.is_some() {
            return self.overlay_key(v, ctrl, shift);
        }
        if self.completion.is_some() && self.completion_key(v) {
            self.invalidate();
            return true;
        }
        let lh = self.text.fonts.line_height;
        match v {
            VK_RETURN if shift => self.input.insert("\n"),
            VK_RETURN => self.submit(),
            VK_TAB if ctrl => self.switch_relative(if shift { -1 } else { 1 }),
            VK_TAB => {
                let id = self.app.active;
                let (t, c) = (self.input.text().to_owned(), self.input.cursor);
                if let Some((nt, nc)) = self.app.complete(id, &t, c, shift) {
                    self.input.set_text(&nt, Some(nc));
                }
            }
            VK_UP if alt => self.switch_relative(-1),
            VK_DOWN if alt => self.switch_relative(1),
            VK_UP | VK_DOWN => {
                let down = v == VK_DOWN;
                if ctrl {
                    self.chat_scroll(if down { -lh } else { lh });
                } else if !self.input.move_v(down, shift) && !shift {
                    let id = self.app.active;
                    let cur = self.input.text().to_owned();
                    let entry = self.app.buffer_mut(id).and_then(|b| {
                        if down { b.input.newer().map(str::to_owned) } else { b.input.older(&cur).map(str::to_owned) }
                    });
                    if let Some(e) = entry {
                        self.input.set_text(&e, None);
                    }
                }
            }
            VK_PRIOR if ctrl => self.switch_relative(-1),
            VK_NEXT if ctrl => self.switch_relative(1),
            VK_PRIOR => self.chat_scroll(self.chat.rect.h * 0.85),
            VK_NEXT => self.chat_scroll(-self.chat.rect.h * 0.85),
            VK_END if ctrl && self.input.is_empty() => self.chat.scroll_to_bottom(),
            VK_LEFT => self.input.move_h(false, ctrl, shift),
            VK_RIGHT => self.input.move_h(true, ctrl, shift),
            VK_HOME => self.input.home(shift, ctrl),
            VK_END => self.input.end(shift, ctrl),
            VK_BACK => self.input.backspace(ctrl),
            VK_DELETE => self.input.delete(ctrl),
            VK_ESCAPE => {
                if self.reply.take_if(|r| r.buffer == self.app.active).is_some() {
                    // Cancelled the reply.
                } else if self.chat.selection.take().is_none() {
                    self.chat.scroll_to_bottom();
                }
            }
            VK_F6 => self.next_activity(),
            _ if alt && (0x30..=0x39).contains(&vk) => {
                let n = if vk == 0x30 { 9 } else { (vk - 0x31) as usize };
                if let Some(id) = self.app.sidebar_order().get(n) {
                    self.switch_to(*id);
                }
            }
            _ if alt && vk == b'A' as u16 => self.next_activity(),
            _ if ctrl => match vk as u8 {
                b'A' => self.input.select_all(),
                b'C' => {
                    self.copy();
                }
                b'X' => {
                    if self.copy() && self.input.has_selection() {
                        self.input.backspace(false);
                    }
                }
                b'V' => {
                    if let Some(t) = win::get_clipboard(self.hwnd) {
                        self.input.insert(&t);
                    }
                }
                b'Z' if shift => self.input.redo(),
                b'Z' => self.input.undo(),
                b'Y' => self.input.redo(),
                b'B' => self.input.toggle_format('\x02'),
                b'I' => self.input.toggle_format('\x1d'),
                b'U' => self.input.toggle_format('\x1f'),
                b'K' => self.input.toggle_format('\x03'),
                b'R' => self.input.toggle_format('\x16'),
                b'O' => self.input.toggle_format('\x0f'),
                b'J' | b'P' => {
                    self.overlay = Some(Overlay::quick_switch(&self.app));
                }
                b'W' => {
                    let id = self.app.active;
                    self.app.input(id, "/close");
                    self.after_update();
                }
                b'L' => {
                    let id = self.app.active;
                    if let Some(b) = self.app.buffer_mut(id) {
                        b.clear();
                    }
                }
                _ => return false,
            },
            _ => return false,
        }
        self.caret_on = true;
        self.invalidate();
        true
    }

    fn overlay_key(&mut self, v: VIRTUAL_KEY, ctrl: bool, shift: bool) -> bool {
        let Some(o) = self.overlay.as_mut() else { return false };
        match v {
            VK_ESCAPE => self.overlay = None,
            VK_RETURN => self.overlay_accept(),
            VK_UP => o.move_selection(-1),
            VK_DOWN => o.move_selection(1),
            VK_PRIOR => o.move_selection(-10),
            VK_NEXT => o.move_selection(10),
            VK_TAB => {
                if let Overlay::ChannelList { by_users, .. } = o {
                    *by_users = !*by_users;
                    o.refresh(&self.app);
                } else {
                    o.move_selection(if shift { -1 } else { 1 });
                }
            }
            _ => {
                let hwnd = self.hwnd;
                let Some(ed) = o.editor() else { return false };
                if !crate::editor::edit_key(ed, v, ctrl, shift, hwnd) {
                    return false;
                }
                o.refresh(&self.app);
            }
        }
        self.invalidate();
        true
    }

    fn overlay_accept(&mut self) {
        let Some(o) = self.overlay.take() else { return };
        match o {
            Overlay::QuickSwitch { results, selected, .. } => {
                if let Some(id) = results.get(selected) {
                    self.switch_to(*id);
                }
            }
            Overlay::Confirm { action, .. } => match action {
                ConfirmAction::Paste { buffer, text } => {
                    self.app.input_confirmed(buffer, &text);
                    self.after_update();
                }
                ConfirmAction::AddZncNetworks { net, names } => self.add_znc_networks(net, names),
                ConfirmAction::RemoveNetwork { name } => self.remove_network(&name),
            },
            Overlay::Raw { raw, .. } => win::set_clipboard(self.hwnd, &raw),
            Overlay::ChannelList { net, rows, selected, .. } => {
                let name = self.app.network(net).and_then(|n| rows.get(selected).map(|&i| n.channel_list[i].0.clone()));
                if let Some(name) = name {
                    let sb = self.app.networks[&net].server_buffer;
                    self.app.input(sb, &format!("/join {name}"));
                    self.after_update();
                } else {
                    self.overlay = Some(Overlay::ChannelList {
                        net,
                        rows,
                        selected,
                        filter: Editor::single_line(),
                        scroll: 0.0,
                        by_users: true,
                    });
                }
            }
        }
        self.invalidate();
    }

    fn add_znc_networks(&mut self, net: schwaetz_net::NetworkId, names: Vec<String>) {
        let Some(base) = self.app.network(net).map(|n| n.cfg.clone()) else { return };
        let password = schwaetz_core::secrets::get(&base.name, schwaetz_core::secrets::SecretKind::ServerPassword);
        for name in names {
            let cfg = NetworkConfig {
                name: format!("{} ({name})", base.name),
                kind: NetworkKind::Znc,
                znc_network: Some(name),
                perform: Vec::new(),
                autojoin: Vec::new(),
                ..base.clone()
            };
            if let Some(p) = &password {
                schwaetz_core::secrets::set(&cfg.name, schwaetz_core::secrets::SecretKind::ServerPassword, p);
            }
            self.app.config.networks.push(cfg.clone());
            let id = self.app.add_network(cfg);
            self.app.connect(id);
        }
        let _ = self.app.config.save(&self.paths.config_file());
        self.after_update();
    }

    fn char_input(&mut self, unit: u16) {
        let c = if (0xD800..0xDC00).contains(&unit) {
            self.high_surrogate = Some(unit);
            return;
        } else if (0xDC00..0xE000).contains(&unit) {
            let Some(hi) = self.high_surrogate.take() else { return };
            char::decode_utf16([hi, unit]).next().and_then(|r| r.ok())
        } else {
            char::from_u32(unit as u32)
        };
        let Some(c) = c else { return };
        if (c as u32) < 0x20 || c == '\x7f' {
            return;
        }
        let mut buf = [0u8; 4];
        let s = c.encode_utf8(&mut buf);
        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            f.char(s);
            self.caret_on = true;
            self.invalidate();
            return;
        }
        if let Some(o) = self.overlay.as_mut() {
            if let Some(ed) = o.editor() {
                ed.insert(s);
                o.refresh(&self.app);
            }
        } else {
            self.input.insert(s);
            self.typed();
            self.refresh_completion();
        }
        self.caret_on = true;
        self.invalidate();
    }

    // ----- mouse ---------------------------------------------------------------------------------

    fn pt(&self, lp: LPARAM) -> (f32, f32) {
        let (x, y) = win::lparam_point(lp);
        (x as f32 / self.scale, y as f32 / self.scale)
    }

    fn on_splitter(&self, x: f32) -> bool {
        (x - self.sidebar_w).abs() <= 4.0
    }

    fn mouse_down(&mut self, x: f32, y: f32, double: bool) {
        unsafe {
            SetCapture(self.hwnd);
        }
        self.anims.pressed = self.control_at(x, y);
        if self.anims.pressed.is_some() {
            self.invalidate();
        }
        if let Some(c) = &self.completion
            && self.overlay.is_none()
            && self.form.is_none()
        {
            if let Some(i) = completion_row(c, x, y) {
                self.pressed = Some(Pressed::Completion(i));
                return;
            }
            if c.rect.contains(x, y) {
                return;
            }
            // A click anywhere else closes the list (until the next `:`).
            self.completion_dismissed = Some(c.colon);
            self.completion = None;
            self.invalidate();
        }
        self.tooltip = None;
        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            let action = f.press(self.win_rect, x, y, double, win::key_down(VK_SHIFT.0));
            self.form_action(action);
            self.invalidate();
            return;
        }
        if let Some(o) = self.overlay.as_ref() {
            self.pressed = Some(Pressed::Overlay(o.target(self.win_rect, x, y)));
            return;
        }
        if self.on_splitter(x) {
            self.drag = Drag::Splitter;
            return;
        }
        if let Some((sb, _)) = self.sidebar.network_button_at(x, y) {
            self.pressed = Some(Pressed::NetworkSettings(sb));
            return;
        }
        if let Some((button, _)) = self.sidebar.button_at(x, y) {
            self.pressed = Some(Pressed::Sidebar(button));
            return;
        }
        if self.stream_btn.is_some_and(|r| r.contains(x, y)) {
            self.pressed = Some(Pressed::OpenStream);
            return;
        }
        if let Some(id) = self.sidebar.hit(x, y) {
            self.switch_to(id);
            // Channels can be dragged to another position within their network.
            if self.app.buffer(id).is_some_and(|b| b.kind == BufferKind::Channel) {
                self.drag = Drag::Row(id);
                self.row_drag_from = y;
            }
            return;
        }
        if self.show_nicklist && self.nicklist.rect.contains(x, y) {
            if double
                && let Some(i) = self.nicklist.hit(x, y)
                && let Some(n) = self.nicklist.nick(i).map(str::to_owned)
            {
                // Twitch has no private messages: open the user's channel page instead.
                let id = self.app.active;
                if let Some(url) = self.app.twitch_profile_url(id, &n) {
                    win::open_url(&url);
                } else {
                    self.app.input(id, &format!("/query {n}"));
                    self.after_update();
                }
            }
            return;
        }
        if self.reply_rect.h > 0.0 && self.reply_rect.contains(x, y) {
            if self.reply_close_rect().contains(x, y) {
                self.pressed = Some(Pressed::ReplyClose);
            }
            return;
        }
        if self.input_rect.contains(x, y) {
            let inner = self.input_rect.inset(14.0, 11.0);
            let shift = win::key_down(VK_SHIFT.0);
            self.input.click(x - inner.x, y - inner.y, shift);
            if double {
                self.input.select_word_at_cursor();
            }
            self.refresh_completion();
            self.drag = Drag::Input;
            self.invalidate();
            return;
        }
        if self.chat.rect.contains(x, y) {
            if self.chat.jump_pill(x, y) {
                self.pressed = Some(Pressed::JumpPill);
                return;
            }
            match self.chat.hit(x, y) {
                Hit::Text(pos) => {
                    let t = unsafe { GetMessageTime() } as u32;
                    let triple = self.last_click.0 != 0
                        && t.wrapping_sub(self.last_click.3) < unsafe { GetDoubleClickTime() } * 2
                        && double;
                    if double {
                        self.chat.select_word(pos);
                        self.copy_on_select();
                        self.last_click = (2, x, y, t);
                    } else if self.last_click.0 == 2
                        && t.wrapping_sub(self.last_click.3) < unsafe { GetDoubleClickTime() }
                        && !triple
                    {
                        self.chat.select_line(pos);
                        self.copy_on_select();
                        self.last_click = (0, x, y, t);
                    } else {
                        self.chat.selection = Some((pos, pos));
                        self.chat.selecting = true;
                        self.drag = Drag::Chat;
                        self.last_click = (1, x, y, t);
                    }
                }
                // Links and buttons act on release (see mouse_up); the nick menu opens right away.
                hit @ (Hit::Link(_) | Hit::Reply(_) | Hit::LoadPreview(_)) => {
                    self.last_click = (3, x, y, 0);
                    self.pressed = Some(Pressed::Chat(hit));
                }
                Hit::Nick(nick) => self.nick_menu(&nick),
                Hit::Nothing => self.chat.selection = None,
            }
            self.invalidate();
        }
    }

    fn open_link(&mut self, target: LinkTarget) {
        match target {
            LinkTarget::Url(u) => win::open_url(&u),
            LinkTarget::Channel(c) => {
                let id = self.app.active;
                if let Some(net) = self.app.buffer(id).and_then(|b| b.network)
                    && let Some(existing) = self.app.find_buffer(net, &c)
                {
                    self.switch_to(existing);
                } else {
                    self.app.input(id, &format!("/join {c}"));
                    self.after_update();
                }
            }
        }
    }

    fn mouse_move(&mut self, x: f32, y: f32) {
        if let Some(c) = self.completion.as_mut()
            && let Some(i) = completion_row(c, x, y)
            && i != c.selected
        {
            c.selected = i;
            self.invalidate();
        }
        let hot = self.control_at(x, y);
        if hot != self.anims.hot {
            self.anims.hot = hot;
            self.invalidate();
        }
        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            if f.mouse_move(x, y) {
                self.invalidate();
            }
            return;
        }
        match self.drag {
            Drag::Splitter => {
                self.sidebar_w = x.clamp(SIDEBAR_MIN, self.win_rect.w * 0.4);
                self.chat.invalidate_styles();
                self.invalidate();
            }
            Drag::Chat => {
                if let Hit::Text(pos) = self.chat.hit(x, y.clamp(self.chat.rect.y, self.chat.rect.bottom() - 1.0))
                    && let Some((a, _)) = self.chat.selection
                {
                    self.chat.selection = Some((a, pos));
                    self.invalidate();
                }
            }
            Drag::Input => {
                let inner = self.input_rect.inset(14.0, 11.0);
                self.input.click(x - inner.x, y - inner.y, true);
                self.invalidate();
            }
            Drag::Row(id) => {
                if self.sidebar.drag.is_none() && (y - self.row_drag_from).abs() < 5.0 {
                    return;
                }
                let drag = self.sidebar.drop_target(id, y).map(|(before, line_y)| crate::lists::RowDrag {
                    id,
                    before,
                    line_y,
                });
                if drag != self.sidebar.drag {
                    self.sidebar.drag = drag;
                    self.invalidate();
                }
            }
            Drag::None => {
                let hover = self.sidebar.hit(x, y);
                if hover != self.sidebar.hover {
                    self.sidebar.hover = hover;
                    self.invalidate();
                }
                // Footer and network buttons animate via `anims.hot`; here only their tooltips.
                let button = self.sidebar.button_at(x, y);
                let net_button = self.sidebar.network_button_at(x, y);
                let nh = if self.show_nicklist { self.nicklist.hit(x, y) } else { None };
                if nh != self.nicklist.hover {
                    self.nicklist.hover = nh;
                    self.invalidate();
                }
                // The emote list covers part of the chat: nothing under it reacts to the pointer.
                let in_chat = self.chat.rect.contains(x, y) && !self.over_completion(x, y);
                let ch = if in_chat { self.chat.line_at(y) } else { None };
                if ch != self.chat.hover {
                    self.chat.hover = ch;
                    self.invalidate();
                }
                let tip = if in_chat {
                    self.chat.emote_at(x, y)
                } else if self.chat.rect.contains(x, y) {
                    None
                } else {
                    // Icon buttons in the sidebar footer explain themselves on hover.
                    button
                        .and_then(|(b, r)| b.tooltip().map(|t| (t.to_owned(), String::new(), r)))
                        .or_else(|| net_button.map(|(_, r)| ("Network settings".to_owned(), String::new(), r)))
                };
                if tip.as_ref().map(|t| t.2) != self.tooltip.as_ref().map(|t| t.2) {
                    self.tooltip = tip;
                    self.invalidate();
                }
            }
        }
    }

    fn mouse_up(&mut self, x: f32, y: f32) {
        unsafe {
            let _ = ReleaseCapture();
        }
        if self.anims.pressed.take().is_some() {
            self.invalidate();
        }
        // A pressed button acts only if the pointer is still on it.
        if let Some(p) = self.pressed.take()
            && self.pressed_at(x, y).as_ref() == Some(&p)
        {
            self.activate(p, x, y);
        }
        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            let action = f.release(self.win_rect, x, y);
            self.form_action(action);
        }
        if let Drag::Row(id) = self.drag
            && let Some(d) = self.sidebar.drag.take()
        {
            self.app.move_channel(id, d.before);
            self.after_update();
        }
        if self.drag == Drag::Chat {
            self.chat.selecting = false;
            if let Some((a, b)) = self.chat.selection
                && a == b
            {
                self.chat.selection = None;
            }
            self.copy_on_select();
        }
        self.drag = Drag::None;
        self.invalidate();
    }

    /// Whether a point is on the emote completion list, which covers part of the chat.
    fn over_completion(&self, x: f32, y: f32) -> bool {
        self.completion.as_ref().is_some_and(|c| c.rect.contains(x, y))
    }

    /// The animated button-like control under a point (hover and press effects).
    fn control_at(&self, x: f32, y: f32) -> Option<crate::anim::Control> {
        use crate::anim::Control;
        use crate::overlay::OverlayTarget;
        if let Some(o) = &self.overlay {
            return match o.target(self.win_rect, x, y) {
                OverlayTarget::Yes => Some(Control::ConfirmYes),
                OverlayTarget::No => Some(Control::ConfirmNo),
                _ => None,
            };
        }
        if let Some(f) = &self.form {
            return f.control_at(self.win_rect, x, y);
        }
        if self.over_completion(x, y) {
            return None;
        }
        if let Some((sb, _)) = self.sidebar.network_button_at(x, y) {
            return Some(Control::NetworkSettings(sb.0));
        }
        if let Some((b, _)) = self.sidebar.button_at(x, y) {
            return Some(match b {
                SidebarButton::Status => Control::SidebarStatus,
                SidebarButton::AddNetwork => Control::SidebarAdd,
                SidebarButton::Settings => Control::SidebarSettings,
            });
        }
        if self.reply_rect.h > 0.0 && self.reply_close_rect().contains(x, y) {
            return Some(Control::ReplyClose);
        }
        if self.stream_btn.is_some_and(|r| r.contains(x, y)) {
            return Some(Control::OpenStream);
        }
        if self.chat.rect.contains(x, y) {
            if self.chat.jump_pill(x, y) {
                return Some(Control::JumpPill);
            }
            if let Hit::Reply(line) = self.chat.hit(x, y) {
                return Some(Control::Reply(line));
            }
        }
        None
    }

    /// The button-like control under a point, compared with the pressed one on release.
    fn pressed_at(&self, x: f32, y: f32) -> Option<Pressed> {
        if let Some(i) = self.completion.as_ref().and_then(|c| completion_row(c, x, y)) {
            return Some(Pressed::Completion(i));
        }
        if let Some(o) = &self.overlay {
            return Some(Pressed::Overlay(o.target(self.win_rect, x, y)));
        }
        if let Some((sb, _)) = self.sidebar.network_button_at(x, y) {
            return Some(Pressed::NetworkSettings(sb));
        }
        if let Some((b, _)) = self.sidebar.button_at(x, y) {
            return Some(Pressed::Sidebar(b));
        }
        if self.stream_btn.is_some_and(|r| r.contains(x, y)) {
            return Some(Pressed::OpenStream);
        }
        if self.reply_rect.h > 0.0 && self.reply_close_rect().contains(x, y) {
            return Some(Pressed::ReplyClose);
        }
        if self.chat.rect.contains(x, y) {
            if self.chat.jump_pill(x, y) {
                return Some(Pressed::JumpPill);
            }
            return Some(Pressed::Chat(self.chat.hit(x, y)));
        }
        None
    }

    /// Runs a control that was pressed and released over the same spot.
    fn activate(&mut self, p: Pressed, x: f32, y: f32) {
        match p {
            Pressed::Overlay(_) => {
                let Some(o) = self.overlay.as_mut() else { return };
                match o.click(self.win_rect, x, y) {
                    OverlayClick::Accept => self.overlay_accept(),
                    OverlayClick::Dismiss => self.overlay = None,
                    OverlayClick::None => {}
                }
            }
            Pressed::NetworkSettings(sb) => {
                // The gear on a network row opens that network's settings.
                let name = self
                    .app
                    .buffer(sb)
                    .and_then(|b| b.network)
                    .and_then(|n| self.app.network(n))
                    .map(|n| n.cfg.name.clone());
                if let Some(cfg) = name.and_then(|n| self.app.config.network(&n).cloned()) {
                    self.form = Some(Box::new(crate::form::Form::network(Some(&cfg))));
                }
            }
            Pressed::Sidebar(SidebarButton::Status) => {
                let sb = self.app.status_buffer;
                self.switch_to(sb);
            }
            Pressed::Sidebar(SidebarButton::AddNetwork) => {
                self.form = Some(Box::new(crate::form::Form::network(None)));
            }
            Pressed::Sidebar(SidebarButton::Settings) => self.open_settings(),
            Pressed::ReplyClose => {
                self.reply = None;
                self.layout();
            }
            Pressed::JumpPill => self.chat.scroll_to_bottom(),
            Pressed::Completion(i) => self.accept_completion(Some(i)),
            Pressed::OpenStream => {
                if let Some(url) = self.app.twitch_stream_url(self.app.active) {
                    win::open_url(&url);
                }
            }
            Pressed::Chat(Hit::Link(target)) => self.open_link(target),
            Pressed::Chat(Hit::Reply(line)) => self.start_reply(line),
            Pressed::Chat(Hit::LoadPreview(url)) => {
                self.images.borrow_mut().requested.insert(url);
                self.chat.invalidate_styles();
            }
            Pressed::Chat(_) => {}
        }
        self.invalidate();
    }

    fn cursor_for(&self, x: f32, y: f32) -> PCWSTR {
        if self.overlay.is_none()
            && let Some(f) = &self.form
        {
            return if f.text_at(x, y) { IDC_IBEAM } else { IDC_ARROW };
        }
        if self.overlay.is_some() {
            return IDC_ARROW;
        }
        if self.on_splitter(x) || self.drag == Drag::Splitter {
            return IDC_SIZEWE;
        }
        if self.sidebar.drag.is_some() {
            return IDC_SIZENS;
        }
        if self.over_completion(x, y) {
            return IDC_ARROW;
        }
        if (self.reply_rect.h > 0.0 && self.reply_close_rect().contains(x, y))
            || self.stream_btn.is_some_and(|r| r.contains(x, y))
        {
            return IDC_HAND;
        }
        if self.input_rect.contains(x, y) {
            return IDC_IBEAM;
        }
        if self.chat.rect.contains(x, y) {
            if self.chat.jump_pill(x, y) {
                return IDC_HAND;
            }
            return match self.chat.hit(x, y) {
                Hit::Link(_) | Hit::Nick(_) | Hit::Reply(_) | Hit::LoadPreview(_) => IDC_HAND,
                _ => IDC_IBEAM,
            };
        }
        if self.sidebar.hit(x, y).is_some() || self.sidebar.button_at(x, y).is_some() {
            return IDC_HAND;
        }
        IDC_ARROW
    }

    fn wheel(&mut self, x: f32, y: f32, delta: i16) {
        let mut lines = 3u32;
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_GETWHEELSCROLLLINES,
                0,
                Some(&mut lines as *mut u32 as *mut _),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            );
        }
        let lh = self.text.fonts.line_height + 3.0;
        let dy = delta as f32 / 120.0 * lines.clamp(1, 20) as f32 * lh;
        if let Some(c) = self.completion.as_mut()
            && c.rect.contains(x, y)
        {
            let max = (c.items.len() as f32 * COMPLETION_ROW + 8.0 - c.rect.h).max(0.0);
            c.scroll = (c.scroll - dy).clamp(0.0, max);
            self.invalidate();
            return;
        }
        // Line positions change; hover state comes back with the next mouse move.
        self.tooltip = None;
        self.chat.hover = None;
        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            f.scroll_by(dy);
            self.invalidate();
            return;
        }
        if let Some(o) = self.overlay.as_mut() {
            o.scroll(dy);
        } else if self.sidebar.rect.contains(x, y) {
            self.sidebar.scroll_by(dy);
        } else if self.show_nicklist && self.nicklist.rect.contains(x, y) {
            self.nicklist.scroll_by(dy);
        } else {
            self.chat_scroll(dy);
            return;
        }
        self.invalidate();
    }

    fn right_click(&mut self, x: f32, y: f32) {
        // A dialog covers the window: only its own text boxes get a (text editing) menu.
        if self.overlay.is_none()
            && let Some(f) = self.form.as_mut()
        {
            if let Some(ed) = f.editor_at(self.win_rect, x, y) {
                edit_menu(self.hwnd, ed);
            }
            self.caret_on = true;
            self.invalidate();
            return;
        }
        if self.overlay.is_some() || self.over_completion(x, y) {
            return;
        }
        if self.input_rect.contains(x, y) {
            edit_menu(self.hwnd, &mut self.input);
            self.caret_on = true;
            self.invalidate();
            return;
        }
        if let Some((SidebarButton::Status, _)) = self.sidebar.button_at(x, y) {
            let sb = self.app.status_buffer;
            self.buffer_menu(sb);
        } else if let Some(id) = self.sidebar.hit(x, y) {
            self.buffer_menu(id);
        } else if self.show_nicklist
            && let Some(n) = self.nicklist.hit(x, y).and_then(|i| self.nicklist.nick(i)).map(str::to_owned)
        {
            self.nick_menu(&n);
        } else if self.chat.rect.contains(x, y) {
            match self.chat.hit(x, y) {
                Hit::Nick(n) => self.nick_menu(&n),
                Hit::Link(LinkTarget::Url(u)) => {
                    let r =
                        win::popup_menu(self.hwnd, &[MenuItem::Item(1, "Open link"), MenuItem::Item(2, "Copy link")]);
                    match r {
                        1 => win::open_url(&u),
                        2 => win::set_clipboard(self.hwnd, &u),
                        _ => {}
                    }
                }
                _ => {
                    let has_sel = self.chat.selection.is_some();
                    let filtered = self.chat.show_filtered;
                    // The line under the pointer, if it was received live (raw form known).
                    let raw = self.chat.line_at(y).and_then(|id| {
                        let b = self.app.active_buffer();
                        b.lines.iter().rev().find(|l| l.id == id)?.extra.as_ref()?.raw.as_deref().map(str::to_owned)
                    });
                    let mut items = vec![];
                    if has_sel {
                        items.push(MenuItem::Item(1, "Copy"));
                        items.push(MenuItem::Separator);
                    }
                    if raw.is_some() {
                        items.push(MenuItem::Item(4, "View raw message"));
                        items.push(MenuItem::Separator);
                    }
                    items.push(MenuItem::Check(2, "Show hidden joins/parts", filtered));
                    items.push(MenuItem::Item(3, "Clear buffer"));
                    match win::popup_menu(self.hwnd, &items) {
                        1 => {
                            self.copy();
                        }
                        2 => {
                            self.chat.show_filtered = !filtered;
                            self.chat.invalidate_styles();
                        }
                        3 => {
                            let id = self.app.active;
                            if let Some(b) = self.app.buffer_mut(id) {
                                b.clear();
                            }
                        }
                        4 => {
                            if let Some(raw) = raw {
                                self.overlay = Some(Overlay::Raw { raw, scroll: 0.0, content_h: 0.0 });
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        self.process_net();
        self.invalidate();
    }

    fn buffer_menu(&mut self, id: BufferId) {
        let Some(b) = self.app.buffer(id) else { return };
        let kind = b.kind;
        let notify = b.notify;
        let net = b.network;
        let conn = net.and_then(|n| self.app.network(n)).map(|n| n.conn);
        let twitch = net.and_then(|n| self.app.network(n)).is_some_and(|n| n.is_twitch());
        let stream = self.app.twitch_stream_url(id).unwrap_or_default();
        let mut items = Vec::new();
        match kind {
            BufferKind::Server => {
                if conn == Some(ConnState::Disconnected) {
                    items.push(MenuItem::Item(10, "Connect"));
                } else {
                    items.push(MenuItem::Item(11, "Disconnect"));
                    items.push(MenuItem::Item(12, "Reconnect now"));
                }
                if !twitch {
                    items.push(MenuItem::Item(13, "Channel list…"));
                }
                items.push(MenuItem::Separator);
                items.push(MenuItem::Item(15, "Edit network…"));
                items.push(MenuItem::Item(14, "Remove network"));
            }
            BufferKind::Channel => {
                let joined = b.joined;
                items.push(if joined { MenuItem::Item(20, "Leave channel") } else { MenuItem::Item(21, "Rejoin") });
                if twitch {
                    items.push(MenuItem::Item(23, "Open stream"));
                }
                items.push(MenuItem::Item(22, "Close"));
            }
            BufferKind::Query => {
                if !twitch {
                    items.push(MenuItem::Item(30, "Whois"));
                }
                items.push(MenuItem::Item(22, "Close"));
            }
            BufferKind::Special => {
                items.push(MenuItem::Item(16, "Add network…"));
                items.push(MenuItem::Item(17, "Settings…"));
            }
        }
        if kind != BufferKind::Special {
            items.push(MenuItem::Separator);
            items.push(MenuItem::Sub(
                "Notifications",
                vec![
                    MenuItem::Check(40, "Default", notify == NotifyLevel::Default),
                    MenuItem::Check(41, "All messages", notify == NotifyLevel::All),
                    MenuItem::Check(42, "Highlights only", notify == NotifyLevel::HighlightsOnly),
                    MenuItem::Check(43, "Mute", notify == NotifyLevel::Mute),
                ],
            ));
        }
        items.push(MenuItem::Item(50, "Mark as read"));
        let choice = win::popup_menu(self.hwnd, &items);
        let cmd = |s: &str| s.to_owned();
        let command = match choice {
            10 => Some(cmd("/connect")),
            11 => Some(cmd("/disconnect")),
            12 => Some(cmd("/reconnect")),
            13 => Some(cmd("/list")),
            20 => Some(cmd("/part")),
            21 => Some(cmd("/cycle")),
            22 => Some(cmd("/close")),
            30 => Some(cmd("/whois")),
            40 => Some(cmd("/notify default")),
            41 => Some(cmd("/notify all")),
            42 => Some(cmd("/notify highlights")),
            43 => Some(cmd("/notify mute")),
            _ => None,
        };
        if choice == 14
            && let Some(name) = net.and_then(|n| self.app.network(n)).map(|n| n.cfg.name.clone())
        {
            self.confirm_remove_network(name);
        }
        match choice {
            15 => self.app.input(id, "/network edit"),
            23 => win::open_url(&stream),
            16 => self.app.input(id, "/network add"),
            17 => self.app.input(id, "/settings"),
            _ => {}
        }
        if choice == 50
            && let Some(b) = self.app.buffer_mut(id)
        {
            b.mark_read();
        }
        if let Some(c) = command {
            self.app.input(id, &c);
        }
        self.after_update();
    }

    fn nick_menu(&mut self, nick: &str) {
        let id = self.app.active;
        if self.app.twitch_profile_url(id, nick).is_some() {
            return self.twitch_nick_menu(nick);
        }
        let is_chan = self.app.active_buffer().kind == BufferKind::Channel;
        let mut items = vec![
            MenuItem::Item(1, "Open query"),
            MenuItem::Item(2, "Whois"),
            MenuItem::Item(3, "Mention"),
            MenuItem::Separator,
        ];
        if is_chan {
            items.push(MenuItem::Sub(
                "Operator",
                vec![
                    MenuItem::Item(10, "Op"),
                    MenuItem::Item(11, "Deop"),
                    MenuItem::Item(12, "Voice"),
                    MenuItem::Item(13, "Devoice"),
                    MenuItem::Separator,
                    MenuItem::Item(14, "Kick"),
                    MenuItem::Item(15, "Ban"),
                    MenuItem::Item(16, "Kick + ban"),
                ],
            ));
        }
        items.push(MenuItem::Sub(
            "CTCP",
            vec![MenuItem::Item(20, "Version"), MenuItem::Item(21, "Ping"), MenuItem::Item(22, "Time")],
        ));
        items.push(MenuItem::Item(30, "Ignore"));
        items.push(MenuItem::Item(31, "Copy nick"));
        let choice = win::popup_menu(self.hwnd, &items);
        let command = match choice {
            1 => Some(format!("/query {nick}")),
            2 => Some(format!("/whois {nick}")),
            10 => Some(format!("/op {nick}")),
            11 => Some(format!("/deop {nick}")),
            12 => Some(format!("/voice {nick}")),
            13 => Some(format!("/devoice {nick}")),
            14 => Some(format!("/kick {nick}")),
            15 => Some(format!("/ban {nick}")),
            16 => Some(format!("/kickban {nick}")),
            20 => Some(format!("/ctcp {nick} VERSION")),
            21 => Some(format!("/ping {nick}")),
            22 => Some(format!("/ctcp {nick} TIME")),
            30 => Some(format!("/ignore {nick}")),
            _ => None,
        };
        match choice {
            3 => {
                let t = if self.input.is_empty() { format!("{nick}: ") } else { format!("{nick} ") };
                self.input.insert(&t);
            }
            31 => win::set_clipboard(self.hwnd, nick),
            _ => {}
        }
        if let Some(c) = command {
            self.app.input(id, &c);
            self.after_update();
        }
        self.invalidate();
    }

    /// Nick menu for Twitch chat: no queries, WHOIS, CTCP or channel modes there (and moderation
    /// commands no longer work over IRC), so it offers the web pages instead.
    fn twitch_nick_menu(&mut self, nick: &str) {
        let id = self.app.active;
        let b = self.app.active_buffer();
        let channel = (b.kind == BufferKind::Channel).then(|| b.name.trim_start_matches('#').to_owned());
        let latest = self
            .app
            .can_reply(id)
            .then(|| b.lines.iter().rev().find(|l| l.nick.eq_ignore_ascii_case(nick) && l.msgid().is_some()))
            .flatten()
            .map(|l| l.id);
        let mut items = vec![MenuItem::Item(1, "Open profile")];
        if channel.is_some() {
            items.push(MenuItem::Item(2, "Open viewer card"));
        }
        items.push(MenuItem::Separator);
        items.push(MenuItem::Item(3, "Mention"));
        if latest.is_some() {
            items.push(MenuItem::Item(4, "Reply to latest message"));
        }
        items.push(MenuItem::Separator);
        items.push(MenuItem::Item(30, "Ignore"));
        items.push(MenuItem::Item(31, "Copy name"));
        match win::popup_menu(self.hwnd, &items) {
            1 => self.open_twitch_profile(nick),
            2 => {
                if let Some(c) = channel {
                    win::open_url(&format!(
                        "https://www.twitch.tv/popout/{c}/viewercard/{}",
                        nick.to_ascii_lowercase()
                    ));
                }
            }
            3 => {
                self.input.insert(&format!("@{nick} "));
            }
            4 => {
                if let Some(l) = latest {
                    self.start_reply(l);
                }
            }
            30 => {
                self.app.input(id, &format!("/ignore {nick}"));
                self.after_update();
            }
            31 => win::set_clipboard(self.hwnd, nick),
            _ => {}
        }
        self.invalidate();
    }

    fn open_twitch_profile(&self, nick: &str) {
        if let Some(url) = self.app.twitch_profile_url(self.app.active, nick) {
            win::open_url(&url);
        }
    }

    fn tray_message(&mut self, lp: LPARAM) {
        let event = (lp.0 & 0xffff) as u32;
        match event {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => self.toggle_visible(),
            win::NIN_BALLOONUSERCLICK => {
                self.restore();
                if let Some(b) = self.notify_buffer.take() {
                    self.switch_to(b);
                }
            }
            WM_RBUTTONUP | WM_CONTEXTMENU => {
                let r = win::popup_menu(
                    self.hwnd,
                    &[
                        MenuItem::Item(1, "Show schwätz"),
                        MenuItem::Item(2, "Reconnect all"),
                        MenuItem::Separator,
                        MenuItem::Item(3, "Quit"),
                    ],
                );
                match r {
                    1 => self.restore(),
                    2 => {
                        let ids: Vec<_> = self.app.networks.keys().copied().collect();
                        for id in ids {
                            self.app.reconnect_now(id);
                        }
                        self.after_update();
                    }
                    3 => self.begin_quit(),
                    _ => {}
                }
                self.process_net();
            }
            _ => {}
        }
    }

    /// Input forwarded from another process (second instance, irc:// links, automation).
    fn remote_input(&mut self, text: &str) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if line.starts_with("irc://") || line.starts_with("ircs://") {
                self.open_irc_url(line);
            } else if line == "!show" {
                self.restore();
            } else if !line.strip_prefix('!').is_some_and(|r| self.automation(r)) {
                let id = self.app.active;
                self.run_input(id, line);
            }
        }
        self.after_update();
        self.invalidate();
    }

    /// Scripted UI actions for automated checks (`scripts/send.ps1 "!click 500 640"`), in DIPs:
    /// `!move x y`, `!click x y`, `!drag x0 y0 x1 y1`, `!key <vk>`, `!submit <text>` (types and presses Enter).
    fn automation(&mut self, cmd: &str) -> bool {
        let (verb, arg) = cmd.split_once(' ').unwrap_or((cmd, ""));
        let xy = || -> Option<(f32, f32)> {
            let (x, y) = arg.trim().split_once(' ')?;
            Some((x.trim().parse().ok()?, y.trim().parse().ok()?))
        };
        match verb {
            "move" => {
                if let Some((x, y)) = xy() {
                    self.mouse_move(x, y);
                }
            }
            "click" => {
                if let Some((x, y)) = xy() {
                    self.mouse_move(x, y);
                    self.mouse_down(x, y, false);
                    self.mouse_up(x, y);
                }
            }
            "drag" => {
                let v: Vec<f32> = arg.split_whitespace().filter_map(|s| s.parse().ok()).collect();
                if let [x0, y0, x1, y1] = v[..] {
                    self.mouse_move(x0, y0);
                    self.mouse_down(x0, y0, false);
                    self.mouse_move(x1, y1);
                    self.mouse_up(x1, y1);
                }
            }
            "key" => {
                if let Ok(vk) = arg.trim().parse() {
                    self.key_down(vk);
                }
            }
            "submit" => {
                self.input.set_text(arg, None);
                self.submit();
            }
            "stream" => {
                // Test data for the Twitch live display without API access:
                // `!stream <login> live|offline <viewers> <game> | <title>`.
                let mut words = arg.splitn(4, ' ');
                let (Some(login), Some(state), Some(viewers)) = (words.next(), words.next(), words.next()) else {
                    return true;
                };
                let (game, title) = words.next().unwrap_or("").split_once(" | ").unwrap_or(("", ""));
                let Some(network) = self.app.network_of(self.app.active).map(|n| n.id) else { return true };
                let info = schwaetz_core::helix::StreamInfo {
                    live: state == "live",
                    title: title.to_owned(),
                    game: game.to_owned(),
                    viewers: viewers.parse().unwrap_or(0),
                    started_at: String::new(),
                };
                self.app.on_live_result(schwaetz_core::helix::LiveResult {
                    network,
                    client_id: None,
                    ids: Default::default(),
                    result: Ok(vec![(login.to_ascii_lowercase(), info)]),
                    unauthorized: false,
                });
            }
            _ => return false,
        }
        true
    }

    fn open_irc_url(&mut self, url: &str) {
        let Some((host, _, _)) = NetworkConfig::parse_server(url) else { return };
        let channel = url
            .split_once("://")
            .and_then(|(_, r)| r.split_once('/'))
            .map(|(_, c)| c.trim_start_matches('/').to_owned())
            .filter(|c| !c.is_empty())
            .map(|c| if c.starts_with(['#', '&']) { c } else { format!("#{c}") });
        self.restore();
        let sb = self.app.status_buffer;
        self.app.input(sb, &format!("/connect {url}"));
        if let Some(chan) = channel {
            // Join once the (possibly new) network is registered.
            let net = self.app.networks.values().find(|n| {
                n.cfg
                    .servers
                    .iter()
                    .any(|s| NetworkConfig::parse_server(s).is_some_and(|(h, _, _)| h.eq_ignore_ascii_case(&host)))
            });
            if let Some(n) = net {
                let id = n.id;
                if n.conn == ConnState::Ready {
                    let b = n.server_buffer;
                    self.app.input(b, &format!("/join {chan}"));
                } else if let Some(cfg) = self.app.networks.get_mut(&id) {
                    cfg.cfg.autojoin.push(chan.clone());
                    let list = cfg.cfg.autojoin_list();
                    cfg.session.config_mut().autojoin = list;
                }
            }
        }
    }

    fn restore(&self) {
        unsafe {
            let _ = ShowWindow(self.hwnd, if IsIconic(self.hwnd).as_bool() { SW_RESTORE } else { SW_SHOW });
            let _ = SetForegroundWindow(self.hwnd);
        }
    }

    fn toggle_visible(&self) {
        unsafe {
            if IsWindowVisible(self.hwnd).as_bool() && !IsIconic(self.hwnd).as_bool() {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            } else {
                self.restore();
            }
        }
    }

    // ----- message dispatch ----------------------------------------------------------------------

    fn handle(&mut self, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                unsafe { BeginPaint(self.hwnd, &mut ps) };
                self.paint();
                unsafe {
                    let _ = EndPaint(self.hwnd, &ps);
                }
                Some(LRESULT(0))
            }
            WM_SIZE => {
                self.invalidate();
                None
            }
            WM_DPICHANGED => {
                let dpi = (wp.0 & 0xffff) as f32;
                self.scale = dpi / 96.0;
                if let Some(g) = self.gfx.as_mut() {
                    g.set_dpi(dpi);
                }
                let r = unsafe { &*(lp.0 as *const RECT) };
                unsafe {
                    let _ = SetWindowPos(
                        self.hwnd,
                        None,
                        r.left,
                        r.top,
                        r.right - r.left,
                        r.bottom - r.top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                self.chat.invalidate_styles();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_GETMINMAXINFO => {
                let mmi = unsafe { &mut *(lp.0 as *mut MINMAXINFO) };
                mmi.ptMinTrackSize = POINT { x: (560.0 * self.scale) as i32, y: (360.0 * self.scale) as i32 };
                Some(LRESULT(0))
            }
            WM_ERASEBKGND => Some(LRESULT(1)),
            WM_APP_MEDIA => {
                self.process_media();
                Some(LRESULT(0))
            }
            WM_APP_LIVE => {
                while let Ok(r) = self.workers.1.try_recv() {
                    match r {
                        WorkerResult::Live(r) => self.app.on_live_result(r),
                        WorkerResult::Emotes(r) => {
                            self.app.on_emote_result(r);
                            self.refresh_completion();
                        }
                        WorkerResult::Auth(net, req, resp) => self.app.on_auth_result(net, req, resp),
                    }
                }
                self.after_update();
                self.invalidate();
                Some(LRESULT(0))
            }
            WM_APP_NET => {
                self.process_net();
                Some(LRESULT(0))
            }
            WM_TIMER => {
                match wp.0 {
                    TIMER_TICK => {
                        let t = now();
                        self.app.tick(t);
                        if let Some(h) = self.services.scripts.as_mut() {
                            h.tick(&mut self.app, t);
                        }
                        self.process_net();
                        self.after_update();
                        // Countdown/lag text in the topic bar.
                        if matches!(self.app.active_buffer().kind, BufferKind::Server) {
                            self.invalidate();
                        }
                    }
                    TIMER_UI_ANIM => {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), TIMER_UI_ANIM);
                        }
                        self.invalidate();
                    }
                    TIMER_ANIM => {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
                        }
                        self.invalidate();
                    }
                    TIMER_PAINT => {
                        unsafe {
                            let _ = KillTimer(Some(self.hwnd), TIMER_PAINT);
                        }
                        self.paint_pending = false;
                        self.invalidate();
                    }
                    TIMER_CARET if self.active_window => {
                        self.caret_on = !self.caret_on;
                        self.invalidate();
                    }
                    _ => {}
                }
                Some(LRESULT(0))
            }
            WM_ACTIVATE => {
                let active = (wp.0 & 0xffff) != 0;
                self.active_window = active;
                self.app.set_focused(active);
                self.caret_on = true;
                self.after_update();
                self.invalidate();
                None
            }
            WM_KEYDOWN | WM_SYSKEYDOWN => {
                let handled = self.key_down(wp.0 as u16);
                // Editing or moving the caret can open, narrow or close the emote list.
                if self.form.is_none() && self.overlay.is_none() {
                    self.refresh_completion();
                }
                if handled { Some(LRESULT(0)) } else { None }
            }
            WM_CHAR => {
                self.char_input(wp.0 as u16);
                Some(LRESULT(0))
            }
            WM_SYSCHAR if win::key_down(VK_MENU.0) => Some(LRESULT(0)),
            WM_LBUTTONDOWN => {
                let (x, y) = self.pt(lp);
                self.mouse_down(x, y, false);
                Some(LRESULT(0))
            }
            WM_LBUTTONDBLCLK => {
                let (x, y) = self.pt(lp);
                self.mouse_down(x, y, true);
                Some(LRESULT(0))
            }
            WM_MOUSEMOVE => {
                if !self.mouse_tracking {
                    self.mouse_tracking = true;
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: self.hwnd,
                        dwHoverTime: 0,
                    };
                    unsafe {
                        let _ = TrackMouseEvent(&mut tme);
                    }
                }
                let (x, y) = self.pt(lp);
                self.mouse_move(x, y);
                Some(LRESULT(0))
            }
            win::WM_MOUSELEAVE => {
                self.mouse_tracking = false;
                if self.drag == Drag::None
                    && (self.chat.hover.is_some()
                        || self.sidebar.hover.is_some()
                        || self.anims.hot.is_some()
                        || self.nicklist.hover.is_some()
                        || self.tooltip.is_some())
                {
                    self.tooltip = None;
                    self.chat.hover = None;
                    self.sidebar.hover = None;
                    self.anims.hot = None;
                    self.nicklist.hover = None;
                    self.invalidate();
                }
                Some(LRESULT(0))
            }
            WM_LBUTTONUP => {
                let (x, y) = self.pt(lp);
                self.mouse_up(x, y);
                Some(LRESULT(0))
            }
            WM_MBUTTONDOWN => {
                let (x, y) = self.pt(lp);
                self.middle_pressed = self.sidebar.hit(x, y);
                Some(LRESULT(0))
            }
            WM_MBUTTONUP => {
                let (x, y) = self.pt(lp);
                if let Some(id) = self.middle_pressed.take()
                    && self.sidebar.hit(x, y) == Some(id)
                {
                    self.middle_click_buffer(id);
                }
                Some(LRESULT(0))
            }
            WM_RBUTTONUP => {
                let (x, y) = self.pt(lp);
                self.right_click(x, y);
                Some(LRESULT(0))
            }
            WM_MOUSEWHEEL => {
                let mut p = POINT { x: win::lparam_point(lp).0, y: win::lparam_point(lp).1 };
                unsafe {
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                self.wheel(p.x as f32 / self.scale, p.y as f32 / self.scale, win::wheel_delta(wp));
                Some(LRESULT(0))
            }
            WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCLIENT => {
                let mut p = POINT::default();
                unsafe {
                    let _ = GetCursorPos(&mut p);
                    let _ = ScreenToClient(self.hwnd, &mut p);
                }
                let c = self.cursor_for(p.x as f32 / self.scale, p.y as f32 / self.scale);
                unsafe {
                    SetCursor(LoadCursorW(None, c).ok());
                }
                Some(LRESULT(1))
            }
            win::WM_APP_TRAY => {
                self.tray_message(lp);
                Some(LRESULT(0))
            }
            WM_COPYDATA => {
                let cds = unsafe { &*(lp.0 as *const windows::Win32::System::DataExchange::COPYDATASTRUCT) };
                if cds.dwData != COPYDATA_MAGIC || cds.lpData.is_null() {
                    return Some(LRESULT(0));
                }
                let units = unsafe { std::slice::from_raw_parts(cds.lpData as *const u16, cds.cbData as usize / 2) };
                let text = String::from_utf16_lossy(units);
                self.remote_input(&text);
                Some(LRESULT(1))
            }
            WM_POWERBROADCAST => {
                if wp.0 as u32 == PBT_APMRESUMEAUTOMATIC || wp.0 as u32 == PBT_APMRESUMESUSPEND {
                    self.app.on_connectivity_restored();
                    self.after_update();
                }
                Some(LRESULT(1))
            }
            WM_SETTINGCHANGE => {
                if self.app.config.appearance.theme == "system" {
                    self.apply_appearance();
                }
                None
            }
            WM_SYSCOMMAND if (wp.0 & 0xfff0) as u32 == SC_MINIMIZE && self.app.config.general.minimize_to_tray => {
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_HIDE);
                }
                Some(LRESULT(0))
            }
            WM_ENDSESSION if wp.0 != 0 => {
                // Windows is logging off or shutting down: persist state now.
                self.save_session();
                Some(LRESULT(0))
            }
            WM_CLOSE => {
                if self.app.config.general.close_to_tray && !self.quitting {
                    unsafe {
                        let _ = ShowWindow(self.hwnd, SW_HIDE);
                    }
                } else {
                    self.begin_quit();
                }
                Some(LRESULT(0))
            }
            WM_DESTROY => {
                self.tray.remove();
                unsafe { PostQuitMessage(0) };
                Some(LRESULT(0))
            }
            _ => None,
        }
    }
}

#[allow(dead_code)]
fn _assert_net_command_send(c: NetCommand) -> NetCommand {
    c
}

/// Runs `f` with the UI if it isn't currently borrowed (accessibility providers).
pub(crate) fn with_ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|c| c.try_borrow_mut().ok().and_then(|mut g| g.as_mut().map(|ui| f(ui))))
}

impl Ui {
    fn to_screen(&self, r: Rect) -> RECT {
        let mut p = POINT::default();
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::ClientToScreen(self.hwnd, &mut p);
        }
        let s = self.scale;
        RECT {
            left: p.x + (r.x * s) as i32,
            top: p.y + (r.y * s) as i32,
            right: p.x + (r.right() * s) as i32,
            bottom: p.y + (r.bottom() * s) as i32,
        }
    }

    pub(crate) fn a11y_snapshot(&self) -> crate::a11y::Snapshot {
        let (title, _) = self.app.topic_for(self.app.active);
        let buffers = self
            .sidebar
            .row_rects()
            .into_iter()
            .filter_map(|(id, r)| {
                let b = self.app.buffer(id)?;
                let mut name = match b.kind {
                    BufferKind::Server => format!(
                        "{} network",
                        self.app.network_of(id).map(|n| n.display_name().to_owned()).unwrap_or_default()
                    ),
                    _ => b.name.clone(),
                };
                if b.highlights > 0 {
                    name.push_str(&format!(", {} highlights", b.highlights));
                } else if b.unread > 0 {
                    name.push_str(&format!(", {} unread", b.unread));
                }
                Some((id.0, name, id == self.app.active, self.to_screen(r)))
            })
            .collect();
        let b = self.app.active_buffer();
        let lines = b
            .lines
            .iter()
            .filter(|l| !l.flags.has(schwaetz_core::LineFlags::FILTERED))
            .rev()
            .take(30)
            .map(spoken)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        crate::a11y::Snapshot {
            title: format!("{title} — schwätz"),
            window: self.to_screen(self.win_rect),
            sidebar: self.to_screen(self.sidebar.rect),
            chat: self.to_screen(self.chat.rect),
            input: self.to_screen(self.input_rect),
            input_text: self.input.text().to_owned(),
            buffers,
            lines,
        }
    }

    pub(crate) fn a11y_select(&mut self, id: u32) {
        self.switch_to(BufferId(id));
    }

    pub(crate) fn a11y_set_input(&mut self, text: &str) {
        self.input.set_text(text, None);
        self.invalidate();
    }

    /// Speaks new messages of the active buffer (and highlights anywhere) to screen readers.
    fn announce(&mut self, lines: &[(BufferId, u64)]) {
        if !crate::a11y::listening() {
            return;
        }
        let now = now();
        for &(bid, lid) in lines {
            let Some(b) = self.app.buffer(bid) else { continue };
            let Some(l) = b.lines.iter().rev().find(|l| l.id == lid) else { continue };
            if l.flags.has(schwaetz_core::LineFlags::OWN)
                || l.flags.has(schwaetz_core::LineFlags::HISTORY)
                || !l.kind.is_message()
            {
                continue;
            }
            let highlight = l.flags.has(schwaetz_core::LineFlags::HIGHLIGHT);
            if bid != self.app.active && !highlight {
                continue;
            }
            // In busy channels only highlights are read out (at most 3 lines per second).
            if now - self.a11y_window.0 > 1000 {
                self.a11y_window = (now, 0);
            }
            if self.a11y_window.1 >= 3 && !highlight {
                continue;
            }
            self.a11y_window.1 += 1;
            let mut text = spoken(l);
            if bid != self.app.active {
                text = format!("{} in {}: {text}", if highlight { "Highlight" } else { "Message" }, b.name);
            }
            crate::a11y::announce(self.hwnd, &text, highlight);
        }
    }
}

fn spoken(l: &schwaetz_core::Line) -> String {
    let text = schwaetz_proto::format::strip(&l.text);
    match l.kind {
        LineKind::Message => format!("{}: {text}", l.display_nick()),
        LineKind::Action => format!("{} {text}", l.display_nick()),
        LineKind::Notice => format!("Notice from {}: {text}", l.display_nick()),
        _ => text,
    }
}

/// The message a reply will be attached to.
#[derive(Clone, Debug)]
pub(crate) struct ReplyTarget {
    buffer: BufferId,
    msgid: String,
    nick: String,
    excerpt: String,
}

/// Caption and note of the network dialog's "Sign in with Twitch" button.
fn twitch_account_button(app: &App, net: Option<schwaetz_net::NetworkId>) -> (&'static str, String) {
    let Some(a) = net.and_then(|n| app.twitch_auth(n)) else {
        return ("Sign in with Twitch", "Save this network as a Twitch network first".into());
    };
    if !a.available() {
        return ("Sign in with Twitch", "Not available in this build".into());
    }
    if let Some(code) = a.user_code() {
        return ("Cancel", format!("Authorize in your browser (code {code})"));
    }
    if a.signing_in() {
        return ("Cancel", "Contacting Twitch…".into());
    }
    match a.login() {
        Some("") => ("Sign out", "Signed in".into()),
        Some(login) => ("Sign out", format!("Signed in as {login}")),
        None => ("Sign in with Twitch", "Used for live status".into()),
    }
}

/// Undo / Cut / Copy / Paste / Select all for a text box. Password boxes never put their text on
/// the clipboard.
fn edit_menu(hwnd: HWND, ed: &mut Editor) {
    let copyable = ed.has_selection() && !ed.masked;
    let mut items = vec![MenuItem::Item(1, "Undo"), MenuItem::Separator];
    if copyable {
        items.push(MenuItem::Item(2, "Cut"));
        items.push(MenuItem::Item(3, "Copy"));
    }
    items.push(MenuItem::Item(4, "Paste"));
    items.push(MenuItem::Separator);
    items.push(MenuItem::Item(5, "Select all"));
    match win::popup_menu(hwnd, &items) {
        1 => ed.undo(),
        2 => {
            win::set_clipboard(hwnd, ed.selected_text());
            ed.backspace(false);
        }
        3 => win::set_clipboard(hwnd, ed.selected_text()),
        4 => {
            if let Some(t) = win::get_clipboard(hwnd) {
                ed.insert(&t);
            }
        }
        5 => ed.select_all(),
        _ => {}
    }
}

/// The completion row under a point.
fn completion_row(c: &EmoteCompletion, x: f32, y: f32) -> Option<usize> {
    let inner = c.rect.inset(0.0, 4.0);
    if !inner.contains(x, y) {
        return None;
    }
    let i = ((y - inner.y + c.scroll) / COMPLETION_ROW) as usize;
    (i < c.items.len()).then_some(i)
}

impl Ui {
    /// Opens, updates or closes the emote list for the `:word` before the caret (Twitch
    /// channels only: a `:` at the start of a word followed by at least one letter or digit).
    fn refresh_completion(&mut self) {
        let text = self.input.text();
        let cursor = self.input.cursor.min(text.len());
        let before = &text[..cursor];
        let word = before.len()
            - before.chars().rev().take_while(|c| c.is_alphanumeric() || *c == '_').map(char::len_utf8).sum::<usize>();
        let colon = word.checked_sub(1).filter(|&i| before.as_bytes()[i] == b':');
        let opens_word = |i: usize| i == 0 || before[..i].chars().next_back().is_some_and(char::is_whitespace);
        let Some(colon) = colon.filter(|&i| word < cursor && opens_word(i)) else {
            if self.completion.is_some() {
                self.invalidate();
            }
            self.completion = None;
            self.completion_dismissed = None;
            self.completion_for = None;
            return;
        };
        if self.completion_dismissed == Some(colon) {
            return;
        }
        let query = before[word..].to_owned();
        let key = (colon, query.clone(), self.app.emote_gen);
        if self.completion_for.as_ref() == Some(&key) {
            return;
        }
        self.completion_for = Some(key);
        // Keep the selection on the same emote when only the lists changed.
        let keep = self
            .completion
            .as_ref()
            .filter(|c| c.colon == colon && c.query == query)
            .map(|c| (c.items.get(c.selected).map(|e| e.name.clone()), c.scroll));
        let items = self.app.emote_completions(self.app.active, &query, 500);
        let (selected, scroll) = match keep {
            Some((Some(name), scroll)) => (items.iter().position(|e| e.name == name).unwrap_or(0), scroll),
            _ => (0, 0.0),
        };
        self.completion = (!items.is_empty()).then(|| EmoteCompletion {
            colon,
            query,
            items,
            selected,
            scroll,
            rect: Rect::default(),
        });
        self.invalidate();
    }

    /// Keys while the emote list is open; returns whether the key was used.
    fn completion_key(&mut self, v: VIRTUAL_KEY) -> bool {
        let Some(c) = self.completion.as_mut() else { return false };
        let n = c.items.len();
        let page = ((c.rect.h / COMPLETION_ROW) as usize).max(1);
        match v {
            VK_UP => c.selected = (c.selected + n - 1) % n,
            VK_DOWN => c.selected = (c.selected + 1) % n,
            VK_PRIOR => c.selected = c.selected.saturating_sub(page),
            VK_NEXT => c.selected = (c.selected + page).min(n - 1),
            VK_TAB | VK_RETURN => {
                self.accept_completion(None);
                return true;
            }
            VK_ESCAPE => {
                self.completion_dismissed = Some(c.colon);
                self.completion = None;
                return true;
            }
            _ => return false,
        }
        // Keep the selection in view.
        let top = c.selected as f32 * COMPLETION_ROW;
        let view = (c.rect.h - 8.0).max(COMPLETION_ROW);
        if top < c.scroll {
            c.scroll = top;
        } else if top + COMPLETION_ROW > c.scroll + view {
            c.scroll = top + COMPLETION_ROW - view;
        }
        true
    }

    /// Replaces the `:word` with the chosen emote (the selected one, or row `index`).
    fn accept_completion(&mut self, index: Option<usize>) {
        let Some(c) = self.completion.take() else { return };
        let Some(e) = c.items.get(index.unwrap_or(c.selected)) else { return };
        let text = self.input.text().to_owned();
        let cursor = self.input.cursor.min(text.len()).max(c.colon);
        let rest = &text[cursor..];
        let space = if rest.starts_with(' ') { "" } else { " " };
        let new = format!("{}{}{space}{rest}", &text[..c.colon], e.name);
        let caret = c.colon + e.name.len() + space.len().max(usize::from(rest.starts_with(' ')));
        self.input.set_text(&new, Some(caret));
        self.completion_dismissed = None;
        self.completion_for = None;
        self.typed();
        self.invalidate();
    }

    /// Draws the emote list above the input, starting at `anchor_x`; at most as tall as the
    /// chat pane, scrolling when longer.
    fn draw_completion(&self, p: &Painter, th: &Theme, c: &mut EmoteCompletion, anchor_x: f32) {
        let f = &self.text.fonts;
        let w = 320.0f32.min(self.chat.rect.w - 16.0);
        let bottom = self.input_rect.y - 6.0;
        let max_h = (bottom - self.chat.rect.y).max(COMPLETION_ROW + 8.0);
        let h = (c.items.len() as f32 * COMPLETION_ROW + 8.0).min(max_h);
        let x = anchor_x.clamp(self.chat.rect.x + 8.0, (self.chat.rect.right() - w - 8.0).max(self.chat.rect.x + 8.0));
        let r = Rect::new(x, bottom - h, w, h);
        c.rect = r;
        c.scroll = c.scroll.clamp(0.0, (c.items.len() as f32 * COMPLETION_ROW + 8.0 - h).max(0.0));
        p.fill_round(Rect::new(r.x, r.y + 3.0, r.w, r.h), 8.0, with_alpha(crate::gfx::hex(0), 0.25));
        p.fill_round(r, 8.0, th.panel_bg);
        p.stroke_round(r, 8.0, th.border, 1.0);
        let inner = r.inset(4.0, 4.0);
        p.clip(inner);
        let first = (c.scroll / COMPLETION_ROW) as usize;
        let visible = (inner.h / COMPLETION_ROW) as usize + 2;
        for (i, e) in c.items.iter().enumerate().skip(first).take(visible) {
            let row = Rect::new(inner.x, inner.y + i as f32 * COMPLETION_ROW - c.scroll, inner.w, COMPLETION_ROW);
            if i == c.selected {
                p.fill_round(row, 5.0, with_alpha(th.accent, 0.18));
            }
            // The emote itself (loaded like inline emotes), at most twice as wide as tall.
            let size = self.images.borrow_mut().size(&e.url, false);
            if let Some((iw, ih)) = size
                && let Some(bmp) = self.images.borrow().bitmap(&e.url)
            {
                let eh = 24.0f32;
                let ew = (eh * iw as f32 / ih.max(1) as f32).min(eh * 2.0);
                p.bitmap(&bmp.cast().unwrap(), Rect::new(row.x + 8.0, row.y + (row.h - eh) / 2.0, ew, eh), 1.0);
            }
            let label = self.text.layout(&e.label(), &f.ui_small, 120.0, 20.0);
            let lw = text::metrics(&label).width;
            p.text(&label, row.right() - lw - 10.0, row.y + (row.h - 14.0) / 2.0, th.text_dim);
            let name = self.text.layout(&e.name, &f.ui, (row.w - 70.0 - lw - 16.0).max(10.0), 20.0);
            p.text(&name, row.x + 62.0, row.y + (row.h - 17.0) / 2.0, th.text);
        }
        p.unclip();
        // Scrollbar when the list is longer than the space.
        let content = c.items.len() as f32 * COMPLETION_ROW;
        if content > inner.h {
            let th_h = (inner.h * inner.h / content).max(20.0);
            let ty = inner.y + (inner.h - th_h) * (c.scroll / (content - inner.h).max(1.0));
            p.fill_round(Rect::new(r.right() - 7.0, ty, 4.0, th_h), 2.0, th.scrollbar);
        }
    }
}
