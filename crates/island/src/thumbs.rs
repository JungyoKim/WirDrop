//! 섬의 스레드 밖에서 만드는 미리보기. 방금 받은 파일의 셸 미리보기는 아직 캐시에 없고,
//! 만들려면(사진 디코딩, 영상 프레임 읽기) 0.1~0.25초가 걸립니다: 애니메이션을 시작하는 섬의
//! 스레드는 그동안 멈출 수 없어 일꾼이 만들고, 섬은 오는 대로 그립니다.
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

use windows::Win32::Foundation::SIZE;
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_THUMBNAILONLY};
use windows::core::{HSTRING, Result};

/// 만들 미리보기: 파일과 크기 (한 변의 최소 픽셀).
pub type Ask = (PathBuf, u32);

/// 만든 미리보기: 파일, 요청한 크기, 비트맵 (HBITMAP, 가져가는 쪽이 지웁니다). 파일에 미리보기가
/// 없으면 none.
pub struct Made {
    pub path: PathBuf,
    pub side: u32,
    pub bitmap: Option<isize>,
}

/// 일꾼을 시작합니다. 파일과 크기가 들어가고 만든 것이 나오며, 하나마다 (섬을 깨우려고) done을
/// 부릅니다.
pub fn spawn(done: impl Fn() + Send + 'static) -> std::io::Result<(Sender<Ask>, Receiver<Made>)> {
    let (ask, asked) = channel::<Ask>();
    let (made, out) = channel();
    std::thread::Builder::new().name("island-thumbnails".into()).spawn(move || {
        // 미리보기 처리기는 단일 스레드 아파트를 원할 수 있습니다.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        while let Ok((path, side)) = asked.recv() {
            let bitmap = thumbnail(&path, side)
                .map_err(|e| tracing::debug!("no thumbnail for {}: {e}", path.display()))
                .ok();
            if made.send(Made { path, side, bitmap }).is_err() {
                break;
            }
            done();
        }
    })?;
    Ok((ask, out))
}

fn thumbnail(path: &Path, side: u32) -> Result<isize> {
    unsafe {
        let item: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(path.as_os_str()), None)?;
        let side = side as i32;
        let bitmap = item.GetImage(SIZE { cx: side, cy: side }, SIIGBF_THUMBNAILONLY | SIIGBF_BIGGERSIZEOK)?;
        Ok(bitmap.0 as isize)
    }
}
