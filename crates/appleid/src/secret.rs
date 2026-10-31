//! appleid가 암호 없이 로그인을 유지하려고 실행 사이에 두는 것 (AirDrop 앱 토큰, 로그인 세션,
//! iCloud 토큰): DPAPI로 보호해 이 Windows 사용자만 다시 읽습니다.
use std::path::Path;

/// 보호한 파일의 표시 (이미 PC에 있는 파일들이 이 표시를 갖고 있어 바꾸지 않습니다).
const MAGIC: &[u8] = b"WINID-DPAPI-1\n";

/// 이 Windows 사용자용으로 보호한 data를 씁니다.
pub fn write(path: &Path, data: &[u8]) -> Result<(), String> {
    let mut out = MAGIC.to_vec();
    out.extend(dpapi(data, true)?);
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

/// write가 쓴 것을 읽습니다. 보호되지 않은 파일은 읽지 않습니다.
pub fn read(path: &Path) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match bytes.strip_prefix(MAGIC) {
        Some(blob) => dpapi(blob, false),
        None => Err(format!("{}: not protected", path.display())),
    }
}

#[cfg(windows)]
fn dpapi(data: &[u8], protect: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CRYPT_INTEGER_BLOB, CryptProtectData, CryptUnprotectData};
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    // SAFETY: input은 호출 동안 data를 가리키고, output은 Windows가 할당해 복사한 뒤 해제합니다.
    let ok = unsafe {
        if protect {
            CryptProtectData(
                &input,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                0,
                &mut output,
            )
        }
    };
    if ok == 0 || output.pbData.is_null() {
        return Err(format!("DPAPI failed ({})", std::io::Error::last_os_error()));
    }
    // SAFETY: Windows가 pbData에 cbData 바이트를 돌려주었습니다.
    let bytes = unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    // SAFETY: CryptProtectData/CryptUnprotectData가 LocalAlloc으로 할당했습니다.
    unsafe { LocalFree(output.pbData as _) };
    Ok(bytes)
}
