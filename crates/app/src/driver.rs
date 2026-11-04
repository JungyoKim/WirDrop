//! usbfwd가 다루는 WinUSB 위의 USB 어댑터.
//!
//! Windows 자체의 winusb.inf(모든 Windows에 있는 Microsoft의 것; 장치는 winusb.sys가 돌립니다)를,
//! 장치 관리자에서 Windows에 맡기지 않고 목록에서 드라이버를 고를 때처럼 어댑터에 입힙니다: 장치
//! 클래스를 USB 장치로 바꾸고, winusb.inf의 "WinUsb Device"를 골라 설치하고, usbfwd가 장치를 여는
//! 장치 인터페이스를 하드웨어 키에 씁니다 (winusb.inf는 장치에 맡기는데 이 장치는 알려 주지
//! 않습니다). WirDrop의 드라이버 패키지도 인증서도 없습니다. 끄면 어댑터를 Windows의 선택(Realtek
//! Wi-Fi 드라이버)으로 돌려줍니다. [needs_binding]과 [eject_driver_cd] 말고는 모두 관리자 권한이
//! 필요합니다 (WirDrop.exe --setup --driver on|off).
use std::io;

use windows::Win32::Devices::DeviceAndDriverInstallation::{
    CM_LOCATE_DEVNODE_NORMAL, CM_Locate_DevNodeW, CM_REENUMERATE_SYNCHRONOUS, CM_Reenumerate_DevNode, CR_SUCCESS, DI_ENUMSINGLEINF,
    DI_FLAGSEX_ALLOWEXCLUDEDDRVS, DI_REMOVEDEVICE_GLOBAL, DICS_FLAG_GLOBAL, DICS_PROPCHANGE, DIF_PROPERTYCHANGE, DIF_REMOVE, DIGCF_ALLCLASSES,
    DIGCF_PRESENT, DIIDFLAG_NOFINISHINSTALLUI, DIREG_DEV, DiInstallDevice, HDEVINFO, SP_CLASSINSTALL_HEADER, SP_DEVINFO_DATA, SP_DEVINSTALL_PARAMS_W,
    SP_DRVINFO_DATA_V2_W, SP_DRVINFO_DETAIL_DATA_W, SP_PROPCHANGE_PARAMS, SP_REMOVEDEVICE_PARAMS, SPDIT_CLASSDRIVER, SPDRP_CLASSGUID, SPDRP_SERVICE,
    SetupDiBuildDriverInfoList, SetupDiCallClassInstaller, SetupDiCreateDevRegKeyW, SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo,
    SetupDiEnumDriverInfoW, SetupDiGetClassDevsW, SetupDiGetDeviceInstallParamsW, SetupDiGetDeviceInstanceIdW, SetupDiGetDeviceRegistryPropertyW,
    SetupDiGetDriverInfoDetailW, SetupDiOpenDevRegKey, SetupDiSetClassInstallParamsW, SetupDiSetDeviceInstallParamsW,
    SetupDiSetDeviceRegistryPropertyW, SetupDiSetSelectedDriverW,
};
use windows::Win32::System::Registry::{HKEY, KEY_READ, REG_MULTI_SZ, RRF_RT_REG_MULTI_SZ, RegCloseKey, RegGetValueW, RegSetValueExW};
use windows::core::{BOOL, GUID, PCWSTR, w};

/// USB devices (Device Manager's "범용 직렬 버스 장치"), winusb.inf's class.
const USB_DEVICE_CLASS: GUID = GUID::from_u128(0x88bae032_5a81_49f0_bc3d_a4ff138216d6);
/// usbfwd가 어댑터를 찾는 장치 인터페이스 (radio::usb가 같은 키에서 읽습니다).
const INTERFACE: &str = "{F6747A8B-61CA-4BB9-9F4B-4CA7B8F7C041}";

/// 꽂혀 있는 지원 어댑터들 (SetupAPI 목록).
struct Adapters {
    set: HDEVINFO,
    devices: Vec<(SP_DEVINFO_DATA, String)>,
}

