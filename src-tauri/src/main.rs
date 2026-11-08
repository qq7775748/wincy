#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod clipboard;
mod store;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, PhysicalPosition, RunEvent, State, WindowEvent};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};

use store::{Item, Settings, Store};

struct AppState {
    store: Mutex<Store>,
    last_seq: AtomicU32,
    foreground: AtomicIsize,
    last_hotkey: Mutex<Instant>,
    hide_at: Mutex<Option<Instant>>,
    shown_at: Mutex<Option<Instant>>,
    /// Timestamp of the last programmatic `set_position`, so that the `Moved`
    /// event it triggers is not mistaken for a user drag.
    prog_move: Mutex<Instant>,
    /// Bumped on every blur so stale "should I hide?" checks can bail out.
    blur_gen: Mutex<u64>,
    /// Whether the popup has held the keyboard focus since it was shown. If it
    /// never got it (Windows refused the activation), a blur must not close it.
    focused_once: Mutex<bool>,
    /// Throttles history.json rewrites while the user drags the popup.
    /// Holds (last write time, coordinates currently on disk).
    pos_save: Mutex<(Instant, Option<(i32, i32)>)>,
    /// Set when a resize happened but its history.json flush was throttled;
    /// the value is written when the window goes away.
    size_dirty: AtomicBool,
    /// Throttles the history.json write that follows a resize drag.
    size_save: Mutex<Instant>,
    pause_item: Mutex<Option<CheckMenuItem<tauri::Wry>>>,
}

/// Opt-in file log (`TEECY_LOG=1`), handy when debugging focus/hotkey issues.
fn log(msg: &str) {
    if std::env::var("TEECY_LOG").is_err() {
        return;
    }
    use std::io::Write;
    let path = std::env::temp_dir().join("teecy.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[{}] {}", store::now_ms(), msg);
    }
}

impl AppState {
    fn new() -> Self {
        Self {
            store: Mutex::new(Store::default()),
            last_seq: AtomicU32::new(0),
            foreground: AtomicIsize::new(0),
            last_hotkey: Mutex::new(Instant::now() - Duration::from_secs(5)),
            hide_at: Mutex::new(None),
            shown_at: Mutex::new(None),
            prog_move: Mutex::new(Instant::now() - Duration::from_secs(5)),
            blur_gen: Mutex::new(0),
            focused_once: Mutex::new(false),
            pos_save: Mutex::new((Instant::now() - Duration::from_secs(5), None)),
            size_dirty: AtomicBool::new(false),
            size_save: Mutex::new(Instant::now() - Duration::from_secs(5)),
            pause_item: Mutex::new(None),
        }
    }
}

// ---------------------------------------------------------------- windows glue

/// Logical popup size — keep in sync with `tauri.conf.json`.
const POPUP_W: f64 = 480.0;
const POPUP_H: f64 = 540.0;
const MARGIN: f64 = 12.0;

#[derive(Clone, Copy, PartialEq)]
enum Anchor {
    Cursor,
    TopRight,
    BottomRight,
    Center,
    Custom,
}

fn anchor_of(s: &str) -> Anchor {
    match s {
        "top_right" => Anchor::TopRight,
        "bottom_right" => Anchor::BottomRight,
        "center" => Anchor::Center,
        "custom" => Anchor::Custom,
        _ => Anchor::Cursor,
    }
}

/// Keep the window fully inside the monitor work area.
fn clamp_into(x: i32, y: i32, w: i32, h: i32, area: (i32, i32, i32, i32), margin: i32) -> (i32, i32) {
    let (left, top, right, bottom) = area;
    let max_x = (right - w - margin).max(left + margin);
    let max_y = (bottom - h - margin).max(top + margin);
    (x.clamp(left + margin, max_x), y.clamp(top + margin, max_y))
}

fn work_area(x: i32, y: i32) -> Option<(i32, i32, i32, i32)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::Graphics::Gdi::{
        GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    unsafe {
        let pt = POINT { x, y };
        let mon = MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(mon, &mut info).as_bool() {
            let r = info.rcWork;
            Some((r.left, r.top, r.right, r.bottom))
        } else {
            None
        }
    }
}

