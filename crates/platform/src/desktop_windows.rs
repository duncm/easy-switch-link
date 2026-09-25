use super::*;
use std::{cell::RefCell, mem::size_of};
use windows::{
    Win32::{
        Foundation::*,
        Graphics::Gdi::*,
        System::{LibraryLoader::GetModuleHandleW, RemoteDesktop::*, StationsAndDesktops::*},
        UI::{HiDpi::*, Input::KeyboardAndMouse::*, Input::*, WindowsAndMessaging::*},
    },
    core::w,
};
const CONTROL: u32 = WM_APP + 41;
thread_local! {static LOOP:RefCell<Option<Loop>>=const{RefCell::new(None)};}
struct Loop {
    model: Model,
    commands: mpsc::Receiver<Command>,
}
unsafe extern "system" fn enum_monitor(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let out = unsafe { &mut *(data.0 as *mut Vec<easy_switch_core::follow::Display>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(h, &mut info as *mut _ as *mut MONITORINFO) }.as_bool() {
        let name = String::from_utf16_lossy(
            &info.szDevice[..info.szDevice.iter().position(|v| *v == 0).unwrap_or(32)],
        );
        let mut device = DISPLAY_DEVICEW {
            cb: size_of::<DISPLAY_DEVICEW>() as u32,
            ..Default::default()
        };
        let name16: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
        let found = unsafe {
            EnumDisplayDevicesW(
                windows::core::PCWSTR(name16.as_ptr()),
                0,
                &mut device,
                EDD_GET_DEVICE_INTERFACE_NAME,
            )
        }
        .as_bool();
        let id = if found {
            String::from_utf16_lossy(
                &device.DeviceID[..device.DeviceID.iter().position(|v| *v == 0).unwrap_or(128)],
            )
        } else {
            name.clone()
        };
        let mut dx = 96;
        let mut dy = 96;
        let _ = unsafe { GetDpiForMonitor(h, MDT_EFFECTIVE_DPI, &mut dx, &mut dy) };
        let r = info.monitorInfo.rcMonitor;
        let scale = dx as f64 / 96.0;
        out.push(easy_switch_core::follow::Display {
            id,
            name,
            x: r.left as f64,
            y: r.top as f64,
            width: (r.right - r.left) as f64,
            height: (r.bottom - r.top) as f64,
            scale,
            binding: format!("{},{},{},{},{}", r.left, r.top, r.right, r.bottom, dx),
        });
    }
    BOOL(1)
}
fn displays() -> Vec<easy_switch_core::follow::Display> {
    let mut result = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(
            None,
            None,
            Some(enum_monitor),
            LPARAM(&mut result as *mut _ as isize),
        );
    }
    result
}
fn point() -> Point {
    let mut p = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut p);
    }
    Point {
        x: p.x as f64,
        y: p.y as f64,
    }
}
fn active_desktop() -> bool {
    unsafe {
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
        else {
            return false;
        };
        let mut name = [0u16; 128];
        let ok = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            256,
            None,
        )
        .is_ok();
        let _ = CloseDesktop(desktop);
        ok && String::from_utf16_lossy(&name[..name.iter().position(|v| *v == 0).unwrap_or(128)])
            .eq_ignore_ascii_case("default")
    }
}
fn warp(p: Point) -> Result<()> {
    ensure!(
        active_desktop(),
        "Current input desktop is not the active user desktop"
    );
    unsafe {
        SetCursorPos(p.x.round() as i32, p.y.round() as i32)?;
    }
    let actual = point();
    ensure!(
        (actual.x - p.x).abs() <= 1.0 && (actual.y - p.y).abs() <= 1.0,
        "Another application constrained cursor placement"
    );
    Ok(())
}
unsafe extern "system" fn proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    LOOP.with(|slot| {
        if let Some(state) = slot.borrow_mut().as_mut() {
            match msg {
                WM_INPUT => {
                    let mut input = RAWINPUT::default();
                    let mut size = size_of::<RAWINPUT>() as u32;
                    let count = unsafe {
                        GetRawInputData(
                            HRAWINPUT(l.0 as _),
                            RID_INPUT,
                            Some(&mut input as *mut _ as *mut _),
                            &mut size,
                            size_of::<RAWINPUTHEADER>() as u32,
                        )
                    };
                    if count != u32::MAX && input.header.dwType == RIM_TYPEKEYBOARD.0 {
                        state.model.state.input_serial =
                            state.model.state.input_serial.wrapping_add(1);
                    }
                    if count != u32::MAX
                        && input.header.dwType == RIM_TYPEMOUSE.0
                        && !input.header.hDevice.is_invalid()
                    {
                        let m = unsafe { input.data.mouse };
                        let buttons =
                            [VK_LBUTTON, VK_RBUTTON, VK_MBUTTON, VK_XBUTTON1, VK_XBUTTON2]
                                .iter()
                                .any(|v| unsafe { GetAsyncKeyState(v.0 as i32) } < 0);
                        if state.model.state.locked && active_desktop() {
                            state.model.state.locked = false;
                            state.model.publish();
                        }
                        let p = point();
                        let delta = if m.usFlags.0 & 1 != 0 {
                            Point {
                                x: p.x - state.model.state.pointer.x,
                                y: p.y - state.model.state.pointer.y,
                            }
                        } else {
                            Point {
                                x: m.lLastX as f64,
                                y: m.lLastY as f64,
                            }
                        };
                        state.model.movement(p, delta, buttons, false);
                    }
                }
                WM_DISPLAYCHANGE | WM_DPICHANGED => state.model.topology(displays()),
                WM_WTSSESSION_CHANGE => {
                    if matches!(
                        w.0 as u32,
                        WTS_SESSION_LOCK
                            | WTS_CONSOLE_DISCONNECT
                            | WTS_REMOTE_DISCONNECT
                            | WTS_SESSION_LOGOFF
                    ) {
                        state.model.state.locked = true;
                        state.model.reservation = None;
                    } else if matches!(
                        w.0 as u32,
                        WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT
                    ) {
                        state.model.state.locked = !active_desktop();
                    }
                    state.model.publish();
                }
                WM_POWERBROADCAST => {
                    if w.0 == PBT_APMSUSPEND as usize {
                        state.model.state.locked = true;
                        state.model.publish();
                    } else if w.0 == PBT_APMRESUMEAUTOMATIC as usize {
                        state.model.state.locked = !active_desktop();
                        state.model.topology(displays());
                        state.model.publish();
                    }
                }
                CONTROL => {
                    while let Ok(c) = state.commands.try_recv() {
                        match c {
                            Command::Configure { enabled, rules } => {
                                state.model.enabled = enabled;
                                state.model.rules = rules;
                                if !enabled {
                                    state.model.reservation = None;
                                }
                            }
                            Command::Authorize => {}
                            Command::Place {
                                deadline,
                                segment,
                                fraction,
                                generation,
                                reply,
                            } => {
                                if reply.is_closed() || Instant::now() >= deadline {
                                    continue;
                                }
                                state.model.state.pointer = point();
                                state.model.state.locked = !active_desktop();
                                let result = state
                                    .model
                                    .placement(&segment, fraction, generation)
                                    .and_then(|(id, p)| {
                                        ensure!(
                                            Instant::now() < deadline,
                                            "Cursor placement expired"
                                        );
                                        warp(p)?;
                                        Ok(id)
                                    })
                                    .map_err(|e| e.to_string());
                                if result.is_err() {
                                    state.model.reservation = None;
                                }
                                if let Err(Ok(token)) = reply.send(result)
                                    && let Some(previous) = state.model.cancel(token)
                                {
                                    let _ = warp(previous);
                                }
                            }
                            Command::Cancel(id) => {
                                if let Some(p) = state.model.cancel(id) {
                                    let _ = warp(p);
                                }
                            }
                            Command::Complete(id) => state.model.complete(id),
                            Command::Stop => unsafe { PostQuitMessage(0) },
                        }
                    }
                }
                _ => {}
            }
        }
    });
    unsafe { DefWindowProcW(hwnd, msg, w, l) }
}
pub fn start(
    commands: mpsc::Receiver<Command>,
    events: crossbeam_channel::Sender<Event>,
    private_dir: std::path::PathBuf,
) -> Result<Arc<dyn Fn() + Send + Sync>> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("desktop-edges".into())
        .spawn(move || {
            let result = (|| -> Result<()> {
                unsafe {
                    let _ =
                        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
                    let module = GetModuleHandleW(None)?;
                    let class = WNDCLASSW {
                        lpfnWndProc: Some(proc),
                        hInstance: module.into(),
                        lpszClassName: w!("EasySwitchLinkEdges"),
                        ..Default::default()
                    };
                    RegisterClassW(&class);
                    let window = CreateWindowExW(
                        WINDOW_EX_STYLE::default(),
                        class.lpszClassName,
                        w!("Easy-Switch Link edges"),
                        WINDOW_STYLE::default(),
                        0,
                        0,
                        0,
                        0,
                        None,
                        None,
                        module,
                        None,
                    )?;
                    let raw = RAWINPUTDEVICE {
                        usUsagePage: 1,
                        usUsage: 2,
                        dwFlags: RIDEV_INPUTSINK,
                        hwndTarget: window,
                    };
                    RegisterRawInputDevices(
                        &[raw, RAWINPUTDEVICE { usUsage: 6, ..raw }],
                        size_of::<RAWINPUTDEVICE>() as u32,
                    )?;
                    WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION)?;
                    let mut model = Model::new("Windows Raw Input", events.clone());
                    model.load_bindings(&private_dir)?;
                    model.state.ready = true;
                    model.state.locked = !active_desktop();
                    model.state.pointer = point();
                    model.topology(displays());
                    LOOP.with(|s| *s.borrow_mut() = Some(Loop { model, commands }));
                    let _ = tx.send(Ok(window.0 as usize));
                    let mut message = MSG::default();
                    while GetMessageW(&mut message, None, 0, 0).as_bool() {
                        let _ = TranslateMessage(&message);
                        DispatchMessageW(&message);
                    }
                    let _ = WTSUnRegisterSessionNotification(window);
                    let _ = DestroyWindow(window);
                }
                Ok(())
            })();
            if let Err(e) = result {
                let _ = tx.send(Err(e.to_string()));
                let _ = events.send(Event::State(DesktopState {
                    reason: e.to_string(),
                    ..Default::default()
                }));
            }
        })?;
    let hwnd = rx.recv()?.map_err(anyhow::Error::msg)?;
    Ok(Arc::new(move || unsafe {
        let _ = PostMessageW(HWND(hwnd as _), CONTROL, WPARAM(0), LPARAM(0));
    }))
}
