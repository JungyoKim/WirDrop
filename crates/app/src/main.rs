#![windows_subsystem = "windows"]
//! Windows 11용 WirDrop: 알림 영역, 탐색기 메뉴, 아일랜드로 iPhone과 AirDrop을 주고받습니다. 자체
//! 창은 없습니다.
//!
//! 실행 파일 하나가 여러 역할을 맡습니다.
//!
//! - 인자 없음 (또는 --autostart): 설정과 카드를 맡는 트레이 앱.
//! - --pick FILE...: 트레이 앱(또는 이미 도는 것)이 마우스 자리에 기기 메뉴를 열어 보낼 곳을 고르게
//!   합니다.
//! - --service: 엔진을 돌리는 Windows 서비스 (--update면 업데이트를 갈아 끼움).
//! - --engine: 서비스가 없을 때 UAC로 띄우는 관리자 권한 엔진 (USB 어댑터, awdl0, AirDrop).
//! - --settings: 트레이 앱의 메뉴를 엽니다 (필요하면 앱을 시작).
//! - --setup ...: 관리자 권한이 필요한 설치 작업 (서비스, 드라이버, 탐색기 메뉴).
//! - --exit: 도는 트레이 앱을 끝냅니다.
#[macro_use]
mod lang;

mod driver;
mod elevate;
mod engine;
mod ipc;
mod paths;
mod record;
mod service;
mod settings;
mod signin;
mod tray;
mod update;
mod win;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = args.first().map(String::as_str).unwrap_or_default();
    let result = match mode {
        "--engine" => engine::main(),
        "--service" => service::main(),
        "--setup" => elevate::setup_main(&args[1..]),
        "--settings" => tray::main(&args),
        "--exit" => {
            tray::forward(ipc::Msg::Exit);
            Ok(())
        }
        _ => tray::main(&args),
    };
    if let Err(e) = result {
        tracing::error!("{mode}: {e}");
        std::process::exit(1);
    }
}