fn position_popup(app: &AppHandle, win: &tauri::WebviewWindow) {
    let state = app.state::<AppState>();
    let (anchor, saved, lw, lh) = {
        let s = state.store.lock().unwrap();
        (
            anchor_of(&s.settings.position),
            (s.settings.pos_x, s.settings.pos_y),
            if s.settings.popup_w > 0.0 { s.settings.popup_w } else { POPUP_W },
            if s.settings.popup_h > 0.0 { s.settings.popup_h } else { POPUP_H },
        )
    };
    let cursor = app.cursor_position().unwrap_or(PhysicalPosition::new(0.0, 0.0));
    let scale = app
        .monitor_from_point(cursor.x, cursor.y)
        .ok()
        .flatten()
        .map(|m| m.scale_factor())
        .unwrap_or(1.0);
    // The window is resizable, so the size comes from settings (kept in sync by
    // the Resized handler); outer_size() is unreliable before the webview has
    // been laid out for the first time.
    let w = (lw * scale) as i32;
    let h = (lh * scale) as i32;
    let margin = (MARGIN * scale) as i32;

    let area = match work_area(cursor.x as i32, cursor.y as i32) {
        Some(a) => a,
        None => {
            let mon = app.monitor_from_point(cursor.x, cursor.y).ok().flatten();
            match mon {
                Some(m) => {
                    let p = m.position();
                    let s = m.size();
                    (p.x, p.y, p.x + s.width as i32, p.y + s.height as i32)
                }
                None => return,
            }
        }
    };
    let (left, top, right, bottom) = area;

    let (x, y) = match anchor {
        Anchor::TopRight => clamp_into(right - w - margin, top + margin, w, h, area, margin),
        Anchor::Center => clamp_into(
            left + (right - left - w) / 2,
            top + (bottom - top - h) / 2,
            w,
            h,
            area,
            margin,
        ),
        Anchor::Custom if saved.0 != 0 || saved.1 != 0 => {
            clamp_into(saved.0, saved.1, w, h, area, margin)
        }
        Anchor::Custom => clamp_into(right - w - margin, bottom - h - margin, w, h, area, margin),
        Anchor::BottomRight => clamp_into(right - w - margin, bottom - h - margin, w, h, area, margin),
        // Cursor: open right under the pointer, flip up/left when it would
        // run past the edge of the work area.
        Anchor::Cursor => {
            let cx = cursor.x as i32;
            let cy = cursor.y as i32;
            let dx = (12.0 * scale) as i32;
            let dy = (10.0 * scale) as i32;
            let x = if cx + w - dx > right - margin { cx - w + dx } else { cx - dx };
            let y = if cy + h - dy > bottom - margin { cy - h + dy } else { cy - dy };
            clamp_into(x, y, w, h, area, margin)
        }
    };

    *state.prog_move.lock().unwrap() = Instant::now();
    let _ = win.set_position(PhysicalPosition::new(x, y));
}

/// Windows refuses SetForegroundWindow for a process that does not own the
/// current foreground; attaching to the foreground thread's input queue is the
/// reliable way to grab keyboard focus for a hotkey popup.
fn force_foreground(win: &tauri::WebviewWindow) {
    use windows::Win32::Foundation::BOOL;
    use windows::Win32::System::Threading::AttachThreadInput;
    use windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow;
    use windows::Win32::UI::WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SetForegroundWindow,
    };
    let Ok(hwnd) = win.hwnd() else { return };
    let hwnd = windows::Win32::Foundation::HWND(hwnd.0);
    unsafe {
        let fg = GetForegroundWindow();
        let fg_thread = GetWindowThreadProcessId(fg, None);
        let me = windows::Win32::System::Threading::GetCurrentThreadId();
        if fg_thread != 0 && fg_thread != me {
            let _ = AttachThreadInput(me, fg_thread, BOOL(1));
        }
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetActiveWindow(hwnd);
        if fg_thread != 0 && fg_thread != me {
            let _ = AttachThreadInput(me, fg_thread, BOOL(0));
        }
    }
}

