//! 하나뿐인 HTTP 클라이언트: Windows 자체의 TLS에, Apple 로그인·인증서 서버가 이어지는 Apple 루트도
//! 함께 신뢰합니다.
use std::sync::OnceLock;

/// Apple Root CA (SHA-256 B0B1730E…1001F024).
const APPLE_ROOT_CA: &[u8] = include_bytes!("../../../vendor/owly-airdrop/src/proto/apple_root_ca.der");

pub fn client() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .add_root_certificate(reqwest::Certificate::from_der(APPLE_ROOT_CA).expect("Apple's root certificate"))
                .http1_title_case_headers()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .expect("an HTTP client")
        })
        .clone()
}
