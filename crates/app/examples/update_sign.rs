//! WirDrop 업데이트의 릴리스 키 (src/update.rs 참고).
//!
//!     cargo run -p wirdrop-app --example update_sign -- keygen KEY
//!
//! 키를 만들어 파일 KEY에 16진수로 쓰고 (비밀로: GitHub Actions 비밀에 두고 저장소에는 절대 넣지
//! 않습니다), 공개 키 절반을 update.rs의 PUBLIC_KEY용으로 출력합니다.
//!
//!     cargo run -p wirdrop-app --example update_sign -- sign KEY ZIP VERSION URL
//!
//! 릴리스의 update.json을 출력합니다: 버전, zip을 받을 URL, zip의 SHA-256과 크기에 서명한 것. KEY는
//! 키 파일, 또는 WIRDROP_UPDATE_KEY 변수를 쓰려면 -.
use std::io::Read;

use ring::signature::{Ed25519KeyPair, KeyPair};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["keygen", path] => keygen(path),
        ["sign", key, zip, version, url] => sign(key, zip, version, url),
        _ => Err("usage: update_sign keygen KEY | sign KEY ZIP VERSION URL".into()),
    };
    if let Err(e) = result {
        eprintln!("update_sign: {e}");
        std::process::exit(1);
    }
}

fn keygen(path: &str) -> Result<(), String> {
    if std::path::Path::new(path).exists() {
        return Err(format!("{path} is there already"));
    }
    let pkcs8 = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new()).map_err(|e| e.to_string())?;
    let key = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).map_err(|e| e.to_string())?;
    std::fs::write(path, hex(pkcs8.as_ref())).map_err(|e| e.to_string())?;
    let public: Vec<String> = key.public_key().as_ref().iter().map(|b| format!("0x{b:02x}")).collect();
    println!("const PUBLIC_KEY: [u8; 32] = [{}];", public.join(", "));
    Ok(())
}

fn sign(key: &str, zip: &str, version: &str, url: &str) -> Result<(), String> {
    let text = if key == "-" {
        std::env::var("WIRDROP_UPDATE_KEY").map_err(|e| e.to_string())?
    } else {
        std::fs::read_to_string(key).map_err(|e| e.to_string())?
    };
    let pkcs8 = unhex(text.trim()).ok_or("the key isn't hex")?;
    let key = Ed25519KeyPair::from_pkcs8(&pkcs8).map_err(|e| e.to_string())?;
    let mut file = std::fs::File::open(zip).map_err(|e| e.to_string())?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let (mut size, mut buf) = (0u64, vec![0u8; 1 << 16]);
    loop {
        let n = file.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        digest.update(&buf[..n]);
        size += n as u64;
    }
    let sha256 = hex(digest.finish().as_ref());
    // update.rs의 Manifest::message와 같은 메시지.
    let message = format!("WirDrop update\n{version}\n{url}\n{sha256}\n{size}\n");
    let signature = hex(key.sign(message.as_bytes()).as_ref());
    let json = serde_json::json!({ "version": version, "url": url, "sha256": sha256, "size": size, "signature": signature });
    println!("{}", serde_json::to_string_pretty(&json).map_err(|e| e.to_string())?);
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}
