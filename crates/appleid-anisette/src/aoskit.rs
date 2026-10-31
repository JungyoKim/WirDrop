//! AOSKit을 불러와 묻습니다 (크레이트 문서 참고).
use std::ffi::{CStr, c_char, c_void};
use std::io::{BufRead, Write};

use windows_sys::Win32::Foundation::{HMODULE, WIN32_ERROR};
use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use windows_sys::Win32::System::Memory::{PAGE_PROTECTION_FLAGS, PAGE_READWRITE, VirtualProtect};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_WOW64_32KEY, KEY_WOW64_64KEY, REG_SAM_FLAGS, RegOpenKeyExA, RegOpenKeyExW,
};

/// Objective-C 개체, 셀렉터, Core Foundation 개체.
type Id = *mut c_void;
type Sel = *mut c_void;
type CfRef = *const c_void;

/// kCFStringEncodingUTF8.
const UTF8: u32 = 0x0800_0100;
/// AOSKit의 HKLM\SOFTWARE\Apple… 조회를 돌리는 HKCU 아래 위치.
const REDIRECT: &str = r"Software\WirDrop\HKLM";

/// 이것이 쓰는 Apple 런타임 호출 몇 가지. 모두 C 호출 (32비트 Windows의 cdecl).
struct Aos {
    send: unsafe extern "C" fn(Id, Sel, Id) -> Id,
    string: unsafe extern "C" fn(*const c_void, *const c_char, u32) -> CfRef,
    lookup: unsafe extern "C" fn(CfRef, CfRef) -> CfRef,
    copy_out: unsafe extern "C" fn(CfRef, *mut c_char, i32, u32) -> u8,
    release: unsafe extern "C" fn(CfRef),
    utilities: Id,
    headers_for_dsid: Sel,
}

/// 표준 입력이 끝날 때까지 요청에 답합니다. 종료 코드: 0.
pub fn run() -> i32 {
    // SAFETY: Apple의 DLL을 불러오고 문서화된 C 시그니처로 내보낸 함수를 찾습니다.
    let aos = unsafe { Aos::load() };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let answer = match line.trim() {
            "" => continue,
            // SAFETY: 위에서 불러온 Apple 런타임을, 불러온 그 스레드에서.
            "otp" => match aos.as_ref().map_err(String::clone).and_then(|aos| unsafe { aos.otp() }) {
                Ok((md, mdm)) => format!("{{\"md\":\"{}\",\"mdm\":\"{}\"}}", escape(&md), escape(&mdm)),
                Err(e) => error(&e),
            },
            other => error(&format!("unknown request {other:?}")),
        };
        if writeln!(out, "{answer}").and_then(|()| out.flush()).is_err() {
            break;
        }
    }
    0
}

fn error(message: &str) -> String {
    format!("{{\"error\":\"{}\"}}", escape(message))
}

/// JSON 문자열의 내용.
fn escape(s: &str) -> String {
    s.chars()
        .flat_map(|c| match c {
            '"' | '\\' => vec!['\\', c],
            c if (c as u32) < 0x20 => format!("\\u{:04x}", c as u32).chars().collect(),
            c => vec![c],
        })
        .collect()
}

impl Aos {
    unsafe fn load() -> Result<Aos, String> {
        let objc = load(c"objc.dll")?;
        let cf = load(c"CoreFoundation.dll")?;
        // AOSKit에는 Foundation이 필요합니다: 먼저 불러오면 다른 것들 옆에서 찾습니다.
        let _ = load(c"Foundation.dll");
        let aoskit = load(c"AOSKit.dll")?;
        // SAFETY: AOSKit을 불러왔고, 대체 함수는 RegOpenKeyEx의 시그니처입니다.
        unsafe {
            redirect_import(aoskit, b"RegOpenKeyExA", open_key_a as *const () as usize);
            redirect_import(aoskit, b"RegOpenKeyExW", open_key_w as *const () as usize);
        }
        // SAFETY: 각 심볼은 Apple 런타임에 있는 그 시그니처의 C 함수입니다.
        unsafe {
            let class: unsafe extern "C" fn(*const c_char) -> Id = symbol(objc, c"objc_getClass")?;
            let selector: unsafe extern "C" fn(*const c_char) -> Sel = symbol(objc, c"sel_registerName")?;
            let utilities = class(c"AOSUtilities".as_ptr());
            if utilities.is_null() {
                return Err("AOSKit has no AOSUtilities".into());
            }
            Ok(Aos {
                send: symbol(objc, c"objc_msgSend")?,
                string: symbol(cf, c"CFStringCreateWithCString")?,
                lookup: symbol(cf, c"CFDictionaryGetValue")?,
                copy_out: symbol(cf, c"CFStringGetCString")?,
                release: symbol(cf, c"CFRelease")?,
                utilities,
                headers_for_dsid: selector(c"retrieveOTPHeadersForDSID:".as_ptr()),
            })
        }
    }