/// Ask for the foreground a few more times; Windows sometimes refuses the first
/// attempt when another process is holding the input queue. The first attempt
/// in `show_popup` runs inline, so these are only a cheap safety net and start
/// late enough not to compete with the initial show.
fn activate_retry(win: tauri::WebviewWindow) {
    std::thread::spawn(move || {
        for delay in [45u64, 110] {
            std::thread::sleep(Duration::from_millis(delay));
            if win.is_focused().unwrap_or(false) {
                break;
            }
            let w = win.clone();
            let app = win.app_handle().clone();
            let _ = app.run_on_main_thread(move || force_foreground(&w));
        }
    });
}

/// True when the mouse pointer sits over the popup — used so a stray focus
/// change does not close the window under the user's cursor.
fn cursor_over(win: &tauri::WebviewWindow) -> bool {
    let Ok(cursor) = win.cursor_position() else { return false };
    let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) else {
        return false;
    };
    let (x, y) = (cursor.x as i32, cursor.y as i32);
    x >= pos.x
        && y >= pos.y
        && x <= pos.x + size.width as i32
        && y <= pos.y + size.height as i32
}

fn hide_popup_from(app: &AppHandle, why: &str) {
    log(&format!("hide popup <- {}", why));
    hide_popup(app);
}

/// Records the window's current logical size so the next launch reopens at the
/// size the user left it at. `force` flushes history.json right away, otherwise
/// the write is throttled — a drag-resize fires an event per pixel.
fn remember_size(app: &AppHandle, win: &tauri::WebviewWindow, force: bool) {
    let Ok(size) = win.outer_size() else { return };
    let scale = win.scale_factor().unwrap_or(1.0);
    if scale <= 0.0 {
        return;
    }
    let lw = size.width as f64 / scale;
    let lh = size.height as f64 / scale;
    let state = app.state::<AppState>();
    let mut s = state.store.lock().unwrap();
    match win.label() {
        "main" => {
            s.settings.popup_w = lw;
            s.settings.popup_h = lh;
        }
        "settings" => {
            s.settings.settings_w = lw;
            s.settings.settings_h = lh;
        }
        _ => return,
    }
    state.size_dirty.store(true, Ordering::Relaxed);
    let mut t = state.size_save.lock().unwrap();
    if force || t.elapsed() >= Duration::from_millis(700) {
        *t = Instant::now();
        s.save();
        state.size_dirty.store(false, Ordering::Relaxed);
    }
}

fn flush_size(app: &AppHandle, label: &str) {
    let state = app.state::<AppState>();
    if !state.size_dirty.load(Ordering::Relaxed) {
        return;
    }
    if let Some(win) = app.get_webview_window(label) {
        // The window is already hidden at this point, but its geometry is
        // still valid.
        remember_size(app, &win, true);
    }
}

fn hide_popup(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.hide();
    }
    let state = app.state::<AppState>();
    {
        // Flush any position that the drag throttle skipped.
        let s = state.store.lock().unwrap();
        if s.settings.position == "custom" {
            let mut guard = state.pos_save.lock().unwrap();
            if guard.1 != Some((s.settings.pos_x, s.settings.pos_y)) {
                guard.1 = Some((s.settings.pos_x, s.settings.pos_y));
                s.save();
            }
        }
    }
    flush_size(app, "main");
    *state.hide_at.lock().unwrap() = Some(Instant::now());
}

fn toggle_popup(app: &AppHandle, from_hotkey: bool) {
    let state = app.state::<AppState>();
    if from_hotkey {
        let mut t = state.last_hotkey.lock().unwrap();
        // RegisterHotKey posts exactly one WM_HOTKEY per press, so this only
        // guards against a duplicated event. It must stay short: a longer
        // window makes "press again to close" feel dead.
        if t.elapsed() < Duration::from_millis(60) {
            return;
        }
        *t = Instant::now();
    }
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    let visible = win.is_visible().unwrap_or(false);
    log(&format!("toggle from_hotkey={} visible={}", from_hotkey, visible));
    if visible && !win.is_focused().unwrap_or(false) {
        // Still on screen but another window owns the focus (keep-on-screen
        // mode): pressing the hotkey should bring it back rather than close it.
        log("toggle -> reactivate");
        *state.hide_at.lock().unwrap() = None;
        let w = win.clone();
        let h = app.clone();
        let _ = h.run_on_main_thread(move || force_foreground(&w));
        activate_retry(win);
        return;
    }
    if visible {
        if !from_hotkey {
            if let Some(t) = *state.hide_at.lock().unwrap() {
                if t.elapsed() < Duration::from_millis(320) {
                    return;
                }
            }
        }
        hide_popup_from(app, "toggle");
    } else {
        show_popup(app);
    }
}

