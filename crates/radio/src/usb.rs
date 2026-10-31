//! WinUSB 위의 어댑터: 레지스터는 제조사 제어 전송으로, 프레임은 벌크 파이프로. 어댑터는
//! Microsoft가 서명한 WinUSB에 묶여 있어야 하므로 (WirDrop 설치가 묶습니다) 여기에는 테스트 서명이
//! 필요 없습니다.
use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::sync::Mutex;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_GET_DEVICE_INTERFACE_LIST_PRESENT, CM_Get_Device_Interface_List_SizeW, CM_Get_Device_Interface_ListW, CR_SUCCESS,
};
use windows::Win32::Devices::Usb::{
    PIPE_TRANSFER_TIMEOUT, RAW_IO, SHORT_PACKET_TERMINATE, USB_INTERFACE_DESCRIPTOR, UsbdPipeTypeBulk, WINUSB_INTERFACE_HANDLE,
    WINUSB_PIPE_INFORMATION, WINUSB_PIPE_POLICY, WINUSB_SETUP_PACKET, WinUsb_AbortPipe, WinUsb_ControlTransfer, WinUsb_Free,
    WinUsb_GetOverlappedResult, WinUsb_Initialize, WinUsb_QueryInterfaceSettings, WinUsb_QueryPipe, WinUsb_ReadPipe, WinUsb_SetPipePolicy,
    WinUsb_WritePipe,
};
use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE};
use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
use windows::Win32::System::IO::OVERLAPPED;
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_READ, RRF_RT_REG_MULTI_SZ, RRF_RT_REG_SZ, RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, WaitForSingleObject};
use windows::core::{GUID, PCWSTR};

/// 레지스터 접근용 Realtek 제조사 요청.
const REQ: u8 = 0x05;
const READ: u8 = 0xc0;
const WRITE: u8 = 0x40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsbId {
    pub vid: u16,
    pub pid: u16,
}

impl UsbId {
    /// TP-Link Archer T3U Nano (RTL8812BU).
    pub const T3U_NANO: UsbId = UsbId { vid: 0x2357, pid: 0x012e };
}

/// 어댑터의 WinUSB 인터페이스.
pub struct Usb {
    file: HANDLE,
    winusb: WINUSB_INTERFACE_HANDLE,
    pub bulk_in: u8,
    /// 서술자 순서대로의 벌크 OUT 엔드포인트 (rtw88의 out_ep[]).
    pub bulk_out: Vec<u8>,
    pub high_speed: bool,
    /// 제어 전송은 아래의 8822B 특이점 때문에 레지스터 접근 하나를 두 단계로 나릅니다. 한 번에
    /// 하나씩.
    ctrl: Mutex<()>,
}

// SAFETY: WinUSB 핸들은 어느 스레드에서든 쓸 수 있습니다. 서로 다른 파이프의 호출은 겹쳐도 되고,
// 제어 전송은 `ctrl`이 차례로 세웁니다.
unsafe impl Send for Usb {}
unsafe impl Sync for Usb {}

