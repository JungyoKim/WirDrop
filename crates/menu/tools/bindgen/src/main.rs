//! Windows App SDK 2 메타데이터로 menu 크레이트의 src/bindings.rs를 다시 만듭니다.
//!
//! NuGet 패키지 세 개(.nupkg는 zip)에서 메타데이터를 받습니다: Microsoft.WindowsAppSDK.WinUI,
//! .InteractiveExperiences, .Foundation 2.0.x. 그다음 이 폴더에서
//!
//!     cargo run -- <folder the three packages are unzipped in>
//!
//! 각 패키지는 nuget.org처럼 소문자 이름의 폴더에 풉니다. filter.txt에는 메뉴가 부르는 멤버를 ABI
//! 이름(put_Text, add_Click)으로, 생성자는 Class::CreateInstance로 적어 그 밖의 것은 따라오지
//! 않습니다. --minimal은 기반 클래스 전달 함수를 빼므로 기반 인터페이스는 cast()로 부릅니다.
//! OnLaunched를 덮어쓰려고 Application은 합성(compose)합니다.
use std::path::Path;

fn main() {
    let root = std::env::args().nth(1).expect("the folder with the unzipped packages");
    let root = Path::new(&root);
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let winui = root.join("microsoft.windowsappsdk.winui").join("metadata");
    let ie = root.join("microsoft.windowsappsdk.interactiveexperiences").join("metadata").join("10.0.18362.0");
    let foundation = root.join("microsoft.windowsappsdk.foundation").join("metadata");
    let filter = std::fs::read_to_string(here.join("filter.txt")).expect("filter.txt");
    let mut args: Vec<String> = vec!["--in".into(), "default".into()];
    for path in [
        winui.join("Microsoft.UI.Xaml.winmd"),
        winui.join("Microsoft.UI.Text.winmd"),
        ie.join("Microsoft.UI.winmd"),
        ie.join("Microsoft.Foundation.winmd"),
        ie.join("Microsoft.Graphics.winmd"),
        foundation,
    ] {
        args.push(path.display().to_string());
    }
    args.extend(
        ["--flat", "--minimal", "--compose", "Microsoft.UI.Xaml.Application"].map(String::from),
    );
    args.extend(["--out".into(), here.join("../../src/bindings.rs").display().to_string()]);
    args.extend(
        ["--implement", "Microsoft.UI.Xaml.IApplicationOverrides", "Microsoft.UI.Xaml.Markup.IXamlMetadataProvider"]
            .map(String::from),
    );
    args.push("--filter".into());
    args.extend(filter.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from));
    windows_bindgen::bindgen(args);
}

