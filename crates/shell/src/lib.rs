//! wirdrop_shell.dll: Windows 11 파일 탐색기 메뉴의 "AirDrop".
//!
//! IExplorerCommand입니다: AirDrop을 고르면 누른 자리에 WirDrop의 기기 메뉴가 열려 주변 기기를
//! 오가는 대로 보여 주고, 하나를 고르면 선택한 파일을 보냅니다. WirDrop.ContextMenu 스파스 패키지가
//! 등록하는 COM 대리 프로세스(dllhost.exe)에서 돌고, WirDrop 트레이 앱에 파이프로 묻습니다. 파이프
//! 호출마다 짧은 기한이 있어 탐색기는 바쁘거나 없는 트레이 앱을 기다리지 않습니다.
//!
//! 하위 메뉴가 아닌 까닭: Windows 11은 상황 메뉴가 열릴 때 명령의 하위 메뉴를 통째로 만들고 열린
//! 동안 다시 묻지 않아, 기기를 찾는 대로 목록을 바꿀 수 없습니다. 찾기는 메뉴가 열릴 때 시작하므로
//! (GetState) AirDrop을 고를 즈음엔 기기를 찾는 중입니다. 기기 메뉴는 누른 자리를 가운데로
//! 열립니다.
#![allow(non_snake_case)]

use std::ffi::c_void;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_NOTIMPL, E_POINTER, HANDLE, HINSTANCE, HMODULE, S_FALSE};
use windows::Win32::System::Com::{CoTaskMemFree, IBindCtx, IClassFactory, IClassFactory_Impl};
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Pipes::GetNamedPipeServerProcessId;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::UI::Shell::{
    ECF_DEFAULT, ECS_ENABLED, IEnumExplorerCommand, IExplorerCommand, IExplorerCommand_Impl, IShellItemArray, SHStrDupW, SIGDN_FILESYSPATH,
    ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{AllowSetForegroundWindow, SW_SHOWNORMAL};
use windows::core::{BOOL, GUID, HRESULT, HSTRING, IUnknown, Interface, PCWSTR, PWSTR, Ref, Result, implement, w};

/// 명령의 클래스. 패키지 매니페스트에도 적혀 있습니다.
pub const CLSID: GUID = GUID::from_u128(0xed292880_3e62_4dbd_8a9a_f484b710a740);

static MODULE: AtomicUsize = AtomicUsize::new(0);

/// 요청이 트레이 앱의 OK를 기다리는 시간.
const SEND_WAIT: Duration = Duration::from_millis(2000);
/// 메뉴를 열 때 찾기를 시작하는 최소 간격: 탐색기는 메뉴마다 명령 상태를 여러 번 묻습니다.
const WAKE_EVERY: Duration = Duration::from_secs(5);

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        MODULE.store(module.0 as usize, Ordering::Relaxed);
    }
    BOOL(1)
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    S_FALSE
}

/// # Safety
/// COM이 올바른 포인터로 부릅니다.
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, out: *mut *mut c_void) -> HRESULT {
    if clsid.is_null() || iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    // SAFETY: 위에서 null을 확인했습니다.
    unsafe {
        *out = std::ptr::null_mut();
        if *clsid != CLSID {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = Factory.into();
        factory.query(iid, out)
    }
}

#[implement(IClassFactory)]
struct Factory;

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, iid: *const GUID, out: *mut *mut c_void) -> Result<()> {
        if out.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: 위에서 null을 확인했습니다.
        unsafe { *out = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let command: IExplorerCommand = Root.into();
        // SAFETY: iid와 out은 올바릅니다.
        unsafe { command.query(iid, out).ok() }
    }

    fn LockServer(&self, _: BOOL) -> Result<()> {
        Ok(())
    }
}

/// 이 DLL 옆의 WirDrop.exe.
fn exe() -> PathBuf {
    let mut buf = [0u16; 1024];
    // SAFETY: 우리 모듈 핸들과 스택 버퍼.
    let n = unsafe { GetModuleFileNameW(Some(HMODULE(MODULE.load(Ordering::Relaxed) as _)), &mut buf) } as usize;
    PathBuf::from(String::from_utf16_lossy(&buf[..n])).with_file_name("WirDrop.exe")
}

/// WirDrop.exe 옆 icons\의 아이콘 파일. WirDrop.exe 자체의 아이콘을 쓰지 않는 까닭: 탐색기는
/// 아이콘을 가져온 파일을 열어 두고, WirDrop.exe는 바꿀 수 있어야 합니다.
fn icon(name: &str) -> String {
    exe().with_file_name("icons").join(name).display().to_string()
}