fn wide(s: &str) -> Vec<u16> {
    std::ffi::OsStr::new(s).encode_wide().chain(Some(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

/// 이 VID/PID로 WinUSB를 설치할 때 쓴 장치 인터페이스 GUID들 (Device Parameters).
fn interface_guids(id: UsbId) -> Vec<GUID> {
    let base = format!(r"SYSTEM\CurrentControlSet\Enum\USB\VID_{:04X}&PID_{:04X}", id.vid, id.pid);
    let mut guids = Vec::new();
    // SAFETY: 지역 버퍼로 하는 평범한 레지스트리 읽기.
    unsafe {
        let mut key = HKEY::default();
        if RegOpenKeyExW(HKEY_LOCAL_MACHINE, PCWSTR(wide(&base).as_ptr()), None, KEY_READ, &mut key).is_err() {
            return guids;
        }
        for i in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            if RegEnumKeyExW(key, i, Some(windows::core::PWSTR(name.as_mut_ptr())), &mut len, None, None, None, None).is_err() {
                break;
            }
            let sub = wide(&format!(r"{}\Device Parameters", from_wide(&name)));
            for (value, flags) in [("DeviceInterfaceGUIDs", RRF_RT_REG_MULTI_SZ), ("DeviceInterfaceGUID", RRF_RT_REG_SZ)] {
                let mut data = [0u16; 512];
                let mut size = (data.len() * 2) as u32;
                if RegGetValueW(
                    key,
                    PCWSTR(sub.as_ptr()),
                    PCWSTR(wide(value).as_ptr()),
                    flags,
                    None,
                    Some(data.as_mut_ptr() as *mut c_void),
                    Some(&mut size),
                )
                .is_ok()
                {
                    for s in String::from_utf16_lossy(&data[..(size as usize / 2)]).split('\0') {
                        if let Ok(g) = GUID::try_from(s.trim().trim_matches(|c| c == '{' || c == '}'))
                            && !guids.contains(&g)
                        {
                            guids.push(g);
                        }
                    }
                }
            }
        }
        let _ = RegCloseKey(key);
    }
    guids
}

/// 이 GUID를 가진, 지금 있는 장치 인터페이스들의 경로.
fn interface_paths(guid: &GUID) -> Vec<String> {
    // SAFETY: 크기 조회로 목록 호출이 채울 버퍼의 크기를 정합니다.
    unsafe {
        let mut len = 0u32;
        if CM_Get_Device_Interface_List_SizeW(&mut len, guid, PCWSTR::null(), CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS || len <= 1 {
            return Vec::new();
        }
        let mut buf = vec![0u16; len as usize];
        if CM_Get_Device_Interface_ListW(guid, PCWSTR::null(), &mut buf, CM_GET_DEVICE_INTERFACE_LIST_PRESENT) != CR_SUCCESS {
            return Vec::new();
        }
        String::from_utf16_lossy(&buf)
            .split('\0')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }
}

impl Usb {
    /// 이 ID의 어댑터가 꽂혀 있고 WinUSB에 묶여 있는지.
    pub fn present(id: UsbId) -> bool {
        Self::find(id).is_some()
    }

    fn find(id: UsbId) -> Option<String> {
        let tag = format!("vid_{:04x}&pid_{:04x}", id.vid, id.pid);
        interface_guids(id)
            .iter()
            .flat_map(interface_paths)
            .find(|p| p.to_ascii_lowercase().contains(&tag))
    }

    pub fn open(id: UsbId) -> io::Result<Usb> {
        let path = Self::find(id).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no {:04x}:{:04x} adapter bound to WinUSB", id.vid, id.pid),
            )
        })?;
        // SAFETY: 핸들은 돌려주는 Usb가 갖고 Drop에서 닫습니다.
        unsafe {
            let file = CreateFileW(
                PCWSTR(wide(&path).as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
            .map_err(|e| io::Error::other(format!("open {path}: {e} (is another program using the adapter?)")))?;
            let mut winusb = WINUSB_INTERFACE_HANDLE::default();
            if let Err(e) = WinUsb_Initialize(file, &mut winusb) {
                let _ = CloseHandle(file);
                return Err(io::Error::other(format!("WinUsb_Initialize: {e}")));
            }
            let mut usb = Usb {
                file,
                winusb,
                bulk_in: 0,
                bulk_out: Vec::new(),
                high_speed: true,
                ctrl: Mutex::new(()),
            };
            let mut desc = USB_INTERFACE_DESCRIPTOR::default();
            WinUsb_QueryInterfaceSettings(winusb, 0, &mut desc).map_err(io::Error::other)?;
            for i in 0..desc.bNumEndpoints {
                let mut pipe = WINUSB_PIPE_INFORMATION::default();
                WinUsb_QueryPipe(winusb, 0, i, &mut pipe).map_err(io::Error::other)?;
                if pipe.PipeType != UsbdPipeTypeBulk {
                    continue;
                }
                if pipe.PipeId & 0x80 != 0 {
                    usb.bulk_in = pipe.PipeId;
                    usb.high_speed = pipe.MaximumPacketSize >= 512;
                } else {
                    usb.bulk_out.push(pipe.PipeId);
                }
            }
            if usb.bulk_in == 0 || usb.bulk_out.is_empty() {
                return Err(io::Error::other("the adapter has no bulk endpoints"));
            }
            usb.policy(usb.bulk_in, RAW_IO, 1u8)?;
            for &ep in &usb.bulk_out.clone() {
                // rtw88은 URB_ZERO_PACKET을 보냅니다: 패킷 크기의 배수인 전송은 길이 0인 패킷으로
                // 끝나야 하고, 아니면 어댑터가 더 기다립니다.
                usb.policy(ep, SHORT_PACKET_TERMINATE, 1u8)?;
                usb.policy(ep, PIPE_TRANSFER_TIMEOUT, 1000u32)?;
            }
            Ok(usb)
        }
    }

    fn policy<T: Copy>(&self, pipe: u8, policy: WINUSB_PIPE_POLICY, value: T) -> io::Result<()> {
        // SAFETY: value는 호출 동안 살아 있고 그 크기를 넘깁니다.
        unsafe {
            WinUsb_SetPipePolicy(self.winusb, pipe, policy, size_of::<T>() as u32, &value as *const T as *const c_void).map_err(io::Error::other)
        }
    }

    fn control(&self, request_type: u8, addr: u16, buf: &mut [u8]) -> io::Result<()> {
        let setup = WINUSB_SETUP_PACKET {
            RequestType: request_type,
            Request: REQ,
            Value: addr,
            Index: 0,
            Length: buf.len() as u16,
        };
        let mut done = 0u32;
        for attempt in 0..3 {
            // SAFETY: buf는 동기 전송 동안 유효합니다.
            match unsafe { WinUsb_ControlTransfer(self.winusb, setup, Some(&mut *buf), Some(&mut done), None) } {
                Ok(()) if done as usize == buf.len() => return Ok(()),
                Ok(()) => return Err(io::Error::other(format!("register 0x{addr:04x}: short transfer {done}"))),
                Err(e) if attempt == 2 => return Err(io::Error::other(format!("register 0x{addr:04x}: {e}"))),
                Err(_) => {}
            }
        }
        unreachable!()
    }

    /// rtw88의 8822B/8821C/8822C 특이점: 상시 전원 구역(0x00-0xff, 0x1000-0x10ff)을 건드린 뒤 첫
    /// 데이터 바이트를 0x4e0에 씁니다.
    fn reg_sec(&self, addr: u16, data: &mut [u8]) {
        if addr <= 0xff || (0x1000..=0x10ff).contains(&addr) {
            let _ = self.control(WRITE, 0x4e0, &mut data[..1]);
        }
    }

    pub fn read(&self, addr: u16, len: usize) -> io::Result<u32> {
        let _g = self.ctrl.lock().unwrap();
        let mut b = [0u8; 4];
        self.control(READ, addr, &mut b[..len])?;
        self.reg_sec(addr, &mut b);
        Ok(u32::from_le_bytes(b))
    }

    pub fn write(&self, addr: u16, val: u32, len: usize) -> io::Result<()> {
        let _g = self.ctrl.lock().unwrap();
        let mut b = val.to_le_bytes();
        self.control(WRITE, addr, &mut b[..len])?;
        self.reg_sec(addr, &mut b);
        Ok(())
    }

    /// 벌크 OUT 전송 한 번 (막힘).
    pub fn bulk_write(&self, ep: u8, data: &[u8]) -> io::Result<()> {
        let mut done = 0u32;
        // SAFETY: data는 동기 호출보다 오래 삽니다.
        unsafe { WinUsb_WritePipe(self.winusb, ep, data, Some(&mut done), None) }
            .map_err(|e| io::Error::other(format!("bulk OUT 0x{ep:02x}: {e}")))?;
        if done as usize != data.len() {
            return Err(io::Error::other(format!("bulk OUT 0x{ep:02x}: {done} of {} bytes", data.len())));
        }
        Ok(())
    }

    /// 기다리는 읽기를 멈춥니다 (읽는 스레드가 끝날 수 있게).
    pub fn abort_in(&self) {
        // SAFETY: 우리 핸들에 대한 평범한 호출.
        let _ = unsafe { WinUsb_AbortPipe(self.winusb, self.bulk_in) };
    }

    /// 벌크 IN 전송 여럿을 동시에 걸어 두는 읽기 도구.
    pub fn reader(&self, count: usize, size: usize) -> io::Result<Reader<'_>> {
        let mut slots = Vec::with_capacity(count);
        for _ in 0..count {
            // SAFETY: 전송마다 수동 리셋 이벤트 하나. Reader의 Drop에서 닫습니다.
            let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }.map_err(io::Error::other)?;
            slots.push(Slot {
                ov: Box::new(OVERLAPPED {
                    hEvent: event,
                    ..Default::default()
                }),
                buf: vec![0u8; size],
            });
        }
        let mut r = Reader { usb: self, slots, next: 0 };
        for i in 0..count {
            r.submit(i)?;
        }
        Ok(r)
    }
}

