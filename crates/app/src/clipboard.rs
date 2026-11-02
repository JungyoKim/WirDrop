//! 받은 것을 클립보드에 올립니다 (설정의 `clipboard`): 받자마자 아무 곳에나 Ctrl+V로 붙여 넣을 수
//! 있게.
//!
//! 파일은 탐색기와 대화 앱이 붙여 넣는 파일 목록(CF_HDROP)으로 올립니다. 받은 것이 그림 한 장이면
//! 그림으로도 올립니다: 원본이 PNG면 그 바이트 그대로("PNG" 꼴, 투명도까지), 그리고 그림판처럼
//! 비트맵만 받는 앱을 위해 CF_DIB로도. 링크는 글자(CF_UNICODETEXT)입니다.
//!
//! 올리려면 주인 창이 있어야 합니다: NULL로 열면 EmptyClipboard가 주인을 비우고 그 뒤
//! SetClipboardData가 실패합니다 (OpenClipboard 문서). 그래서 트레이 창을 주인으로 씁니다.
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Foundation::{GENERIC_READ, GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat24bppBGR, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut,
    WICDecodeMetadataCacheOnLoad,
};
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoIncrementMTAUsage};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, RegisterClipboardFormatW, SetClipboardData};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::core::{HSTRING, w};

/// 붙여 넣을 수 있는 꼴 (winuser.h).
const CF_DIB: u32 = 8;
const CF_UNICODETEXT: u32 = 13;
const CF_HDROP: u32 = 15;

/// 그림으로도 올려 보는 파일. HEIC, AVIF, WEBP는 Windows에 코덱이 있을 때만 열리고, 안 되면 파일로만
/// 올라갑니다.
const PICTURES: &[&str] = &["png", "jpg", "jpeg", "jfif", "heic", "heif", "avif", "gif", "bmp", "tif", "tiff", "webp"];
/// 이보다 큰 그림은 풀지 않습니다: 24비트로 펴면 한 장이 120 MB를 넘습니다.
const MAX_PIXELS: u64 = 40 << 20;
/// 그대로 올릴 PNG의 크기 한계.
const MAX_PNG: u64 = 64 << 20;

/// 푼 그림: 너비, 높이, 줄 하나의 길이, 픽셀. 너무 커서 풀지 않았으면 None입니다.
type Decoded = Option<(u32, u32, usize, Vec<u8>)>;

/// 받은 파일들을 클립보드에 올립니다. 그림을 푸는 데 시간이 걸릴 수 있어 따로 돌립니다: 카드와 알림은
/// 기다리지 않습니다.
pub fn copy_received(owner: isize, paths: Vec<PathBuf>) {
    tokio::task::spawn_blocking(move || match copy_files(owner, &paths) {
        Ok(what) => tracing::info!("clipboard: {what}"),
        Err(e) => tracing::warn!("clipboard: {e}"),
    });
}

/// 받은 링크를 글자로 올립니다.
pub fn copy_link(owner: isize, url: &str) {
    match put(owner, vec![(CF_UNICODETEXT, utf16(url))]) {
        Ok(()) => tracing::info!("clipboard: a link"),
        Err(e) => tracing::warn!("clipboard: {e}"),
    }
}

/// 파일들을 올리고 무엇을 올렸는지 (로그에 쓸 만큼) 돌려줍니다.
fn copy_files(owner: isize, paths: &[PathBuf]) -> io::Result<String> {
    if paths.is_empty() {
        return Err(io::Error::other("nothing to copy"));
    }
    let mut items = vec![(CF_HDROP, drop_list(paths))];
    if let [only] = paths
        && is_picture(only)
    {
        items.extend(picture(only));
    }
    let formats = items.len();
    put(owner, items)?;
    Ok(format!("{} file(s) in {formats} format(s)", paths.len()))
}

