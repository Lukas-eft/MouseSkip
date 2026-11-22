#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod icon;
mod model;
mod raw_input;
mod storage;

use crate::model::{ConfigPatch, SharedAppState, SkipKind, TimeRange};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::borrow::Cow;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{ResizeDirection, Window, WindowBuilder};
use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use wry::http::{header::CONTENT_TYPE, Method, Request, Response, StatusCode};
use wry::{NewWindowResponse, WebViewBuilder};

const INDEX_HTML: &str = include_str!("ui/index.html");
const CSP_HEADER: &str = "default-src 'self' app: http://app.localhost https://app.localhost; style-src 'self' 'unsafe-inline'; script-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' app: http://app.localhost https://app.localhost; font-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none';";

#[derive(Debug, Clone)]
enum UserEvent {
    Tray(TrayIconEvent),
    Menu(MenuEvent),
    DragWindow,
    MinimizeWindow,
    ToggleMaximize,
    HideToTray,
    ResizeWindow(ResizeDirection),
    ExitApp,
}

fn parse_resize_direction(dir: &str) -> Option<ResizeDirection> {
    match dir {
        "North" => Some(ResizeDirection::North),
        "South" => Some(ResizeDirection::South),
        "East" => Some(ResizeDirection::East),
        "West" => Some(ResizeDirection::West),
        "NorthEast" => Some(ResizeDirection::NorthEast),
        "NorthWest" => Some(ResizeDirection::NorthWest),
        "SouthEast" => Some(ResizeDirection::SouthEast),
        "SouthWest" => Some(ResizeDirection::SouthWest),
        _ => None,
    }
}

fn parse_query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v);
            }
        }
    }
    None
}

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<Cow<'static, [u8]>> {
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "application/json; charset=utf-8")
        .header("Cache-Control", "no-store")
        .header("Content-Security-Policy", CSP_HEADER)
        .body(Cow::Owned(body))
        .unwrap()
}

fn handle_custom_protocol(
    request: Request<Vec<u8>>,
    state: &Arc<SharedAppState>,
    store_path: &std::path::Path,
) -> Response<Cow<'static, [u8]>> {
    let uri = request.uri();
    let path = uri.path();
    let query = uri.query().unwrap_or("");
    let method = request.method();

    if method == Method::GET && (path == "/" || path == "/index.html") {
        return Response::builder()
            .status(StatusCode::OK)
            .header(CONTENT_TYPE, "text/html; charset=utf-8")
            .header("Content-Security-Policy", CSP_HEADER)
            .body(Cow::Borrowed(INDEX_HTML.as_bytes()))
            .unwrap();
    }

    if method == Method::GET && path == "/api/dashboard" {
        let range = parse_query_param(query, "range")
            .map(TimeRange::parse)
            .unwrap_or(TimeRange::Last15m);
        let kind_filter = parse_query_param(query, "kind").and_then(SkipKind::parse_filter);
        let now_epoch_ms = chrono::Utc::now().timestamp_millis();

        let resp = match state.snapshot_dashboard(range, kind_filter, now_epoch_ms) {
            Ok(r) => r,
            Err(_) => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    br#"{"error":"state_locked"}"#.to_vec(),
                );
            }
        };

        let bytes = serde_json::to_vec(&resp).unwrap_or_else(|_| b"{}".to_vec());
        return json_response(StatusCode::OK, bytes);
    }

    if method == Method::POST && path == "/api/settings" {
        let Ok(patch) = serde_json::from_slice::<ConfigPatch>(request.body()) else {
            return json_response(
                StatusCode::BAD_REQUEST,
                br#"{"error":"invalid_json"}"#.to_vec(),
            );
        };

        let updated_cfg = if let Ok(mut cfg) = state.config.write() {
            if let Some(v) = patch.dropout_threshold_ms {
                cfg.dropout_threshold_ms = v;
            }
            if let Some(v) = patch.spinout_threshold_px {
                cfg.spinout_threshold_px = v;
            }
            if let Some(v) = patch.active_pause_cutoff_ms {
                cfg.active_pause_cutoff_ms = v;
            }
            if let Some(v) = patch.monitoring_enabled {
                cfg.monitoring_enabled = v;
            }
            cfg.clamp_in_place();
            cfg.clone()
        } else {
            return json_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                br#"{"error":"state_locked"}"#.to_vec(),
            );
        };

        state.mark_dirty();

        let bytes = serde_json::to_vec(&updated_cfg).unwrap_or_else(|_| b"{}".to_vec());
        return json_response(StatusCode::OK, bytes);
    }

    if method == Method::POST && path == "/api/reset" {
        if let Ok(mut events) = state.events.lock() {
            events.clear();
        }
        storage::flush_store_now(state, store_path);
        return json_response(StatusCode::OK, br#"{"ok":true}"#.to_vec());
    }

    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Cow::Borrowed(b"Not Found".as_slice()))
        .unwrap()
}

