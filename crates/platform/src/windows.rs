use super::*;
use ::windows::{
    Win32::{
        Devices::{DeviceAndDriverInstallation::*, HumanInterfaceDevice::*},
        Foundation::*,
        Storage::FileSystem::*,
        System::{IO::*, LibraryLoader::GetModuleHandleW, Threading::*},
        UI::WindowsAndMessaging::*,
    },
    core::{PCWSTR, w},
};
use anyhow::{Context, bail, ensure};
use std::{collections::BTreeMap, mem::size_of, sync::OnceLock};

struct Handle(HANDLE);
// Owned kernel handles may be moved between threads. Event handles alone are shared below.
unsafe impl Send for Handle {}
impl Drop for Handle {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

struct EventHandle(Handle);
// SetEvent is explicitly thread-safe; only the transport owner resets this event.
unsafe impl Sync for EventHandle {}
impl EventHandle {
    fn new() -> Result<Self> {
        Ok(Self(Handle(unsafe {
            CreateEventW(None, true, false, None)?
        })))
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

pub fn discover() -> Result<Vec<ReceiverInfo>> {
    unsafe {
        let guid = HidD_GetHidGuid();
        let set = SetupDiGetClassDevsW(
            Some(&guid),
            PCWSTR::null(),
            HWND::default(),
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        )?;
        struct SetGuard(HDEVINFO);
        impl Drop for SetGuard {
            fn drop(&mut self) {
                unsafe {
                    let _ = SetupDiDestroyDeviceInfoList(self.0);
                }
            }
        }
        let _guard = SetGuard(set);
        let mut found = BTreeMap::<String, ReceiverInfo>::new();
        for index in 0..1024 {
            let mut data = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &guid, index, &mut data).is_err() {
                break;
            }
            let mut required = 0;
            let _ =
                SetupDiGetDeviceInterfaceDetailW(set, &data, None, 0, Some(&mut required), None);
            if !(8..=65536).contains(&required) {
                continue;
            }
            // u64 storage satisfies the Windows structure's alignment requirement.
            let mut buffer = vec![0u64; (required as usize).div_ceil(8)];
            let detail = buffer
                .as_mut_ptr()
                .cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            SetupDiGetDeviceInterfaceDetailW(set, &data, Some(detail), required, None, None)?;
            let path = PCWSTR((*detail).DevicePath.as_ptr())
                .to_string()?
                .to_lowercase();
            if !path.contains("vid_046d") {
                continue;
            }
            let path_w = wide(&path);
            let Ok(raw) = CreateFileW(
                PCWSTR(path_w.as_ptr()),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL,
                None,
            ) else {
                continue;
            };
            let h = Handle(raw);
            let mut attr = HIDD_ATTRIBUTES {
                Size: size_of::<HIDD_ATTRIBUTES>() as u32,
                ..Default::default()
            };
            if !HidD_GetAttributes(h.0, &mut attr).as_bool()
                || attr.VendorID != 0x046d
                || !RECEIVER_PIDS.contains(&attr.ProductID)
            {
                continue;
            }
            let mut pp = PHIDP_PREPARSED_DATA::default();
            if !HidD_GetPreparsedData(h.0, &mut pp).as_bool() {
                continue;
            }
            let mut caps = HIDP_CAPS::default();
            let result = HidP_GetCaps(pp, &mut caps);
            let _ = HidD_FreePreparsedData(pp);
            if result.is_err() || caps.UsagePage != 0xff00 || !matches!(caps.Usage, 1 | 2) {
                continue;
            }
            let parts: Vec<_> = path.split('#').collect();
            if parts.len() < 3 {
                continue;
            }
            let collection_base = parts[1].split("&col").next().unwrap_or(parts[1]);
            let instance_base = parts[2].rsplit_once('&').map(|v| v.0).unwrap_or(parts[2]);
            let id = format!("{collection_base}#{instance_base}");
            found
                .entry(id.clone())
                .or_insert_with(|| ReceiverInfo {
                    id,
                    product_id: attr.ProductID,
                    endpoints: Vec::new(),
                })
                .endpoints
                .push(Endpoint {
                    path,
                    input_len: caps.InputReportByteLength as usize,
                    output_len: caps.OutputReportByteLength as usize,
                });
        }
        Ok(found
            .into_values()
            .filter(|r| r.endpoints.iter().any(|e| e.output_len >= 20))
            .collect())
    }
}

struct ReadSlot {
    handle: Handle,
    event: EventHandle,
    overlapped: Box<OVERLAPPED>,
    buffer: Box<[u8]>,
    output_len: usize,
    pending: bool,
}
// The pinned buffers and OVERLAPPED are only accessed by their owning transport thread.
unsafe impl Send for ReadSlot {}
impl ReadSlot {
    fn begin(&mut self) -> Result<()> {
        if self.pending {
            return Ok(());
        }
        unsafe {
            ResetEvent(self.event.0.0)?;
            *self.overlapped = OVERLAPPED {
                hEvent: self.event.0.0,
                ..Default::default()
            };
            self.pending = true;
            if let Err(e) = ReadFile(
                self.handle.0,
                Some(&mut self.buffer),
                None,
                Some(&mut *self.overlapped),
            ) && e.code() != ERROR_IO_PENDING.to_hresult()
            {
                self.pending = false;
                return Err(e.into());
            }
        }
        Ok(())
    }
}
impl Drop for ReadSlot {
    fn drop(&mut self) {
        if self.pending {
            unsafe {
                let _ = CancelIoEx(self.handle.0, Some(&*self.overlapped));
                let mut n = 0;
                let _ = GetOverlappedResult(self.handle.0, &*self.overlapped, &mut n, true);
            }
        }
    }
}

struct WindowsTransport {
    slots: Vec<ReadSlot>,
    wake: Arc<EventHandle>,
}

pub fn open(info: &ReceiverInfo) -> Result<Box<dyn Transport>> {
    let mut slots = Vec::new();
    for endpoint in &info.endpoints {
        let path = wide(&endpoint.path);
        let handle = Handle(
            unsafe {
                CreateFileW(
                    PCWSTR(path.as_ptr()),
                    GENERIC_READ.0 | GENERIC_WRITE.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_OVERLAPPED,
                    None,
                )
            }
            .context("Cannot open Logitech vendor HID interface")?,
        );
        ensure!(
            (7..=1024).contains(&endpoint.input_len),
            "Invalid HID report size"
        );
        slots.push(ReadSlot {
            handle,
            event: EventHandle::new()?,
            overlapped: Box::default(),
            buffer: vec![0; endpoint.input_len].into_boxed_slice(),
            output_len: endpoint.output_len,
            pending: false,
        });
    }
    Ok(Box::new(WindowsTransport {
        slots,
        wake: Arc::new(EventHandle::new()?),
    }))
}

impl Transport for WindowsTransport {
    fn waker(&self) -> Wake {
        let event = self.wake.clone();
        Arc::new(move || unsafe {
            let _ = SetEvent(event.0.0);
        })
    }
    fn read(&mut self, timeout: Option<Duration>) -> Result<Option<Vec<u8>>> {
        for slot in &mut self.slots {
            slot.begin()?;
        }
        let mut handles = vec![self.wake.0.0];
        handles.extend(self.slots.iter().map(|s| s.event.0.0));
        let ms = timeout
            .map(|d| d.as_millis().min(u32::MAX as u128 - 1) as u32)
            .unwrap_or(INFINITE);
        let result = unsafe { WaitForMultipleObjects(&handles, false, ms) };
        if result == WAIT_TIMEOUT {
            return Ok(None);
        }
        if result == WAIT_OBJECT_0 {
            unsafe {
                ResetEvent(self.wake.0.0)?;
            }
            return Ok(None);
        }
        let index = result.0.wrapping_sub(WAIT_OBJECT_0.0 + 1) as usize;
        ensure!(index < self.slots.len(), "HID wait failed: {}", result.0);
        let slot = &mut self.slots[index];
        let mut n = 0;
        let result =
            unsafe { GetOverlappedResult(slot.handle.0, &*slot.overlapped, &mut n, false) };
        slot.pending = false;
        result?;
        ensure!(n > 0, "HID interface disconnected");
        Ok(Some(slot.buffer[..n as usize].to_vec()))
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let slot = self
            .slots
            .iter()
            .filter(|s| s.output_len >= bytes.len())
            .min_by_key(|s| s.output_len)
            .context("No matching HID output collection")?;
        let mut buffer = vec![0; slot.output_len];
        buffer[..bytes.len()].copy_from_slice(bytes);
        let event = EventHandle::new()?;
        let mut op = OVERLAPPED {
            hEvent: event.0.0,
            ..Default::default()
        };
        unsafe {
            if let Err(e) = WriteFile(slot.handle.0, Some(&buffer), None, Some(&mut op))
                && e.code() != ERROR_IO_PENDING.to_hresult()
            {
                return Err(e.into());
            }
            if WaitForSingleObject(event.0.0, 500) != WAIT_OBJECT_0 {
                let _ = CancelIoEx(slot.handle.0, Some(&op));
                let mut n = 0;
                let _ = GetOverlappedResult(slot.handle.0, &op, &mut n, true);
                bail!("HID write timed out");
            }
            let mut n = 0;
            GetOverlappedResult(slot.handle.0, &op, &mut n, false)?;
            ensure!(n as usize == buffer.len(), "Incomplete HID write");
        }
        Ok(())
    }
}

static CHANGES: OnceLock<crossbeam_channel::Sender<()>> = OnceLock::new();
unsafe extern "system" fn watcher_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if (msg == WM_DEVICECHANGE || msg == WM_POWERBROADCAST)
        && let Some(tx) = CHANGES.get()
    {
        let _ = tx.try_send(());
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}

pub fn watch_changes(tx: crossbeam_channel::Sender<()>) -> Result<()> {
    let _ = CHANGES.set(tx);
    std::thread::Builder::new()
        .name("device-notifications".into())
        .spawn(|| unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let class = WNDCLASSW {
                lpfnWndProc: Some(watcher_proc),
                hInstance: instance.into(),
                lpszClassName: w!("EasySwitchLinkDeviceWatcher"),
                ..Default::default()
            };
            if RegisterClassW(&class) == 0 {
                return;
            }
            let Ok(hwnd) = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class.lpszClassName,
                w!("Easy-Switch Link"),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                instance,
                None,
            ) else {
                return;
            };
            let guid = HidD_GetHidGuid();
            let filter = DEV_BROADCAST_DEVICEINTERFACE_W {
                dbcc_size: size_of::<DEV_BROADCAST_DEVICEINTERFACE_W>() as u32,
                dbcc_devicetype: DBT_DEVTYP_DEVICEINTERFACE.0,
                dbcc_classguid: guid,
                ..Default::default()
            };
            let _notification = RegisterDeviceNotificationW(
                hwnd,
                &filter as *const _ as *const _,
                DEVICE_NOTIFY_WINDOW_HANDLE,
            );
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        })?;
    Ok(())
}