#[derive(Clone, Serialize)]
struct ThemePayload {
    theme: String,
    accent: String,
}

fn show_popup(app: &AppHandle) {
    let state = app.state::<AppState>();
    let Some(win) = app.get_webview_window("main") else {
        return;
    };
    position_popup(app, &win);
    *state.shown_at.lock().unwrap() = Some(Instant::now());
    *state.focused_once.lock().unwrap() = false;
    *state.hide_at.lock().unwrap() = None;

    // 1. Let the page rebuild itself while the window is still invisible, so
    //    the first frame that reaches the screen is already correct — one
    //    composite instead of "old contents, then new contents".
    //    The theme travels with the event: reading it through a second IPC hop
    //    would land a frame late (and previously meant a stale colour on the
    //    first paint after the user changed the accent).
    let (theme, accent) = {
        let s = state.store.lock().unwrap();
        (s.settings.theme.clone(), s.settings.accent.clone())
    };
    let _ = win.emit("popup-shown", ThemePayload { theme, accent });

    // 2. Show + activate in a single hop on the UI thread. Order matters:
    //    SetForegroundWindow must run after the window is actually visible,
    //    and from the thread that owns it. Calling it straight from the
    //    hotkey thread (as before) ran it before the queued show() and was
    //    silently wasted.
    let w = win.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = w.show();
        force_foreground(&w);
        let _ = w.set_focus();
        // Focus the search box only once the window really is active — while
        // it is hidden the DOM focus may not stick.
        let _ = w.emit("popup-active", ());
        log("shown");
        activate_retry(w.clone());
    });
}

fn open_settings(app: &AppHandle) {
    // The popup is always-on-top and (in "keep on screen" mode) may be sitting
    // on screen right now; it would simply cover the settings window.
    if let Some(p) = app.get_webview_window("main") {
        if p.is_visible().unwrap_or(false) {
            hide_popup_from(app, "open_settings");
        }
    }
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.center();
        let _ = win.show();
        let _ = win.set_focus();
        let _ = win.emit("settings-shown", ());
    }
}

// ------------------------------------------------------------- clipboard flow

fn spawn_watcher(app: AppHandle) {
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_millis(150));
        let state = app.state::<AppState>();
        let seq = clipboard::sequence();
        if seq == 0 {
            continue;
        }
        let monitoring = {
            let s = state.store.lock().unwrap();
            s.settings.monitoring
        };
        if !monitoring {
            state.last_seq.store(seq, Ordering::Relaxed);
            continue;
        }
        if seq == state.last_seq.load(Ordering::Relaxed) {
            continue;
        }
        state.last_seq.store(seq, Ordering::Relaxed);
        if let Some(clip) = clipboard::read() {
            ingest(&app, clip);
        }
    });
}

fn ingest(app: &AppHandle, clip: clipboard::Clip) {
    let state = app.state::<AppState>();
    let id = store::new_id();
    let dir: PathBuf = {
        let s = state.store.lock().unwrap();
        s.dir.clone()
    };

    let mut file: Option<String> = None;
    let mut thumb: Option<String> = None;
    let mut size: u64 = clip.text.len() as u64;

    if let Some(png) = &clip.png {
        let path = dir.join("images").join(format!("{}.png", id));
        if std::fs::write(&path, png).is_ok() {
            file = Some(path.to_string_lossy().to_string());
            size = png.len() as u64;
        }
        thumb = clipboard::thumbnail(png, 240);
    }

    let hash = store::hash_of(&clip.kind, &clip.text, clip.png.as_deref(), &clip.files);

    let item = Item {
        id,
        kind: clip.kind,
        text: clip.text,
        html: clip.html,
        file,
        thumb,
        files: clip.files,
        pinned: false,
        created_at: store::now_ms(),
        size,
        hash,
    };

    {
        let mut s = state.store.lock().unwrap();
        s.add(item);
    }
    let _ = app.emit("history-changed", ());
}

