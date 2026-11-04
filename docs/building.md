# Building WirDrop

[← Back to the README](../README.md) · [한국어](ko/building.md)

## Build

You need Rust 1.95 or newer. To stage a full install you also need the Windows SDK (`makeappx`),
the i686 target (`rustup target add i686-pc-windows-msvc`, for the 32-bit
`appleid-anisette.exe`) and `wintun.dll` (amd64, from [wintun.net](https://www.wintun.net)).

```powershell
git clone https://github.com/JungyoKim/WirDrop
cd WirDrop
cargo build --release
./packaging/stage.ps1 -Out D:\WirDrop -Wintun C:\path\to\wintun.dll   # everything WirDrop runs from
```

The setup program is `packaging/installer/WirDrop.iss` (Inno Setup 6):

```powershell
ISCC.exe /DAppVersion=0.2.0 /DStage=D:\WirDrop /Ooutput packaging\installer\WirDrop.iss
```

## The library on its own

From an administrator terminal:

```powershell
cargo build --release -p wirdrop --example wirdrop-cli
cargo build --release -p wirdrop-radio --bin usbfwd
# usbfwd.exe and wintun.dll beside wirdrop-cli.exe, then:
wirdrop-cli receive --name "My PC" --save "$env:USERPROFILE\Downloads\AirDrop" --accept-all
wirdrop-cli send .\photo.jpg --to iPhone
```

## Releases

Pushing a version tag (`v1.2.3`) runs `.github/workflows/release.yml`, which builds and publishes:

- `WirDrop-Setup-<version>.exe`, the setup program for new installs
- `WirDrop-<version>.zip` and its signed `update.json`, which installed copies update from
- `WirDrop-<version>-winget.zip`, the winget manifests ready to submit to microsoft/winget-pkgs

The workflow needs the repository secret `WIRDROP_UPDATE_KEY`, the release key made by
`cargo run -p wirdrop-app --example update_sign -- keygen KEY`. Its public half is built into the
app (`crates/app/src/update.rs`), which refuses any update not signed with it. Run the workflow
by hand (Actions → Release → Run workflow) to build everything without publishing.

## Checks

- `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace`. CI runs
  both, plus owly's tests in `vendor/`.
- Screen text comes in Korean and English. Set `WIRDROP_LANG=en` or `ko` to try either one
  regardless of the Windows display language.
- `scripts/check-transfer.ps1` runs a loopback transfer test (Korean file names, folders, a
  declined request and a 256 MB speed run) and compares it, and the real transfers in the engine's
  log, with a baseline saved on your PC (`-SaveBaseline`).
- The demos show every island card and menu without a radio:
  `cargo run -p wirdrop-island --example demo gallery` and `cargo run -p wirdrop-menu --example demo`.

## Repository

| Path | What | License |
|---|---|---|
| `crates/wirdrop` | the library (AWDL link, AirDrop) and `examples/wirdrop-cli.rs` | MIT |
| `crates/radio` | wirdrop-radio, the adapter driver, and `usbfwd` | MIT, with BSD-3-Clause code from rtw88 and rtw89 ([NOTICE](../crates/radio/NOTICE.md)) |
| `crates/app` | `WirDrop.exe`: tray app, engine, service, updater | MIT |
| `crates/island` | the island (Windows.UI.Composition, Direct2D) | MIT; Pretendard (OFL) and Framework7 Icons (MIT) in `assets/` |
| `crates/menu`, `crates/shell` | the WinUI 3 menus and the Explorer menu's DLL | MIT |
| `crates/appleid`, `crates/appleid-anisette` | the Apple ID sign-in and its 32-bit helper | MIT |
| `vendor/owly-awdl`, `vendor/owly-airdrop` | owly, with the changes listed in their READMEs | MIT |
| `packaging` | staging, the installer, the Explorer menu package, winget manifests | MIT |
| `scripts` | development checks | MIT |
| `assets` | README images | MIT |

No part of WirDrop is under the GPL or the MPL. `usbfwd.exe` embeds Realtek's RTL8822B firmware
(`crates/radio/firmware`), redistributable unmodified under Realtek's license; `stage.ps1` ships
`crates/radio/NOTICE.md` and that license with it. iCloud for Windows is not shipped:
`appleid.exe` downloads it from Apple on the PC.