fn dup(s: &str) -> Result<PWSTR> {
    // SAFETY: SHStrDupW가 부르는 쪽이 해제할 메모리로 문자열을 복사합니다.
    unsafe { SHStrDupW(&HSTRING::from(s)) }
}

fn pipe_name() -> String {
    use windows::Win32::System::RemoteDesktop::ProcessIdToSessionId;
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let mut session = 0;
    // SAFETY: 출력 인자 하나를 채우는 호출.
    let _ = unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) };
    format!(r"\\.\pipe\WirDrop.{session}")
}

/// 트레이 앱에 messages를 보내고 형식이 want인 답을 wait까지 기다립니다 (None이면 답이 없어 전혀
/// 기다리지 않습니다). grant면 먼저 트레이 앱이 전경을 가져가게 합니다: 이 프로세스는 그럴 수 있고
/// (탐색기가 클릭과 함께 넘겨줍니다) 그래서 트레이가 연 것이 앞으로 옵니다. 트레이 앱이 없거나 제때
/// 답하지 않으면 None.
fn ask(messages: Vec<Value>, want: Option<&'static str>, wait: Duration, grant: bool) -> Option<Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let result = (|| -> std::io::Result<Option<Value>> {
            let mut pipe = std::fs::OpenOptions::new().read(true).write(true).open(pipe_name())?;
            if grant {
                use std::os::windows::io::AsRawHandle;
                let mut pid = 0;
                // SAFETY: 우리가 가진 파이프 핸들과 출력 인자.
                unsafe {
                    if GetNamedPipeServerProcessId(HANDLE(pipe.as_raw_handle()), &mut pid).is_ok() {
                        let _ = AllowSetForegroundWindow(pid);
                    }
                }
            }
            let mut out = vec![json!({ "t": "hello", "role": "menu", "pid": std::process::id() })];
            out.extend(messages);
            for m in out {
                writeln!(pipe, "{m}")?;
            }
            pipe.flush()?;
            let Some(want) = want else { return Ok(None) };
            let deadline = Instant::now() + Duration::from_secs(5);
            for line in BufReader::new(&pipe).lines() {
                let v: Value = serde_json::from_str(&line?).unwrap_or_default();
                if v["t"] == want {
                    return Ok(Some(v));
                }
                if Instant::now() > deadline {
                    break;
                }
            }
            Ok(None)
        })();
        let _ = tx.send(result.ok().flatten());
    });
    want?;
    rx.recv_timeout(wait).ok().flatten()
}

/// 선택한 파일들의 경로.
fn paths(items: &Ref<IShellItemArray>) -> Vec<String> {
    let mut paths = Vec::new();
    let Some(items) = items.as_ref() else { return paths };
    // SAFETY: 셸 항목 호출. 돌려받은 문자열은 CoTaskMemFree로 해제합니다.
    unsafe {
        let Ok(count) = items.GetCount() else { return paths };
        for i in 0..count {
            if let Ok(item) = items.GetItemAt(i)
                && let Ok(name) = item.GetDisplayName(SIGDN_FILESYSPATH)
            {
                if let Ok(s) = name.to_string() {
                    paths.push(s);
                }
                CoTaskMemFree(Some(name.0 as _));
            }
        }
    }
    paths
}