pub fn user_id() -> Result<String> {
    use ::windows::Win32::Security::Authorization::*;
    use ::windows::Win32::Security::*;
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;
        let token = Handle(token);
        let mut required = 0;
        let _ = GetTokenInformation(token.0, TokenUser, None, 0, &mut required);
        let mut buffer = vec![0u64; (required as usize).div_ceil(8)];
        GetTokenInformation(
            token.0,
            TokenUser,
            Some(buffer.as_mut_ptr().cast()),
            required,
            &mut required,
        )?;
        let user = &*buffer.as_ptr().cast::<TOKEN_USER>();
        let mut text = ::windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut text)?;
        let result = text.to_string();
        let _ = LocalFree(HLOCAL(text.0.cast()));
        Ok(result?)
    }
}

pub fn set_autostart(enabled: bool, executable: &std::path::Path) -> Result<()> {
    use ::windows::Win32::System::Registry::*;
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run"),
            0,
            None,
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
        .ok()?;
        let result = if enabled {
            let value = wide(&format!("\"{}\" run", executable.display()));
            let bytes = std::slice::from_raw_parts(value.as_ptr().cast::<u8>(), value.len() * 2);
            RegSetValueExW(key, w!("EasySwitchLink"), 0, REG_SZ, Some(bytes)).ok()
        } else {
            let result = RegDeleteValueW(key, w!("EasySwitchLink"));
            if result == ERROR_FILE_NOT_FOUND {
                Ok(())
            } else {
                result.ok()
            }
        };
        let _ = RegCloseKey(key);
        result?;
    }
    Ok(())
}
