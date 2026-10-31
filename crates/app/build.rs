use std::path::Path;

fn main() {
    // 아이콘(1: WirDrop, 2: 쉬는 중, 3: 폰), 애플리케이션 매니페스트(res/app.rc, 파일 경로를 절대
    // 경로로), 그리고 파일 속성과 Windows 앱 목록에 보일 버전(Cargo의 것).
    let res = Path::new(&std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("res");
    let dir = res.display().to_string().replace('\\', "\\\\");
    let rc = std::fs::read_to_string(res.join("app.rc"))
        .unwrap()
        .replace(" \"", &format!(" \"{dir}\\\\"));
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let numbers: Vec<u32> = version.split(['.', '-']).take(3).map(|n| n.parse().unwrap_or(0)).collect();
    let (a, b, c) = (numbers[0], numbers.get(1).copied().unwrap_or(0), numbers.get(2).copied().unwrap_or(0));
    let rc = format!(
        "{rc}
1 VERSIONINFO
FILEVERSION {a},{b},{c},0
PRODUCTVERSION {a},{b},{c},0
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK \"StringFileInfo\"
  BEGIN
    BLOCK \"040904b0\"
    BEGIN
      VALUE \"CompanyName\", \"WirDrop\"
      VALUE \"FileDescription\", \"WirDrop\"
      VALUE \"FileVersion\", \"{version}\"
      VALUE \"InternalName\", \"WirDrop\"
      VALUE \"LegalCopyright\", \"MIT License\"
      VALUE \"OriginalFilename\", \"WirDrop.exe\"
      VALUE \"ProductName\", \"WirDrop\"
      VALUE \"ProductVersion\", \"{version}\"
    END
  END
  BLOCK \"VarFileInfo\"
  BEGIN
    VALUE \"Translation\", 0x409, 1200
  END
END
"
    );
    let out = Path::new(&std::env::var("OUT_DIR").unwrap()).join("app.rc");
    std::fs::write(&out, rc).unwrap();
    embed_resource::compile(&out, embed_resource::NONE).manifest_required().unwrap();
    for f in [
        "res/app.rc",
        "res/app.manifest",
        "res/wirdrop.ico",
        "res/wirdrop-off.ico",
        "res/phone.ico",
    ] {
        println!("cargo:rerun-if-changed={f}");
    }
}
