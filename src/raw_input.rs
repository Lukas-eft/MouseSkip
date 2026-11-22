use crate::model::SharedAppState;
use std::sync::atomic::Ordering;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Instant;

#[cfg(windows)]
use windows_sys::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
#[cfg(windows)]
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_TIME_CRITICAL,
};
#[cfg(windows)]
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRegisteredRawInputDevices, RegisterRawInputDevices, HRAWINPUT,
    MOUSE_MOVE_ABSOLUTE, RAWINPUT, RAWINPUTDEVICE, RAWINPUTHEADER, RIDEV_INPUTSINK, RID_INPUT,
    RIM_TYPEMOUSE,
};
#[cfg(windows)]
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    GetWindowLongPtrW, KillTimer, PostMessageW, PostQuitMessage, RegisterClassExW, SetTimer,
    SetWindowLongPtrW, TranslateMessage, GWLP_USERDATA, HWND_MESSAGE, MSG, WM_APP, WM_DESTROY,
    WM_INPUT, WM_TIMER, WNDCLASSEXW,
};

#[cfg(windows)]
const WM_APP_SHUTDOWN: u32 = WM_APP + 1;
#[cfg(windows)]
const TIMER_REASSERT_SINK_ID: usize = 1;

pub struct RawInputGuard {
    #[cfg(windows)]
    hwnd_raw: isize,
    join_handle: Option<thread::JoinHandle<()>>,
}