/// 클립보드를 비우고 꼴마다 올립니다. 먼저 올린 것을 앱이 먼저 고릅니다.
fn put(owner: isize, items: Vec<(u32, Vec<u8>)>) -> io::Result<()> {
    open(HWND(owner as _))?;
    // SAFETY: 클립보드는 열려 있고, 올린 메모리는 Windows가 가져갑니다 (못 올리면 우리가 놓습니다).
    unsafe {
        let mut failed = EmptyClipboard().err().map(io::Error::other);
        for (format, bytes) in &items {
            if failed.is_some() {
                break;
            }
            match global(bytes) {
                Ok(memory) => {
                    if let Err(e) = SetClipboardData(*format, Some(HANDLE(memory.0))) {
                        let _ = GlobalFree(Some(memory));
                        failed = Some(io::Error::other(e));
                    }
                }
                Err(e) => failed = Some(e),
            }
        }
        let _ = CloseClipboard();
        match failed {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

/// 클립보드를 엽니다. 한 번에 한 프로세스만 열 수 있어, 다른 앱이 쓰고 있으면 잠깐 뒤 다시 합니다.
fn open(hwnd: HWND) -> io::Result<()> {
    for attempt in 0..20 {
        // SAFETY: 우리 창을 주인으로 엽니다.
        if unsafe { OpenClipboard(Some(hwnd)) }.is_ok() {
            return Ok(());
        }
        if attempt < 19 {
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    Err(io::Error::other("another app is holding the clipboard"))
}

/// 바이트를 클립보드가 가져갈 수 있는 메모리에 담습니다.
fn global(bytes: &[u8]) -> io::Result<HGLOBAL> {
    // SAFETY: 받은 크기만큼 잡고, 잡은 만큼만 씁니다.
    unsafe {
        let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len()).map_err(io::Error::other)?;
        let at = GlobalLock(memory);
        if at.is_null() {
            let _ = GlobalFree(Some(memory));
            return Err(io::Error::other("out of memory for the clipboard"));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), at.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(memory);
        Ok(memory)
    }
}

/// CF_HDROP의 내용: DROPFILES(20바이트) 다음에 UTF-16 경로들, 끝에 NUL 하나를 더 둡니다.
fn drop_list(paths: &[PathBuf]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&20u32.to_le_bytes()); // pFiles: 경로가 시작하는 곳
    bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    bytes.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    bytes.extend_from_slice(&0i32.to_le_bytes()); // fNC
    bytes.extend_from_slice(&1i32.to_le_bytes()); // fWide: 경로가 UTF-16
    for path in paths {
        for unit in path.as_os_str().encode_wide().chain(std::iter::once(0)) {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
    }
    bytes.extend_from_slice(&0u16.to_le_bytes());
    bytes
}

/// CF_UNICODETEXT의 내용: UTF-16에 NUL 하나.
fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity((text.len() + 1) * 2);
    for unit in text.encode_utf16().chain(std::iter::once(0)) {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn is_picture(path: &Path) -> bool {
    path.extension().is_some_and(|e| PICTURES.iter().any(|p| e.eq_ignore_ascii_case(p))) && path.is_file()
}

/// 그림 한 장을 그림으로 붙여 넣을 수 있는 꼴들로. 못 읽으면 빈 목록이고, 파일로는 이미 올라갑니다.
fn picture(path: &Path) -> Vec<(u32, Vec<u8>)> {
    let mut items = Vec::new();
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))
        && std::fs::metadata(path).is_ok_and(|m| m.len() <= MAX_PNG)
        && let Ok(bytes) = std::fs::read(path)
    {
        // SAFETY: 상수 이름으로 꼴 하나를 등록합니다 (브라우저와 Office가 쓰는 "PNG").
        let png = unsafe { RegisterClipboardFormatW(w!("PNG")) };
        if png != 0 {
            items.push((png, bytes));
        }
    }
    match dib(path) {
        Ok(bytes) => items.push((CF_DIB, bytes)),
        Err(e) => tracing::debug!("clipboard: no bitmap for {}: {e}", path.display()),
    }
    items
}

/// 그림을 CF_DIB로: BITMAPINFOHEADER 다음에 아래에서 위로 쌓은 24비트 BGR 줄들.
fn dib(path: &Path) -> Result<Vec<u8>, String> {
    com();
    let decoded = (|| -> windows::core::Result<Decoded> {
        // SAFETY: WIC로 파일 하나를 풀어 우리 버퍼에 담습니다. 버퍼는 줄 길이 x 높이로 잡습니다.
        unsafe {
            let wic: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let file = HSTRING::from(path.as_os_str());
            let decoder = wic.CreateDecoderFromFilename(&file, None, GENERIC_READ, WICDecodeMetadataCacheOnLoad)?;
            let frame = decoder.GetFrame(0)?;
            let (mut width, mut height) = (0u32, 0u32);
            frame.GetSize(&mut width, &mut height)?;
            if width == 0 || height == 0 || u64::from(width) * u64::from(height) > MAX_PIXELS {
                return Ok(None);
            }
            let converter = wic.CreateFormatConverter()?;
            converter.Initialize(
                &frame,
                &GUID_WICPixelFormat24bppBGR,
                WICBitmapDitherTypeNone,
                None,
                0.0,
                WICBitmapPaletteTypeMedianCut,
            )?;
            // DIB의 줄은 4바이트에 맞춰 놓입니다.
            let stride = (width as usize * 3).next_multiple_of(4);
            let mut pixels = vec![0u8; stride * height as usize];
            converter.CopyPixels(std::ptr::null(), stride as u32, &mut pixels)?;
            Ok(Some((width, height, stride, pixels)))
        }
    })();
    let (width, height, stride, pixels) = decoded.map_err(|e| e.to_string())?.ok_or("the picture is too large")?;
    let mut bytes = Vec::with_capacity(40 + pixels.len());
    bytes.extend_from_slice(&dib_header(width, height, pixels.len() as u32));
    // DIB의 첫 줄은 그림의 마지막 줄입니다.
    for row in pixels.chunks_exact(stride).rev() {
        bytes.extend_from_slice(row);
    }
    Ok(bytes)
}

/// BITMAPINFOHEADER 40바이트: 24비트 BGR, biHeight가 양수라 아래에서 위로. 나머지 칸은 0입니다
/// (BI_RGB, 쓰는 색 수 없음).
fn dib_header(width: u32, height: u32, size: u32) -> [u8; 40] {
    let mut header = [0u8; 40];
    let mut put = |at: usize, bytes: &[u8]| header[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, &40u32.to_le_bytes()); // biSize
    put(4, &(width as i32).to_le_bytes());
    put(8, &(height as i32).to_le_bytes());
    put(12, &1u16.to_le_bytes()); // biPlanes
    put(14, &24u16.to_le_bytes()); // biBitCount
    put(20, &size.to_le_bytes()); // biSizeImage
    header
}

/// WIC를 쓰려면 이 스레드가 COM 아파트에 있어야 합니다. 트레이는 이미 MTA를 띄워 두지만, 여기서도
/// 한 번 띄워 어느 스레드에서 불려도 되게 합니다 (쿠키는 일부러 두어 프로세스가 끝날 때까지 삽니다).
fn com() {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: MTA를 붙잡기만 합니다.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drop_list_is_a_header_then_utf16_paths_and_one_more_nul() {
        let bytes = drop_list(&[PathBuf::from("C:\\a.txt"), PathBuf::from("C:\\b.txt")]);
        assert_eq!(u32::from_le_bytes(bytes[..4].try_into().unwrap()), 20);
        assert_eq!(i32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1, "fWide");
        let names: Vec<u8> = "C:\\a.txt\0C:\\b.txt\0\0".encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(&bytes[20..], names);
    }

    #[test]
    fn a_bitmap_header_says_24_bit_bottom_up() {
        let header = dib_header(3, 2, 24);
        assert_eq!(u32::from_le_bytes(header[..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(header[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(header[14..16].try_into().unwrap()), 24);
        assert_eq!(u32::from_le_bytes(header[16..20].try_into().unwrap()), 0, "BI_RGB");
        assert_eq!(u32::from_le_bytes(header[20..24].try_into().unwrap()), 24);
    }

    #[test]
    fn only_picture_names_are_offered_as_pictures() {
        assert!(!is_picture(Path::new("C:\\holiday.mp4")));
        assert!(!is_picture(Path::new("C:\\notes")));
    }
}