#[cfg(windows)]
fn apply_rounded_corners(window: &Window) {
    use windows_sys::Win32::Graphics::Dwm::{
        DwmSetWindowAttribute, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
    };

    if let Ok(handle) = window.window_handle() {
        if let RawWindowHandle::Win32(win32) = handle.as_raw() {
            let hwnd = win32.hwnd.get() as windows_sys::Win32::Foundation::HWND;
            let pref = DWMWCP_ROUND;
            unsafe {
                let _ = DwmSetWindowAttribute(
                    hwnd,
                    DWMWA_WINDOW_CORNER_PREFERENCE as u32,
                    &pref as *const i32 as *const core::ffi::c_void,
                    std::mem::size_of::<i32>() as u32,
                );
            }
        }
    }
}

#[cfg(not(windows))]
fn apply_rounded_corners(_window: &Window) {}

fn apply_mica_backdrop(window: &Window) {
    #[cfg(windows)]
    {
        if window_vibrancy::apply_mica(window, Some(true)).is_err() {
            let _ = window_vibrancy::apply_acrylic(window, Some((9, 9, 11, 165)));
        }
    }
    #[cfg(not(windows))]
    {
        let _ = window;
    }
}

fn show_dashboard_window(window: &Window, webview: &wry::WebView) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
    let _ = webview.evaluate_script("window.__onWindowRestored && window.__onWindowRestored();");
}

fn hide_dashboard_to_tray(window: &Window, webview: &wry::WebView) {
    window.set_visible(false);
    let _ = webview.evaluate_script("window.__onWindowHidden && window.__onWindowHidden();");
}

fn run_smoke_test() -> Result<(), String> {
    let store_path = storage::default_store_path();
    let persisted = storage::load_store(&store_path);
    let state = SharedAppState::new(persisted.config, persisted.events);

    let guard = raw_input::start_raw_input_listener(Arc::clone(&state))?;
    if !state.raw_input_registered.load(Ordering::Acquire) {
        return Err("Raw Input device registration flag was false".to_string());
    }
    if !guard.verify_mouse_sink_registered() {
        return Err("GetRegisteredRawInputDevices did not point to MouseSkip sink".to_string());
    }

    let now_epoch_ms = chrono::Utc::now().timestamp_millis();
    let resp = state
        .snapshot_dashboard(TimeRange::Last15m, None, now_epoch_ms)
        .map_err(|e| format!("Failed to snapshot dashboard: {e}"))?;
    if !resp.live.raw_input_active {
        return Err("Raw Input active status was false".to_string());
    }

    guard.shutdown();
    println!("SMOKE_TEST_OK");
    Ok(())
}