impl RawInputGuard {
    pub fn verify_mouse_sink_registered(&self) -> bool {
        #[cfg(windows)]
        {
            if self.hwnd_raw == 0 {
                return false;
            }
            is_mouse_sink_registered_to(self.hwnd_raw as HWND)
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    pub fn shutdown(mut self) {
        #[cfg(windows)]
        if self.hwnd_raw != 0 {
            unsafe {
                PostMessageW(self.hwnd_raw as HWND, WM_APP_SHUTDOWN, 0, 0);
            }
            self.hwnd_raw = 0;
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RawInputGuard {
    fn drop(&mut self) {
        #[cfg(windows)]
        if self.hwnd_raw != 0 {
            unsafe {
                PostMessageW(self.hwnd_raw as HWND, WM_APP_SHUTDOWN, 0, 0);
            }
            self.hwnd_raw = 0;
        }
        if let Some(handle) = self.join_handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(windows)]
fn to_wide_null(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn register_mouse_sink(hwnd: HWND) -> Result<(), u32> {
    let rid = RAWINPUTDEVICE {
        usUsagePage: 0x01,
        usUsage: 0x02,
        dwFlags: RIDEV_INPUTSINK,
        hwndTarget: hwnd,
    };

    let ok = unsafe {
        RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32)
    };
    if ok == 0 {
        Err(unsafe { GetLastError() })
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn is_mouse_sink_registered_to(hwnd: HWND) -> bool {
    let mut count: u32 = 0;
    let struct_size = std::mem::size_of::<RAWINPUTDEVICE>() as u32;
    let res = unsafe { GetRegisteredRawInputDevices(std::ptr::null_mut(), &mut count, struct_size) };
    if res == u32::MAX || count == 0 {
        return false;
    }

    let mut devices = vec![RAWINPUTDEVICE::default(); count as usize];
    let fetched =
        unsafe { GetRegisteredRawInputDevices(devices.as_mut_ptr(), &mut count, struct_size) };
    if fetched == u32::MAX {
        return false;
    }

    devices.iter().take(fetched as usize).any(|d| {
        d.usUsagePage == 0x01
            && d.usUsage == 0x02
            && (d.dwFlags & RIDEV_INPUTSINK) != 0
            && d.hwndTarget == hwnd
    })
}

#[cfg(windows)]
unsafe extern "system" fn raw_input_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_INPUT => {
            let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const SharedAppState;
            if !ptr.is_null() {
                let state = unsafe { &*ptr };
                handle_wm_input(lparam, state);
            }
            0
        }
        WM_TIMER => {
            if wparam == TIMER_REASSERT_SINK_ID {
                let active = is_mouse_sink_registered_to(hwnd) || register_mouse_sink(hwnd).is_ok();
                let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const SharedAppState;
                if !ptr.is_null() {
                    let state = unsafe { &*ptr };
                    state.live.set_raw_input_active(active);
                    state.raw_input_registered.store(active, Ordering::Release);
                }
            }
            0
        }
        WM_APP_SHUTDOWN => {
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            unsafe {
                KillTimer(hwnd, TIMER_REASSERT_SINK_ID);
                let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *const SharedAppState;
                if !ptr.is_null() {
                    drop(Arc::from_raw(ptr));
                }
                PostQuitMessage(0);
            }
            0
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

#[cfg(windows)]
fn handle_wm_input(lparam: LPARAM, state: &SharedAppState) {
    let mut buf = [0u64; 16];
    let mut size = std::mem::size_of_val(&buf) as u32;

    let copied = unsafe {
        GetRawInputData(
            lparam as HRAWINPUT,
            RID_INPUT,
            buf.as_mut_ptr() as *mut core::ffi::c_void,
            &mut size,
            std::mem::size_of::<RAWINPUTHEADER>() as u32,
        )
    };

    if copied == u32::MAX || (copied as usize) < std::mem::size_of::<RAWINPUT>() {
        return;
    }

    let raw = unsafe { &*(buf.as_ptr() as *const RAWINPUT) };
    if raw.header.dwType != RIM_TYPEMOUSE {
        return;
    }

    let mouse = unsafe { raw.data.mouse };
    if (mouse.usFlags & MOUSE_MOVE_ABSOLUTE) != 0 {
        return;
    }

    let dx = mouse.lLastX;
    let dy = mouse.lLastY;
    if dx == 0 && dy == 0 {
        return;
    }

    let device_id = raw.header.hDevice as usize;
    let now_instant = Instant::now();
    let now_epoch_ms = chrono::Utc::now().timestamp_millis();

    state.record_raw_packet(device_id, dx, dy, now_instant, now_epoch_ms);
}

#[cfg(windows)]
pub fn start_raw_input_listener(state: Arc<SharedAppState>) -> Result<RawInputGuard, String> {
    let (tx, rx) = mpsc::sync_channel::<Result<isize, String>>(1);

    let join_handle = thread::Builder::new()
        .name("mouseskip-raw-input".to_string())
        .spawn(move || {
            unsafe {
                let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL);
            }

            let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
            let class_name = to_wide_null("MouseSkipRawInputSinkClass");
            let window_name = to_wide_null("MouseSkipRawInputSink");

            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(raw_input_wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: std::ptr::null_mut(),
                hCursor: std::ptr::null_mut(),
                hbrBackground: std::ptr::null_mut(),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };

            let atom = unsafe { RegisterClassExW(&wc) };
            if atom == 0 {
                let err = unsafe { GetLastError() };
                if err != ERROR_CLASS_ALREADY_EXISTS {
                    let _ = tx.send(Err(format!("RegisterClassExW failed: Win32 error {err}")));
                    return;
                }
            }

            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    window_name.as_ptr(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    hinstance,
                    std::ptr::null(),
                )
            };

            if hwnd.is_null() {
                let err = unsafe { GetLastError() };
                let _ = tx.send(Err(format!("CreateWindowExW failed: Win32 error {err}")));
                return;
            }

            let state_raw = Arc::into_raw(Arc::clone(&state));
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, state_raw as isize);
            }

            if let Err(err) = register_mouse_sink(hwnd) {
                unsafe {
                    let ptr = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) as *const SharedAppState;
                    if !ptr.is_null() {
                        drop(Arc::from_raw(ptr));
                    }
                    DestroyWindow(hwnd);
                }
                let _ = tx.send(Err(format!(
                    "RegisterRawInputDevices failed: Win32 error {err}"
                )));
                return;
            }

            unsafe {
                SetTimer(hwnd, TIMER_REASSERT_SINK_ID, 1000, None);
            }

            state.raw_input_registered.store(true, Ordering::Release);
            state.live.set_raw_input_active(true);

            let _ = tx.send(Ok(hwnd as isize));

            let mut msg: MSG = unsafe { std::mem::zeroed() };
            loop {
                let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
                if ret <= 0 {
                    break;
                }
                unsafe {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        })
        .map_err(|e| format!("Failed to spawn WM_INPUT thread: {e}"))?;

    match rx.recv() {
        Ok(Ok(hwnd_raw)) => Ok(RawInputGuard {
            hwnd_raw,
            join_handle: Some(join_handle),
        }),
        Ok(Err(err)) => {
            let _ = join_handle.join();
            Err(err)
        }
        Err(err) => {
            let _ = join_handle.join();
            Err(format!("Raw Input thread exited before initialization: {err}"))
        }
    }
}

#[cfg(not(windows))]
pub fn start_raw_input_listener(_state: Arc<SharedAppState>) -> Result<RawInputGuard, String> {
    Err("Windows Raw Input (WM_INPUT) is only supported on Windows.".to_string())
}