fn spawn_foreground_tracker(app: AppHandle) {
    std::thread::spawn(move || {
        let me = unsafe { windows::Win32::System::Threading::GetCurrentProcessId() };
        loop {
            std::thread::sleep(Duration::from_millis(180));
            unsafe {
                let h = windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow();
                if h.0 as isize == 0 {
                    continue;
                }
                let mut pid = 0u32;
                windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                    h,
                    Some(&mut pid),
                );
                if pid != 0 && pid != me {
                    app.state::<AppState>()
                        .foreground
                        .store(h.0 as isize, Ordering::Relaxed);
                }
            }
        }
    });
}

fn send_ctrl_v() {
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYBD_EVENT_FLAGS, VK_CONTROL, VK_V,
    };
    let key = |vk, up: bool| {
        let mut input = INPUT::default();
        input.r#type = INPUT_KEYBOARD;
        input.Anonymous = INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: 0,
                dwFlags: if up {
                    windows::Win32::UI::Input::KeyboardAndMouse::KEYEVENTF_KEYUP
                } else {
                    KEYBD_EVENT_FLAGS(0)
                },
                time: 0,
                dwExtraInfo: 0,
            },
        };
        input
    };
    let inputs = [key(VK_CONTROL, false), key(VK_V, false), key(VK_V, true), key(VK_CONTROL, true)];
    unsafe {
        SendInput(&inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn paste_into_previous(app: AppHandle) {
    let target = app.state::<AppState>().foreground.load(Ordering::Relaxed);
    hide_popup_from(&app, "paste");
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(70));
        if target != 0 {
            unsafe {
                let hwnd = windows::Win32::Foundation::HWND(target as _);
                let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(hwnd);
            }
        }
        std::thread::sleep(Duration::from_millis(80));
        send_ctrl_v();
    });
}

// ------------------------------------------------------------------- commands

#[tauri::command]
async fn list_items(state: State<'_, AppState>) -> Result<Vec<Item>, String> {
    // Async so the clone + serialisation never runs on the UI thread, where it
    // would compete with the first paint of the popup.
    Ok(state.store.lock().unwrap().items.clone())
}

#[tauri::command]
fn get_settings(state: State<AppState>) -> Settings {
    state.store.lock().unwrap().settings.clone()
}

#[tauri::command]
fn save_settings(app: AppHandle, settings: Settings) -> Result<Settings, String> {
    let state = app.state::<AppState>();
    let old = state.store.lock().unwrap().settings.clone();

    if old.hotkey != settings.hotkey {
        let new_sc: Shortcut = settings
            .hotkey
            .parse()
            .map_err(|_| "热键格式无效".to_string())?;
        if let Ok(old_sc) = old.hotkey.parse::<Shortcut>() {
            let _ = app.global_shortcut().unregister(old_sc);
        }
        if app.global_shortcut().register(new_sc).is_err() {
            if let Ok(old_sc) = old.hotkey.parse::<Shortcut>() {
                let _ = app.global_shortcut().register(old_sc);
            }
            return Err("该热键已被其他程序占用，请换一个".into());
        }
    }

    {
        let mut s = state.store.lock().unwrap();
        s.settings = settings.clone();
        // Shrinking the limit takes effect right away instead of waiting for
        // the next copy.
        s.trim();
        s.save();
    }
    // Lets the popup pick up a theme change without reopening the settings.
    let _ = app.emit("settings-changed", ());

    // Turning "keep on screen" off while the popup is up but unfocused should
    // take effect immediately instead of waiting for the next blur.
    if old.keep_open && !settings.keep_open {
        if let Some(win) = app.get_webview_window("main") {
            if win.is_visible().unwrap_or(false) && !win.is_focused().unwrap_or(false) {
                hide_popup_from(&app, "keep_open off");
            }
        }
    }
    Ok(settings)
}