fn main() {
    if std::env::args().any(|a| a == "--smoke-test") {
        if let Err(err) = run_smoke_test() {
            eprintln!("SMOKE_TEST_FAILED: {err}");
            std::process::exit(1);
        }
        return;
    }

    let store_path = storage::default_store_path();
    let persisted = storage::load_store(&store_path);

    let state = SharedAppState::new(persisted.config, persisted.events);
    let mut persistence_guard =
        Some(storage::spawn_persistence_worker(Arc::clone(&state), store_path.clone()));

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    #[allow(unused_mut)]
    let mut win_builder = WindowBuilder::new()
        .with_title("MouseSkip — Raw Input Monitor")
        .with_decorations(false)
        .with_transparent(true)
        .with_inner_size(LogicalSize::new(1120.0, 770.0))
        .with_min_inner_size(LogicalSize::new(880.0, 620.0));

    if let Ok(win_icon) = icon::create_window_icon() {
        win_builder = win_builder.with_window_icon(Some(win_icon));
    }

    #[cfg(windows)]
    {
        use tao::platform::windows::WindowBuilderExtWindows;
        win_builder = win_builder.with_undecorated_shadow(false);
    }

    let window = win_builder.build(&event_loop).expect("Failed to create window");

    apply_mica_backdrop(&window);
    apply_rounded_corners(&window);

    #[cfg(windows)]
    {
        use tao::platform::windows::WindowExtWindows;
        window.set_undecorated_shadow(true);
    }

    // System Tray Menu
    let tray_menu = Menu::new();
    let item_open = MenuItem::with_id("open", "Open Dashboard", true, None);
    let item_sep = PredefinedMenuItem::separator();
    let item_quit = MenuItem::with_id("quit", "Quit MouseSkip", true, None);

    let _ = tray_menu.append_items(&[&item_open, &item_sep, &item_quit]);

    let tray_proxy = proxy.clone();
    TrayIconEvent::set_event_handler(Some(move |event| {
        let _ = tray_proxy.send_event(UserEvent::Tray(event));
    }));

    let menu_proxy = proxy.clone();
    MenuEvent::set_event_handler(Some(move |event| {
        let _ = menu_proxy.send_event(UserEvent::Menu(event));
    }));

    let tray_icon_image = icon::create_tray_icon().expect("Failed to render tray icon");
    let _tray_icon = TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_menu_on_left_click(false)
        .with_tooltip("MouseSkip — Raw Input Monitor (Click to open)")
        .with_icon(tray_icon_image)
        .build()
        .expect("Failed to build tray icon");

    // WebView2 with transparent backdrop and custom app:// protocol
    let protocol_state = Arc::clone(&state);
    let protocol_store_path = store_path.clone();
    let ipc_proxy = proxy.clone();

    let webview = WebViewBuilder::new()
        .with_transparent(true)
        .with_custom_protocol("app".into(), move |_id, request| {
            handle_custom_protocol(request, &protocol_state, &protocol_store_path)
        })
        .with_navigation_handler(|url| {
            url.starts_with("app://localhost")
                || url.starts_with("http://app.localhost")
                || url.starts_with("https://app.localhost")
        })
        .with_new_window_req_handler(|_, _| NewWindowResponse::Deny)
        .with_url("app://localhost/")
        .with_ipc_handler(move |req| {
            let body = req.body();
            if body == "drag_window" {
                let _ = ipc_proxy.send_event(UserEvent::DragWindow);
            } else if body == "minimize_window" {
                let _ = ipc_proxy.send_event(UserEvent::MinimizeWindow);
            } else if body == "toggle_maximize" {
                let _ = ipc_proxy.send_event(UserEvent::ToggleMaximize);
            } else if body == "hide_to_tray" {
                let _ = ipc_proxy.send_event(UserEvent::HideToTray);
            } else if let Some(dir_str) = body.strip_prefix("resize:") {
                if let Some(dir) = parse_resize_direction(dir_str) {
                    let _ = ipc_proxy.send_event(UserEvent::ResizeWindow(dir));
                }
            }
        })
        .build(&window)
        .expect("Failed to create WebView");

    // Start the WM_INPUT listener AFTER EventLoop/Window/WebView creation so tao's
    // default raw-input registration does not overwrite our RIDEV_INPUTSINK target.
    let mut raw_input_guard = match raw_input::start_raw_input_listener(Arc::clone(&state)) {
        Ok(guard) => Some(guard),
        Err(err) => {
            eprintln!("Warning: WM_INPUT Raw Input init error: {err}");
            None
        }
    };
    let mut webview = Some(webview);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } => {
                // Never terminate on close; minimize to Windows System Tray instead.
                if let Some(wv) = webview.as_ref() {
                    hide_dashboard_to_tray(&window, wv);
                }
            }
            Event::UserEvent(user_event) => match user_event {
                UserEvent::Tray(tray_ev) => match tray_ev {
                    TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    }
                    | TrayIconEvent::DoubleClick {
                        button: MouseButton::Left,
                        ..
                    } => {
                        if let Some(wv) = webview.as_ref() {
                            show_dashboard_window(&window, wv);
                        }
                    }
                    _ => {}
                },
                UserEvent::Menu(menu_ev) => match menu_ev.id.as_ref() {
                    "open" => {
                        if let Some(wv) = webview.as_ref() {
                            show_dashboard_window(&window, wv);
                        }
                    }
                    "quit" => {
                        let _ = proxy.send_event(UserEvent::ExitApp);
                    }
                    _ => {}
                },
                UserEvent::DragWindow => {
                    let _ = window.drag_window();
                }
                UserEvent::MinimizeWindow => {
                    window.set_minimized(true);
                }
                UserEvent::ToggleMaximize => {
                    window.set_maximized(!window.is_maximized());
                }
                UserEvent::HideToTray => {
                    if let Some(wv) = webview.as_ref() {
                        hide_dashboard_to_tray(&window, wv);
                    }
                }
                UserEvent::ResizeWindow(dir) => {
                    let _ = window.drag_resize_window(dir);
                }
                UserEvent::ExitApp => {
                    if let Some(p_guard) = persistence_guard.take() {
                        p_guard.shutdown();
                    }
                    storage::flush_store_now(&state, &store_path);
                    if let Some(guard) = raw_input_guard.take() {
                        guard.shutdown();
                    }
                    let _ = webview.take();
                    *control_flow = ControlFlow::Exit;
                }
            },
            _ => {}
        }
    });
}
