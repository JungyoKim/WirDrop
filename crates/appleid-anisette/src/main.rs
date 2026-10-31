//! appleid-anisette: Apple 로그인 서버가 Windows PC에 요구하는 일회용 암호 헤더(X-Apple-I-MD,
//! X-Apple-I-MD-M)를 Apple 자신의 코드, iCloud for Windows 7.21의 AOSKit으로 구합니다. AOSKit은
//! 32비트라 i686용으로 빌드한 따로 프로세스입니다. appleid.exe가 iCloud 폴더를 PATH에 두고 시작해
//! 표준 입출력으로 한 줄씩 묻고, 입력이 끝날 때까지 답합니다:
//!
//! ```text
//! otp   ->   {"md":"…","mdm":"…"}   or   {"error":"…"}
//! ```
//!
//! 풀기만 한 iCloud for Windows에는 폴더 위치를 알려 줄 HKLM\SOFTWARE\Apple Inc. 항목이 없고, 권한
//! 없는 프로그램은 만들 수도 없습니다: 그래서 AOSKit이 거기서 찾는 것을
//! HKCU\Software\WirDrop\HKLM으로 돌리고, appleid.exe가 그곳에 씁니다.

#[cfg(all(windows, target_arch = "x86"))]
mod aoskit;

fn main() {
    #[cfg(all(windows, target_arch = "x86"))]
    std::process::exit(aoskit::run());
    #[cfg(not(all(windows, target_arch = "x86")))]
    {
        eprintln!("appleid-anisette works only as a 32-bit Windows program: build it with --target i686-pc-windows-msvc");
        std::process::exit(1);
    }
}
