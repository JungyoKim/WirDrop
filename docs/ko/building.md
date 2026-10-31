# WirDrop 빌드하기

[← README로 돌아가기](../../readme/README.ko.md) · [English](../building.md)

## 빌드

Rust 1.95 이상이 필요합니다. 설치할 파일 전체를 모으려면 Windows SDK(`makeappx`), i686 타깃
(`rustup target add i686-pc-windows-msvc`, 32비트 `appleid-anisette.exe`용),
`wintun.dll`(amd64, [wintun.net](https://www.wintun.net))도 필요합니다.

```powershell
git clone https://github.com/JungyoKim/WirDrop
cd WirDrop
cargo build --release
./packaging/stage.ps1 -Out D:\WirDrop -Wintun C:\path\to\wintun.dll   # WirDrop이 도는 폴더 전체
```

설치 프로그램은 `packaging/installer/WirDrop.iss`(Inno Setup 6)입니다.

```powershell
ISCC.exe /DAppVersion=0.2.0 /DStage=D:\WirDrop /Ooutput packaging\installer\WirDrop.iss
```

## 라이브러리만 쓰기

관리자 터미널에서:

```powershell
cargo build --release -p wirdrop --example wirdrop-cli
cargo build --release -p wirdrop-radio --bin usbfwd
# usbfwd.exe와 wintun.dll을 wirdrop-cli.exe 옆에 두고:
wirdrop-cli receive --name "내 PC" --save "$env:USERPROFILE\Downloads\AirDrop" --accept-all
wirdrop-cli send .\photo.jpg --to iPhone
```

## 릴리스

버전 태그(`v1.2.3`)를 올리면 `.github/workflows/release.yml`이 다음을 만들어 올립니다.

- `WirDrop-Setup-<버전>.exe`: 새로 설치할 때 쓰는 설치 프로그램
- `WirDrop-<버전>.zip`과 서명한 `update.json`: 설치된 WirDrop이 업데이트할 때 받는 것
- `WirDrop-<버전>-winget.zip`: microsoft/winget-pkgs에 낼 winget 매니페스트

워크플로에는 저장소 비밀 `WIRDROP_UPDATE_KEY`가 필요합니다.
`cargo run -p wirdrop-app --example update_sign -- keygen KEY`로 만든 릴리스 키이고, 그 공개 키는
앱(`crates/app/src/update.rs`)에 들어 있어 이 키로 서명하지 않은 업데이트는 받지 않습니다.
워크플로를 직접 실행하면(Actions → Release → Run workflow) 올리지 않고 빌드만 해 봅니다.

## 점검

- `cargo clippy --workspace --all-targets -- -D warnings`와 `cargo test --workspace`. CI가 둘 다,
  그리고 `vendor/`의 owly 테스트도 돌립니다.
- 화면 문구는 한국어와 영어가 있습니다. `WIRDROP_LANG=en`이나 `ko`로 Windows 표시 언어와 관계없이
  골라 볼 수 있습니다.
- `scripts/check-transfer.ps1`은 루프백 전송 테스트(한글 파일 이름, 폴더, 거절, 256 MB 속도)를
  돌리고, 그 결과와 엔진 로그의 실제 전송을 내 PC에 저장한 기준선(`-SaveBaseline`)과 견줍니다.
- 데모는 무선 없이 아일랜드의 모든 카드와 메뉴를 보여 줍니다:
  `cargo run -p wirdrop-island --example demo gallery`, `cargo run -p wirdrop-menu --example demo`.

## 저장소

| 경로 | 내용 | 라이선스 |
|---|---|---|
| `crates/wirdrop` | 라이브러리(AWDL 링크, AirDrop)와 `examples/wirdrop-cli.rs` | MIT |
| `crates/radio` | 어댑터 드라이버 wirdrop-radio와 `usbfwd` | MIT, rtw88의 BSD-3-Clause 코드 포함 ([NOTICE](../../crates/radio/NOTICE.md)) |
| `crates/app` | `WirDrop.exe`: 트레이 앱, 엔진, 서비스, 업데이트 | MIT |
| `crates/island` | 아일랜드 (Windows.UI.Composition, Direct2D) | MIT; `assets/`의 Pretendard(OFL), Framework7 Icons(MIT) |
| `crates/menu`, `crates/shell` | WinUI 3 메뉴와 탐색기 메뉴의 DLL | MIT |
| `crates/appleid`, `crates/appleid-anisette` | Apple ID 로그인과 32비트 도우미 | MIT |
| `vendor/owly-awdl`, `vendor/owly-airdrop` | owly (바꾼 내용은 각 README에) | MIT |
| `packaging` | 파일 모으기, 설치 프로그램, 탐색기 메뉴 패키지, winget 매니페스트 | MIT |
| `scripts` | 개발용 점검 | MIT |
| `assets` | README 이미지 | MIT |

GPL이나 MPL이 적용되는 부분은 없습니다. `usbfwd.exe`에는 Realtek의 RTL8822B 펌웨어
(`crates/radio/firmware`, Realtek 라이선스로 수정 없이 재배포 가능)가 들어 있고, `stage.ps1`이
`crates/radio/NOTICE.md`와 그 라이선스를 함께 넣습니다. iCloud for Windows는 배포물에 넣지 않고,
`appleid.exe`가 PC에서 Apple로부터 받습니다.