#[tauri::command]
fn copy_item(app: AppHandle, id: String, plain: bool, paste: bool) -> Result<(), String> {
    let state = app.state::<AppState>();
    let item = {
        let s = state.store.lock().unwrap();
        s.items.iter().find(|i| i.id == id).cloned()
    };
    let Some(item) = item else {
        return Err("条目已不存在".into());
    };

    let png = item.file.as_ref().and_then(|p| std::fs::read(p).ok());
    let payload = clipboard::WritePayload {
        text: &item.text,
        html: if plain || item.kind == "image" {
            None
        } else {
            item.html.as_deref()
        },
        png: png.as_deref(),
        files: &item.files,
    };
    if !clipboard::write(&payload) {
        return Err("写入剪贴板失败".into());
    }
    // Do not record our own write as a brand new entry.
    state
        .last_seq
        .store(clipboard::sequence(), Ordering::Relaxed);

    if paste {
        paste_into_previous(app);
    } else if state.store.lock().unwrap().settings.keep_open {
        // "Keep on screen" mode: the write already succeeded, so leave the
        // window up and let the page show a short confirmation instead.
        log("copy kept popup open");
        let _ = app.emit("item-copied", ());
    } else {
        hide_popup_from(&app, "copy_text");
    }
    Ok(())
}

#[tauri::command]
fn toggle_pin(app: AppHandle, state: State<AppState>, id: String) -> Result<bool, String> {
    let pinned = state.store.lock().unwrap().toggle_pin(&id);
    let _ = app.emit("history-changed", ());
    pinned.ok_or_else(|| "条目已不存在".into())
}

#[tauri::command]
fn delete_item(app: AppHandle, state: State<AppState>, id: String) -> Result<(), String> {
    state.store.lock().unwrap().remove(&id);
    let _ = app.emit("history-changed", ());
    Ok(())
}

#[tauri::command]
fn clear_history(app: AppHandle, state: State<AppState>, include_pinned: bool) -> Result<(), String> {
    state.store.lock().unwrap().clear(include_pinned);
    let _ = app.emit("history-changed", ());
    Ok(())
}

#[tauri::command]
fn set_monitoring(app: AppHandle, state: State<AppState>, enabled: bool) -> bool {
    {
        let mut s = state.store.lock().unwrap();
        s.settings.monitoring = enabled;
        s.save();
    }
    if let Some(item) = state.pause_item.lock().unwrap().as_ref() {
        let _ = item.set_checked(!enabled);
    }
    if enabled {
        state
            .last_seq
            .store(clipboard::sequence(), Ordering::Relaxed);
    }
    let _ = app.emit("history-changed", ());
    enabled
}

#[tauri::command]
fn hide_popup_cmd(app: AppHandle) {
    hide_popup_from(&app, "cmd");
}

/// Diagnostics only: lets the page drop timestamps into the same log file so
/// the frontend half of the open/close latency can be measured.
#[tauri::command]
fn perf(msg: String) {
    log(&format!("js {}", msg));
}

#[tauri::command]
fn show_settings(app: AppHandle) {
    open_settings(&app);
}

#[tauri::command]
fn close_settings(app: AppHandle) {
    if let Some(win) = app.get_webview_window("settings") {
        let _ = win.hide();
    }
    flush_size(&app, "settings");
}