impl Drop for Adapters {
    fn drop(&mut self) {
        // SAFETY: adapters()가 만든 목록을 한 번 없앱니다.
        unsafe {
            let _ = SetupDiDestroyDeviceInfoList(self.set);
        }
    }
}

fn adapters() -> io::Result<Adapters> {
    // SAFETY: 여기서 만든 목록에 대한 SetupAPI 호출이고, 버퍼는 지역 변수입니다.
    unsafe {
        let set = SetupDiGetClassDevsW(None, w!("USB"), None, DIGCF_PRESENT | DIGCF_ALLCLASSES)?;
        let mut found = Adapters { set, devices: Vec::new() };
        for i in 0.. {
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInfo(set, i, &mut data).is_err() {
                break;
            }
            let mut id = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut id), None).is_err() {
                continue;
            }
            let id = String::from_utf16_lossy(&id[..id.iter().position(|&c| c == 0).unwrap_or(id.len())]).to_ascii_uppercase();
            // 복합 장치의 인터페이스(…&MI_00)가 아닌 장치 자체.
            let ours = crate::win::ADAPTERS
                .iter()
                .any(|(vid, pid, _)| id.starts_with(&format!(r"USB\VID_{vid:04X}&PID_{pid:04X}\")));
            if ours {
                found.devices.push((data, id));
            }
        }
        Ok(found)
    }
}

/// 장치의 문자열 속성 (예: 서비스). 있으면.
fn property(
    set: HDEVINFO,
    data: &SP_DEVINFO_DATA,
    which: windows::Win32::Devices::DeviceAndDriverInstallation::SETUP_DI_REGISTRY_PROPERTY,
) -> String {
    let mut buf = [0u8; 1024];
    // SAFETY: 크기를 준 지역 버퍼로 읽습니다.
    if unsafe { SetupDiGetDeviceRegistryPropertyW(set, data, which, None, Some(&mut buf), None) }.is_err() {
        return String::new();
    }
    let wide: Vec<u16> = buf
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .take_while(|&c| c != 0)
        .collect();
    String::from_utf16_lossy(&wide)
}

/// 장치가 usbfwd가 여는 인터페이스와 함께 WinUSB로 돕니다.
fn bound(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> bool {
    if !property(set, data, SPDRP_SERVICE).eq_ignore_ascii_case("WinUSB") {
        return false;
    }
    // SAFETY: 장치의 하드웨어 키를 지역 버퍼로 읽고 닫습니다.
    unsafe {
        let Ok(key) = SetupDiOpenDevRegKey(set, data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, KEY_READ.0) else {
            return false;
        };
        let mut buf = [0u16; 512];
        let mut size = (buf.len() * 2) as u32;
        let read = RegGetValueW(
            key,
            PCWSTR::null(),
            w!("DeviceInterfaceGUIDs"),
            RRF_RT_REG_MULTI_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        );
        let _ = RegCloseKey(key);
        read.is_ok()
            && String::from_utf16_lossy(&buf[..size as usize / 2])
                .to_ascii_uppercase()
                .contains(INTERFACE)
    }
}

/// 지원 어댑터가 꽂혀 있지만 아직 WinUSB가 아닙니다. 권한이 필요 없습니다.
pub fn needs_binding() -> bool {
    adapters().is_ok_and(|a| a.devices.iter().any(|(data, _)| !bound(a.set, data)))
}

/// RTL8912AU 어댑터(ipTIME BE6500UA)는 꽂을 때마다 먼저 Realtek 드라이버 설치 파일을 담은 가상
/// CD(0bda:1a2b, "RTK Driver Storage")로 나타나고, CD를 꺼내야 무선 랜(0bda:8912)으로 바뀝니다.
/// 그런 CD를 모두 꺼내고 꺼낸 수를 돌려줍니다. 드라이브 문자가 붙기 전에도 찾도록 CD-ROM 장치
/// 인터페이스로 찾고, 다른 CD 드라이브는 건드리지 않습니다. 권한이 필요 없습니다.
pub fn eject_driver_cd() -> usize {
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_DEVICEINTERFACE, SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W, SetupDiEnumDeviceInterfaces,
        SetupDiGetDeviceInterfaceDetailW,
    };
    use windows::Win32::Foundation::{CloseHandle, GENERIC_READ};
    use windows::Win32::Storage::FileSystem::{CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows::Win32::System::IO::DeviceIoControl;
    use windows::Win32::System::Ioctl::{GUID_DEVINTERFACE_CDROM, IOCTL_STORAGE_EJECT_MEDIA};
    let mut ejected = 0;
    // SAFETY: 여기서 만든 목록에 대한 SetupAPI 호출, 지역 버퍼, 연 핸들은 닫습니다.
    unsafe {
        let Ok(set) = SetupDiGetClassDevsW(
            Some(&GUID_DEVINTERFACE_CDROM),
            PCWSTR::null(),
            None,
            DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
        ) else {
            return 0;
        };
        for i in 0.. {
            let mut iface = SP_DEVICE_INTERFACE_DATA {
                cbSize: size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDeviceInterfaces(set, None, &GUID_DEVINTERFACE_CDROM, i, &mut iface).is_err() {
                break;
            }
            let mut buf = vec![0u64; 256];
            let detail = buf.as_mut_ptr().cast::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DEVICE_INTERFACE_DETAIL_DATA_W>() as u32;
            let mut data = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ..Default::default()
            };
            if SetupDiGetDeviceInterfaceDetailW(set, &iface, Some(detail), (buf.len() * 8) as u32, None, Some(&mut data)).is_err() {
                continue;
            }
            let mut id = [0u16; 512];
            if SetupDiGetDeviceInstanceIdW(set, &data, Some(&mut id), None).is_err() {
                continue;
            }
            let id = String::from_utf16_lossy(&id[..id.iter().position(|&c| c == 0).unwrap_or(id.len())]).to_ascii_uppercase();
            if !id.contains("VEN_RTK&PROD_DRIVER_STORAGE") {
                continue;
            }
            let path = PCWSTR((*detail).DevicePath.as_ptr());
            let file = CreateFileW(
                path,
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_FLAGS_AND_ATTRIBUTES(0),
                None,
            );
            let result = file.and_then(|h| {
                let mut returned = 0u32;
                let r = DeviceIoControl(h, IOCTL_STORAGE_EJECT_MEDIA, None, 0, None, 0, Some(&mut returned), None);
                let _ = CloseHandle(h);
                r
            });
            match result {
                Ok(()) => {
                    tracing::info!("driver: ejected the adapter's driver CD ({id}) so it comes back as Wi-Fi");
                    ejected += 1;
                }
                Err(e) => tracing::warn!("driver: can't eject the adapter's driver CD ({id}): {e}"),
            }
        }
        let _ = SetupDiDestroyDeviceInfoList(set);
    }
    ejected
}

/// 어댑터마다 WirDrop이 무엇을 할지 아무것도 바꾸지 않고 적습니다 (setup.log).
pub fn probe() -> io::Result<()> {
    let a = adapters()?;
    for (data, id) in &a.devices {
        let mut data = *data;
        tracing::info!(
            "driver: {id}: service {:?}, class {:?}, on WinUSB for WirDrop: {}",
            property(a.set, &data, SPDRP_SERVICE),
            property(a.set, &data, SPDRP_CLASSGUID),
            bound(a.set, &data)
        );
        // SAFETY: bind_one과 같되, 아무것도 고르거나 설치하지 않습니다.
        unsafe {
            winusb_list(a.set, &mut data)?;
            match winusb_driver(a.set, &data) {
                Ok(drv) => tracing::info!("driver: it would get {:?} by {:?}", text(&drv.Description), text(&drv.ProviderName)),
                Err(e) => tracing::warn!("driver: {e}"),
            }
        }
    }
    if a.devices.is_empty() {
        tracing::info!("driver: no supported adapter is plugged in");
    }
    Ok(())
}

/// 꽂혀 있는 지원 어댑터가 WinUSB가 아니면 WinUSB로 바꿉니다. 바꾼 수를 돌려줍니다.
pub fn bind() -> io::Result<usize> {
    let a = adapters()?;
    let mut changed = 0;
    for (data, id) in &a.devices {
        if bound(a.set, data) {
            tracing::info!("driver: {id} is on WinUSB already");
            continue;
        }
        let mut data = *data;
        tracing::info!("driver: {id} on {:?}: putting it on WinUSB", property(a.set, &data, SPDRP_SERVICE));
        // SAFETY: 목록의 장치에 대한 SetupAPI 호출, 구조체 크기는 호출에 맞춥니다.
        unsafe { bind_one(a.set, &mut data) }?;
        changed += 1;
    }
    Ok(changed)
}

/// winusb.inf의 드라이버를 이 장치의 클래스 드라이버로 둡니다. 장치 관리자 목록에서 그 종류를 고를
/// 때처럼 먼저 클래스를 USB 장치로 바꿉니다.
unsafe fn winusb_list(set: HDEVINFO, data: &mut SP_DEVINFO_DATA) -> io::Result<()> {
    unsafe {
        let mut params = SP_DEVINSTALL_PARAMS_W {
            cbSize: size_of::<SP_DEVINSTALL_PARAMS_W>() as u32,
            ..Default::default()
        };
        SetupDiGetDeviceInstallParamsW(set, Some(data), &mut params)?;
        params.Flags |= DI_ENUMSINGLEINF;
        params.FlagsEx |= DI_FLAGSEX_ALLOWEXCLUDEDDRVS;
        let windir = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let inf: Vec<u16> = format!(r"{windir}\INF\winusb.inf").encode_utf16().collect();
        params.DriverPath = [0; 260];
        params.DriverPath[..inf.len()].copy_from_slice(&inf);
        SetupDiSetDeviceInstallParamsW(set, Some(data), &params)?;
        SetupDiBuildDriverInfoList(set, Some(data), SPDIT_CLASSDRIVER)?;
        Ok(())
    }
}

/// winusb_list가 만든 목록 속 winusb.inf의 "WinUsb Device" (Billboard와 ADB 장치도 있습니다; 이름은
/// Windows 언어에 따라 바뀌지만 설치 섹션은 그대로입니다).
unsafe fn winusb_driver(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> io::Result<SP_DRVINFO_DATA_V2_W> {
    unsafe {
        for i in 0.. {
            let mut drv = SP_DRVINFO_DATA_V2_W {
                cbSize: size_of::<SP_DRVINFO_DATA_V2_W>() as u32,
                ..Default::default()
            };
            if SetupDiEnumDriverInfoW(set, Some(data), SPDIT_CLASSDRIVER, i, &mut drv).is_err() {
                break;
            }
            // 상세 정보 끝에 길이가 정해지지 않은 하드웨어 ID가 붙으니 자리를 넉넉히 잡습니다.
            let mut buf = vec![0u64; 1024];
            let detail = buf.as_mut_ptr().cast::<SP_DRVINFO_DETAIL_DATA_W>();
            (*detail).cbSize = size_of::<SP_DRVINFO_DETAIL_DATA_W>() as u32;
            if SetupDiGetDriverInfoDetailW(set, Some(data), &drv, Some(detail), (buf.len() * 8) as u32, None).is_err() {
                continue;
            }
            let section = text(&(*detail).SectionName).to_ascii_uppercase();
            if section == "WINUSB" {
                return Ok(drv);
            }
        }
        Err(io::Error::other("winusb.inf offers no WinUsb Device for it"))
    }
}

unsafe fn bind_one(set: HDEVINFO, data: &mut SP_DEVINFO_DATA) -> io::Result<()> {
    unsafe {
        let class: Vec<u8> = format!("{{{USB_DEVICE_CLASS:?}}}\0").encode_utf16().flat_map(u16::to_le_bytes).collect();
        SetupDiSetDeviceRegistryPropertyW(set, data, SPDRP_CLASSGUID, Some(&class))?;
        data.ClassGuid = USB_DEVICE_CLASS;
        winusb_list(set, data)?;
        let mut drv = winusb_driver(set, data)?;
        tracing::info!("driver: installing {:?}", text(&drv.Description));
        SetupDiSetSelectedDriverW(set, Some(data), Some(&mut drv))?;
        // 설치 전에 써야 WinUSB가 시작하면서 인터페이스를 등록합니다. 설치가 키를 새로 썼을 수 있어
        // 설치 뒤에도 다시 쓰고, 장치를 다시 시작해 반영합니다.
        interface(set, data)?;
        let mut reboot = BOOL(0);
        DiInstallDevice(None, set, data, Some(&drv), DIIDFLAG_NOFINISHINSTALLUI, Some(&mut reboot))?;
        interface(set, data)?;
        let params = SP_PROPCHANGE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_PROPERTYCHANGE,
            },
            StateChange: DICS_PROPCHANGE,
            Scope: DICS_FLAG_GLOBAL,
            HwProfile: 0,
        };
        SetupDiSetClassInstallParamsW(
            set,
            Some(data),
            Some(&params.ClassInstallHeader),
            size_of::<SP_PROPCHANGE_PARAMS>() as u32,
        )?;
        SetupDiCallClassInstaller(DIF_PROPERTYCHANGE, set, Some(data))?;
        if reboot.as_bool() {
            tracing::warn!("driver: Windows wants a restart to finish");
        }
        Ok(())
    }
}

/// usbfwd가 여는 인터페이스를 장치의 하드웨어 키에 씁니다.
unsafe fn interface(set: HDEVINFO, data: &SP_DEVINFO_DATA) -> io::Result<()> {
    unsafe {
        let key: HKEY = SetupDiCreateDevRegKeyW(set, data, DICS_FLAG_GLOBAL.0, 0, DIREG_DEV, None, PCWSTR::null())?;
        let value: Vec<u8> = format!("{INTERFACE}\0\0").encode_utf16().flat_map(u16::to_le_bytes).collect();
        let set_value = RegSetValueExW(key, w!("DeviceInterfaceGUIDs"), None, REG_MULTI_SZ, Some(&value));
        let _ = RegCloseKey(key);
        set_value.ok()?;
        Ok(())
    }
}

/// 지원 어댑터마다 WinUSB를 뗍니다: 장치를 지웠다가 다시 찾게 해, Windows가 고른 드라이버가
/// 붙습니다. 바꾼 수를 돌려줍니다.
pub fn unbind() -> io::Result<usize> {
    let a = adapters()?;
    let mut changed = 0;
    for (data, id) in &a.devices {
        if !property(a.set, data, SPDRP_SERVICE).eq_ignore_ascii_case("WinUSB") {
            continue;
        }
        tracing::info!("driver: {id}: off WinUSB");
        let params = SP_REMOVEDEVICE_PARAMS {
            ClassInstallHeader: SP_CLASSINSTALL_HEADER {
                cbSize: size_of::<SP_CLASSINSTALL_HEADER>() as u32,
                InstallFunction: DIF_REMOVE,
            },
            Scope: DI_REMOVEDEVICE_GLOBAL,
            HwProfile: 0,
        };
        // SAFETY: 목록의 장치에 대한 SetupAPI 호출, 구조체 크기는 호출에 맞춥니다.
        unsafe {
            SetupDiSetClassInstallParamsW(
                a.set,
                Some(data),
                Some(&params.ClassInstallHeader),
                size_of::<SP_REMOVEDEVICE_PARAMS>() as u32,
            )?;
            SetupDiCallClassInstaller(DIF_REMOVE, a.set, Some(data))?;
        }
        changed += 1;
    }
    if changed > 0 {
        // SAFETY: 루트 장치 노드를 찾아 다시 열거합니다.
        unsafe {
            let mut root = 0u32;
            if CM_Locate_DevNodeW(&mut root, PCWSTR::null(), CM_LOCATE_DEVNODE_NORMAL) == CR_SUCCESS {
                let _ = CM_Reenumerate_DevNode(root, CM_REENUMERATE_SYNCHRONOUS);
            }
        }
    }
    Ok(changed)
}

fn text(s: &[u16]) -> String {
    String::from_utf16_lossy(&s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())])
}