    /// +[AOSUtilities retrieveOTPHeadersForDSID:]가 DSID "-2"(계정이 아니라 기기)에 대해 주는
    /// 기기의 OTP 헤더.
    unsafe fn otp(&self) -> Result<(String, String), String> {
        // SAFETY: 살아 있는 인자로 하는 Apple 런타임 호출. 사전은 우리가 해제할 것이 아닙니다.
        unsafe {
            let dsid = (self.string)(std::ptr::null(), c"-2".as_ptr(), UTF8);
            let headers = (self.send)(self.utilities, self.headers_for_dsid, dsid as Id);
            (self.release)(dsid);
            if headers.is_null() {
                return Err("AOSKit gave no OTP headers".into());
            }
            Ok((self.text(headers, c"X-Apple-MD")?, self.text(headers, c"X-Apple-MD-M")?))
        }
    }

    /// CFDictionary에서 key 아래의 문자열.
    unsafe fn text(&self, dictionary: Id, key: &CStr) -> Result<String, String> {
        // SAFETY: otp에서와 같음. 버퍼는 말한 길이입니다.
        unsafe {
            let k = (self.string)(std::ptr::null(), key.as_ptr(), UTF8);
            let value = (self.lookup)(dictionary as CfRef, k);
            (self.release)(k);
            if value.is_null() {
                return Err(format!("no {} in AOSKit's answer", key.to_string_lossy()));
            }
            let mut buffer = vec![0 as c_char; 2048];
            if (self.copy_out)(value, buffer.as_mut_ptr(), buffer.len() as i32, UTF8) == 0 {
                return Err(format!("{} didn't fit", key.to_string_lossy()));
            }
            Ok(CStr::from_ptr(buffer.as_ptr()).to_string_lossy().into_owned())
        }
    }
}

fn load(name: &CStr) -> Result<HMODULE, String> {
    // SAFETY: PATH에서 찾는 이름의 라이브러리.
    let module = unsafe { LoadLibraryA(name.as_ptr().cast()) };
    if module.is_null() {
        return Err(format!("{} didn't load ({})", name.to_string_lossy(), std::io::Error::last_os_error()));
    }
    Ok(module)
}

/// module이 내보낸 name을 형식 F의 함수 포인터로.
///
/// # Safety
/// F는 내보낸 것과 맞는 함수 포인터 형식이어야 합니다.
unsafe fn symbol<F: Copy>(module: HMODULE, name: &CStr) -> Result<F, String> {
    // SAFETY: 평범한 조회.
    let found = unsafe { GetProcAddress(module, name.as_ptr().cast()) };
    let Some(found) = found else {
        return Err(format!("no {}", name.to_string_lossy()));
    };
    debug_assert_eq!(size_of::<F>(), size_of_val(&found));
    // SAFETY: F가 그 형식이라는 부르는 쪽의 보증. 둘 다 코드 포인터입니다.
    Ok(unsafe { std::mem::transmute_copy(&found) })
}