#[tauri::command]
fn quit_app(app: AppHandle) {
    app.exit(0);
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<bool, String> {
    use tauri_plugin_autostart::ManagerExt;
    let manager = app.autolaunch();
    let res = if enabled {
        manager.enable()
    } else {
        manager.disable()
    };
    res.map_err(|e| e.to_string())?;
    Ok(manager.is_enabled().unwrap_or(enabled))
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    use tauri_plugin_autostart::ManagerExt;
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn open_data_dir(app: AppHandle) -> Result<(), String> {
    let dir = {
        let state = app.state::<AppState>();
        let s = state.store.lock().unwrap();
        s.dir.clone()
    };
    std::process::Command::new("explorer")
        .arg(dir)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ----------------------------------------------------------------- entry point

fn main() {
    tauri::Builder::default()
        // Must be the first plugin: a second launch then hands control to the
        // running instance instead of spawning a duplicate tray icon.
        .plugin(tauri_plugin_single_instance::init(|app, _argv, _cwd| {
            log("second instance -> show popup");
            show_popup(app);
        }))
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state() == ShortcutState::Pressed {
                        toggle_popup(app, true);
                    }
                })
                .build(),
        )
        .manage(AppState::new())
        .invoke_handler(tauri::generate_handler![
            list_items,
            get_settings,
            save_settings,
            copy_item,
            toggle_pin,
            delete_item,
            clear_history,
            set_monitoring,
            hide_popup_cmd,
            perf,
            show_settings,
            close_settings,
            quit_app,
            set_autostart,
            get_autostart,
            open_data_dir
        ])
        .setup(|app| {
            let handle = app.handle().clone();

            // data dir + persisted history
            let dir = app.path().app_data_dir()?;
            store::migrate_legacy(&dir);
            let store = Store::load(dir);
            let settings = store.settings.clone();
            {
                let state = app.state::<AppState>();
                *state.store.lock().unwrap() = store;
                state
                    .last_seq
                    .store(clipboard::sequence(), Ordering::Relaxed);
            }

            // global hotkey
            if let Ok(sc) = settings.hotkey.parse::<Shortcut>() {
                let _ = app.global_shortcut().register(sc);
            }

            // Both windows are resizable; reopen at the size the user left.
            for (label, w, h) in [
                ("main", settings.popup_w, settings.popup_h),
                ("settings", settings.settings_w, settings.settings_h),
            ] {
                if w > 0.0 && h > 0.0 {
                    if let Some(win) = app.get_webview_window(label) {
                        let _ = win.set_size(tauri::LogicalSize::new(w, h));
                    }
                }
            }

            // tray
            let open_i = MenuItem::with_id(app, "open", "打开 Teecy", true, None::<&str>)?;
            let settings_i = MenuItem::with_id(app, "settings", "设置…", true, None::<&str>)?;
            let pause_i =
                CheckMenuItem::with_id(app, "pause", "暂停记录", true, !settings.monitoring, None::<&str>)?;
            let clear_i = MenuItem::with_id(app, "clear", "清空历史（保留固定项）", true, None::<&str>)?;
            let clear_all_i =
                MenuItem::with_id(app, "clear_all", "清空全部历史", true, None::<&str>)?;
            let quit_i = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(
                app,
                &[
                    &open_i,
                    &settings_i,
                    &PredefinedMenuItem::separator(app)?,
                    &pause_i,
                    &clear_i,
                    &clear_all_i,
                    &PredefinedMenuItem::separator(app)?,
                    &quit_i,
                ],
            )?;

            {
                let state = app.state::<AppState>();
                *state.pause_item.lock().unwrap() = Some(pause_i.clone());
            }

            let icon = app
                .default_window_icon()
                .cloned()
                .ok_or("missing window icon")?;

            TrayIconBuilder::with_id("teecy")
                .icon(icon)
                .tooltip("Teecy · 剪贴板历史")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_popup(tray.app_handle(), false);
                    }
                })
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => toggle_popup(app, false),
                    "settings" => open_settings(app),
                    "pause" => {
                        let state = app.state::<AppState>();
                        let enabled = {
                            let s = state.store.lock().unwrap();
                            s.settings.monitoring
                        };
                        let next = !enabled;
                        {
                            let mut s = state.store.lock().unwrap();
                            s.settings.monitoring = next;
                            s.save();
                        }
                        if let Some(item) = state.pause_item.lock().unwrap().as_ref() {
                            let _ = item.set_checked(!next);
                        }
                        let _ = app.emit("history-changed", ());
                    }
                    "clear" => {
                        app.state::<AppState>().store.lock().unwrap().clear(false);
                        let _ = app.emit("history-changed", ());
                    }
                    "clear_all" => {
                        app.state::<AppState>().store.lock().unwrap().clear(true);
                        let _ = app.emit("history-changed", ());
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .build(app)?;

            spawn_watcher(handle.clone());
            spawn_foreground_tracker(handle.clone());

            // handy while developing: TEECY_SHOW=1 opens the popup immediately
            if std::env::var("TEECY_SHOW").is_ok() {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(400));
                    show_popup(&h);
                });
            }
            if std::env::var("TEECY_TOGGLE").is_ok() {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(900));
                    log("debug toggle (hotkey path)");
                    toggle_popup(&h, true);
                });
            }
            if std::env::var("TEECY_BENCH").is_ok() {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(1200));
                    for i in 0..6 {
                        log(&format!("bench #{} (same code path as the hotkey)", i));
                        toggle_popup(&h, true);
                        std::thread::sleep(Duration::from_millis(900));
                    }
                });
            }
            if std::env::var("TEECY_SETTINGS").is_ok() {
                let h = handle.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(600));
                    open_settings(&h);
                });
            }

            Ok(())
        })
        .on_window_event(|window, event| match event {
            WindowEvent::Focused(true) => {
                if window.label() == "main" {
                    *window
                        .app_handle()
                        .state::<AppState>()
                        .focused_once
                        .lock()
                        .unwrap() = true;
                }
            }
            WindowEvent::Focused(false) => {
                if window.label() != "main" || std::env::var("TEECY_HOLD").is_ok() {
                    return;
                }
                let app = window.app_handle();
                let state = app.state::<AppState>();
                if state.store.lock().unwrap().settings.keep_open {
                    // Pinned to the desktop: losing focus must not close it.
                    log("blur ignored (keep_open)");
                    return;
                }
                if !*state.focused_once.lock().unwrap() {
                    // Activation never landed — keep the window up and try
                    // again instead of closing it under the user.
                    log("blur ignored (never focused)");
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = app.run_on_main_thread(move || force_foreground(&w));
                    }
                    return;
                }
                let fresh = state
                    .shown_at
                    .lock()
                    .unwrap()
                    .map(|t| t.elapsed() < Duration::from_millis(600))
                    .unwrap_or(false);
                if fresh {
                    // Just opened: Windows may bounce the focus for a moment.
                    log("blur ignored (grace)");
                    if let Some(w) = app.get_webview_window("main") {
                        force_foreground(&w);
                    }
                    return;
                }
                // Confirm 180 ms later, so a single stray blur does not close
                // the popup while the user is still reaching for it.
                let generation = {
                    let mut g = state.blur_gen.lock().unwrap();
                    *g += 1;
                    *g
                };
                let app = app.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(180));
                    let state = app.state::<AppState>();
                    if *state.blur_gen.lock().unwrap() != generation {
                        return;
                    }
                    let Some(w) = app.get_webview_window("main") else { return };
                    if !w.is_visible().unwrap_or(false) || w.is_focused().unwrap_or(false) {
                        return;
                    }
                    if cursor_over(&w) {
                        log("blur ignored (cursor inside)");
                        return;
                    }
                    log("blur -> hide");
                    hide_popup_from(&app, "blur");
                });
            }
            WindowEvent::Resized(_) => {
                // Both windows are freely resizable; remember the geometry so
                // the next launch (and every later popup) uses it.
                if !window.is_visible().unwrap_or(false) {
                    return;
                }
                let app = window.app_handle();
                if let Some(w) = app.get_webview_window(window.label()) {
                    remember_size(app, &w, false);
                }
            }
            WindowEvent::Moved(_) => {
                if window.label() != "main" {
                    return;
                }
                let app = window.app_handle();
                let state = app.state::<AppState>();
                if state.prog_move.lock().unwrap().elapsed() < Duration::from_millis(400) {
                    return;
                }
                if !window.is_visible().unwrap_or(false) {
                    return;
                }
                // A real drag: remember where the user put it.
                if let Ok(p) = window.outer_position() {
                    let mut announce = false;
                    {
                        let mut s = state.store.lock().unwrap();
                        if s.settings.position != "custom" {
                            s.settings.position = "custom".into();
                            announce = true;
                        }
                        if s.settings.pos_x != p.x || s.settings.pos_y != p.y {
                            s.settings.pos_x = p.x;
                            s.settings.pos_y = p.y;
                            // Dragging fires an event per pixel — only flush the
                            // history file now and then, the last value is saved
                            // when the popup closes.
                            let mut guard = state.pos_save.lock().unwrap();
                            if guard.0.elapsed() >= Duration::from_millis(700) {
                                guard.0 = Instant::now();
                                s.save();
                                guard.1 = Some((p.x, p.y));
                            }
                        }
                    }
                    if announce {
                        log(&format!("custom position {},{}", p.x, p.y));
                        let _ = app.emit("settings-changed", ());
                    }
                }
            }
            WindowEvent::CloseRequested { api, .. } => {
                api.prevent_close();
                let _ = window.hide();
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("error while building Teecy")
        .run(|_app, event| {
            if let RunEvent::ExitRequested { api, .. } = event {
                api.prevent_exit();
            }
        });
}