/// WirDrop.exe를 인자와 함께 우리 패키지 밖에서 시작합니다. 이 DLL은 패키지의 COM 대리 프로세스에서
/// 돌고, 거기서 패키지 폴더의 프로세스를 띄우면 패키지 정체성을 물려받습니다: 그러면 WirDrop의
/// WinUI 메뉴가 Windows App SDK 부트스트랩을 건너뛰고 실패합니다.
fn launch(args: &str) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Threading::{
        CreateProcessW, DeleteProcThreadAttributeList, EXTENDED_STARTUPINFO_PRESENT, InitializeProcThreadAttributeList, LPPROC_THREAD_ATTRIBUTE_LIST,
        PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY, PROCESS_INFORMATION, STARTUPINFOEXW, UpdateProcThreadAttribute,
    };
    /// PROCESS_CREATION_DESKTOP_APP_BREAKAWAY_OVERRIDE: 자식은 패키지 밖에서 돕니다.
    const BREAKAWAY: u32 = 0x04;
    let exe = exe();
    let mut command: Vec<u16> = format!("\"{}\" {args}", exe.display()).encode_utf16().chain(Some(0)).collect();
    // SAFETY: 첫 호출로 크기를 정하고 아래에서 해제하는 속성 목록. 명령줄은 쓸 수 있는 NUL 종료
    // 버퍼이고, 프로세스 핸들은 닫습니다.
    let started = unsafe {
        let mut size = 0usize;
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
        let mut buf = vec![0u8; size];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(buf.as_mut_ptr().cast());
        let mut started = false;
        if InitializeProcThreadAttributeList(Some(list), 1, None, &mut size).is_ok() {
            let policy = BREAKAWAY;
            if UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_DESKTOP_APP_POLICY as usize,
                Some((&raw const policy).cast()),
                size_of::<u32>(),
                None,
                None,
            )
            .is_ok()
            {
                let mut si = STARTUPINFOEXW::default();
                si.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
                si.lpAttributeList = list;
                let mut pi = PROCESS_INFORMATION::default();
                let dir = HSTRING::from(exe.parent().map(|d| d.as_os_str()).unwrap_or_default());
                if CreateProcessW(
                    &HSTRING::from(exe.as_os_str()),
                    Some(PWSTR(command.as_mut_ptr())),
                    None,
                    None,
                    false,
                    EXTENDED_STARTUPINFO_PRESENT,
                    None,
                    &dir,
                    &si.StartupInfo,
                    &mut pi,
                )
                .is_ok()
                {
                    let _ = CloseHandle(pi.hProcess);
                    let _ = CloseHandle(pi.hThread);
                    started = true;
                }
            }
            DeleteProcThreadAttributeList(list);
        }
        started
    };
    if !started {
        // 패키지 안이 아닙니다 (고전 메뉴): 그냥 시작해도 됩니다.
        // SAFETY: 평범한 문자열.
        unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &HSTRING::from(exe.as_os_str()),
                &HSTRING::from(args),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

fn quoted(files: &[String]) -> String {
    files.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(" ")
}

/// 아무것도 기다리지 않고 찾기를 시작합니다 (트레이 앱이 주변 폰을 깨웁니다).
fn wake() {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.is_some_and(|t| t.elapsed() < WAKE_EVERY) {
        return;
    }
    *last = Some(Instant::now());
    ask(vec![json!({ "t": "wake" })], None, Duration::ZERO, false);
}

/// 파일을 보낼 WirDrop 기기 메뉴를 마우스 자리를 가운데로 엽니다 (위치는 트레이 앱이 구합니다: 이
/// COM 대리 프로세스는 모니터마다의 배율을 모를 수 있습니다). 트레이 앱이 돌면 그것을 통해, 아니면
/// 파일과 함께 시작해서. 폴더나 폴더의 빈 곳이면 files는 그 폴더이고 통째로 보냅니다 (디스크에 없는
/// 폴더면 비어 있고, 그때는 트레이 앱이 파일을 묻습니다).
fn share(files: &[String]) {
    let msg = json!({ "t": "pick", "files": files });
    if ask(vec![msg], Some("ok"), SEND_WAIT, true).is_none() {
        launch(&format!("--pick {}", quoted(files)));
    }
}

/// "AirDrop" 항목.
#[implement(IExplorerCommand)]
struct Root;

impl IExplorerCommand_Impl for Root_Impl {
    fn GetTitle(&self, _: Ref<IShellItemArray>) -> Result<PWSTR> {
        dup("AirDrop")
    }

    fn GetIcon(&self, _: Ref<IShellItemArray>) -> Result<PWSTR> {
        dup(&icon("wirdrop.ico"))
    }

    fn GetToolTip(&self, _: Ref<IShellItemArray>) -> Result<PWSTR> {
        Err(E_NOTIMPL.into())
    }

    fn GetCanonicalName(&self) -> Result<GUID> {
        Ok(CLSID)
    }

    fn GetState(&self, _: Ref<IShellItemArray>, _: BOOL) -> Result<u32> {
        // 메뉴가 열리는 중: AirDrop을 고를 즈음 기기가 들어와 있도록 찾기를 시작합니다.
        wake();
        Ok(ECS_ENABLED.0 as u32)
    }

    fn Invoke(&self, items: Ref<IShellItemArray>, _: Ref<IBindCtx>) -> Result<()> {
        share(&paths(&items));
        Ok(())
    }

    fn GetFlags(&self) -> Result<u32> {
        Ok(ECF_DEFAULT.0 as u32)
    }

    fn EnumSubCommands(&self) -> Result<IEnumExplorerCommand> {
        Err(E_NOTIMPL.into())
    }
}