/// module이 가져오는 그 이름의 함수를 가져오기 주소 표(32비트 이미지)를 고쳐 replacement로
/// 돌립니다. 고친 수를 돌려줍니다.
///
/// # Safety
/// module은 불러온 32비트 이미지여야 하고, replacement는 그 가져오기 형식의 함수여야 합니다.
unsafe fn redirect_import(module: HMODULE, function: &[u8], replacement: usize) -> usize {
    let base = module as *const u8;
    // SAFETY: 헤더가 알려 주는 대로, 불러온 이미지 안의 오프셋.
    let at = |offset: usize| unsafe { base.add(offset).cast::<u32>().read_unaligned() } as usize;
    // SAFETY: 위와 같음.
    let at16 = |offset: usize| unsafe { base.add(offset).cast::<u16>().read_unaligned() };
    let optional = at(0x3c) + 4 + 20;
    // PE32의 선택적 헤더. 데이터 디렉터리는 96바이트 뒤에 시작하고, 가져오기는 두 번째입니다.
    if at16(optional) != 0x10b {
        return 0;
    }
    let imports = at(optional + 96 + 8);
    if imports == 0 {
        return 0;
    }
    let mut rewritten = 0;
    for library in (imports..).step_by(20) {
        let (names, name, slots) = (at(library), at(library + 12), at(library + 16));
        if name == 0 {
            break;
        }
        if names == 0 {
            continue;
        }
        for i in 0.. {
            let entry = at(names + 4 * i);
            if entry == 0 {
                break;
            }
            // 서수로 가져온 것에는 맞춰 볼 이름이 없습니다.
            if entry & 0x8000_0000 != 0 {
                continue;
            }
            // SAFETY: 이미지의 가져오기 표에 있는 이름 (2바이트 힌트 뒤).
            let imported = unsafe { CStr::from_ptr(base.add(entry + 2).cast()) };
            if imported.to_bytes() != function {
                continue;
            }
            // SAFETY: 이미지 안 가져오기 칸. 한 번 쓰려고 쓰기 가능하게 합니다.
            unsafe {
                let slot = base.add(slots + 4 * i).cast_mut().cast::<u32>();
                let mut was: PAGE_PROTECTION_FLAGS = 0;
                VirtualProtect(slot.cast(), 4, PAGE_READWRITE, &mut was);
                slot.write_unaligned(replacement as u32);
                VirtualProtect(slot.cast(), 4, was, &mut was);
            }
            rewritten += 1;
        }
    }
    rewritten
}

/// HKLM\SOFTWARE\Apple…: iCloud for Windows 설치 프로그램이 썼을 곳.
fn is_apple(sub: &str) -> bool {
    sub.get(..14).is_some_and(|start| start.eq_ignore_ascii_case(r"SOFTWARE\Apple"))
}

fn without_view(sam: REG_SAM_FLAGS) -> REG_SAM_FLAGS {
    sam & !(KEY_WOW64_32KEY | KEY_WOW64_64KEY)
}

unsafe extern "system" fn open_key_a(key: HKEY, sub: *const u8, options: u32, sam: REG_SAM_FLAGS, out: *mut HKEY) -> WIN32_ERROR {
    if key == HKEY_LOCAL_MACHINE && !sub.is_null() {
        // SAFETY: RegOpenKeyExA의 하위 키는 C 문자열입니다.
        let name = unsafe { CStr::from_ptr(sub.cast()) }.to_string_lossy();
        if is_apple(&name) {
            let path = format!("{REDIRECT}\\{name}\0");
            // SAFETY: 같은 호출을 HKCU 아래로.
            return unsafe { RegOpenKeyExA(HKEY_CURRENT_USER, path.as_ptr(), options, without_view(sam), out) };
        }
    }
    // SAFETY: AOSKit이 한 호출.
    unsafe { RegOpenKeyExA(key, sub, options, sam, out) }
}

unsafe extern "system" fn open_key_w(key: HKEY, sub: *const u16, options: u32, sam: REG_SAM_FLAGS, out: *mut HKEY) -> WIN32_ERROR {
    if key == HKEY_LOCAL_MACHINE && !sub.is_null() {
        // SAFETY: RegOpenKeyExW의 하위 키는 NUL로 끝나는 와이드 문자열입니다.
        let name = unsafe {
            let len = (0..).take_while(|&i| *sub.add(i) != 0).count();
            String::from_utf16_lossy(std::slice::from_raw_parts(sub, len))
        };
        if is_apple(&name) {
            let path: Vec<u16> = format!("{REDIRECT}\\{name}").encode_utf16().chain([0]).collect();
            // SAFETY: 같은 호출을 HKCU 아래로.
            return unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, path.as_ptr(), options, without_view(sam), out) };
        }
    }
    // SAFETY: AOSKit이 한 호출.
    unsafe { RegOpenKeyExW(key, sub, options, sam, out) }
}