impl Drop for Usb {
    fn drop(&mut self) {
        // SAFETY: 두 핸들 모두 우리 것입니다.
        unsafe {
            let _ = WinUsb_Free(self.winusb);
            let _ = CloseHandle(self.file);
        }
    }
}

struct Slot {
    ov: Box<OVERLAPPED>,
    buf: Vec<u8>,
}

/// 제출한 순서대로 끝나는 벌크 IN 읽기들.
pub struct Reader<'a> {
    usb: &'a Usb,
    slots: Vec<Slot>,
    next: usize,
}

impl Reader<'_> {
    fn submit(&mut self, i: usize) -> io::Result<()> {
        let s = &mut self.slots[i];
        let event = s.ov.hEvent;
        *s.ov = OVERLAPPED {
            hEvent: event,
            ..Default::default()
        };
        // SAFETY: 버퍼와 OVERLAPPED는 읽기가 끝날 때까지 (박스에 담겨 / 소유되어) 제자리에
        // 있습니다.
        match unsafe { WinUsb_ReadPipe(self.usb.winusb, self.usb.bulk_in, Some(&mut s.buf[..]), None, Some(&*s.ov)) } {
            Ok(()) => Ok(()),
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => Ok(()),
            Err(e) => Err(io::Error::other(format!("bulk IN: {e}"))),
        }
    }

    /// 다음 전송을 기다려 그 바이트를 `f`에 넘기고 다시 제출합니다.
    pub fn next<R>(&mut self, f: impl FnOnce(&[u8]) -> R) -> io::Result<R> {
        let i = self.next;
        let mut done = 0u32;
        {
            let s = &self.slots[i];
            // SAFETY: 우리가 걸어 둔 전송을 기다립니다.
            unsafe {
                WaitForSingleObject(s.ov.hEvent, INFINITE);
                WinUsb_GetOverlappedResult(self.usb.winusb, &*s.ov, &mut done, true).map_err(|e| io::Error::other(format!("bulk IN: {e}")))?;
            }
        }
        let r = f(&self.slots[i].buf[..done as usize]);
        self.submit(i)?;
        self.next = (i + 1) % self.slots.len();
        Ok(r)
    }
}

impl Drop for Reader<'_> {
    fn drop(&mut self) {
        self.usb.abort_in();
        for s in &self.slots {
            let mut done = 0u32;
            // SAFETY: 버퍼가 사라지기 전에 중단된 전송이 끝나길 기다립니다.
            unsafe {
                let _ = WinUsb_GetOverlappedResult(self.usb.winusb, &*s.ov, &mut done, true);
                let _ = CloseHandle(s.ov.hEvent);
            }
        }
    }
}
