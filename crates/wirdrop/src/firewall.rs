//! 상대가 awdl0으로 이 프로그램에 닿게 하는 Windows 방화벽 규칙: AirDrop TCP 포트와 mDNS. awdl0은
//! 식별되지 않은 공용 네트워크라 들어오는 연결이 기본으로 막힙니다.
use std::io;
use std::os::windows::process::CommandExt;
use std::process::{Command, Stdio};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 지금 실행 파일을 위한 들어오기 허용 규칙. 버리면 지웁니다. 관리자 권한이 필요합니다.
pub struct FirewallRules {
    names: Vec<String>,
}

impl FirewallRules {
    pub fn allow_current_exe(tcp_port: u16) -> io::Result<FirewallRules> {
        Self::allow_current_exe_on(&[tcp_port])
    }

    /// [FirewallRules::allow_current_exe]와 같되 TCP 포트 여럿: Apple ID마다 받는 쪽 하나.
    pub fn allow_current_exe_on(tcp_ports: &[u16]) -> io::Result<FirewallRules> {
        let exe = std::env::current_exe()?;
        let stem = exe.file_stem().and_then(|s| s.to_str()).unwrap_or("app").replace(' ', "_");
        let tcp = tcp_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
        let wanted: Vec<(&str, String)> = [("TCP", tcp), ("UDP", "5353".to_string())]
            .into_iter()
            .filter(|(_, port)| !port.is_empty())
            .collect();
        // 규칙 하나가 0.2초쯤 걸리는 netsh 실행 두 번이라 규칙들을 나란히 만듭니다.
        let added: Vec<io::Result<String>> = std::thread::scope(|s| {
            let runs: Vec<_> = wanted
                .iter()
                .map(|(proto, port)| {
                    let (stem, exe) = (&stem, &exe);
                    s.spawn(move || {
                        let name = format!("wirdrop-{stem}-{proto}-{}", port.replace(',', "-"));
                        // 비정상 종료한 실행이 남긴 규칙.
                        let _ = netsh(&["delete", "rule", &format!("name={name}")]);
                        netsh(&[
                            "add",
                            "rule",
                            &format!("name={name}"),
                            "dir=in",
                            "action=allow",
                            &format!("program={}", exe.display()),
                            &format!("protocol={proto}"),
                            &format!("localport={port}"),
                            "profile=any",
                        ])
                        .map(|()| name)
                    })
                })
                .collect();
            runs.into_iter()
                .map(|r| r.join().unwrap_or_else(|_| Err(io::Error::other("netsh run panicked"))))
                .collect()
        });
        // 다른 규칙이 실패하면 추가한 규칙도 (Drop에서) 다시 지웁니다.
        let mut rules = FirewallRules { names: Vec::new() };
        let mut failed = None;
        for r in added {
            match r {
                Ok(name) => rules.names.push(name),
                Err(e) => failed = Some(e),
            }
        }
        match failed {
            Some(e) => Err(e),
            None => Ok(rules),
        }
    }
}

impl Drop for FirewallRules {
    fn drop(&mut self) {
        for name in &self.names {
            let _ = netsh(&["delete", "rule", &format!("name={name}")]);
        }
    }
}

fn netsh(args: &[&str]) -> io::Result<()> {
    let status = Command::new("netsh")
        .args(["advfirewall", "firewall"])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "netsh advfirewall firewall {} failed (administrator rights needed)",
            args[0]
        )))
    }
}
