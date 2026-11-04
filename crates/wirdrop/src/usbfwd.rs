//! USB 어댑터의 사용자 모드 드라이버 usbfwd를 자식 프로세스로 돌립니다.
//!
//! usbfwd(저장소의 crates/radio: WinUSB 위의 사용자 모드 RTL8822BU/RTL8812BU와 RTL8912AU 드라이버
//! wirdrop-radio)는 어댑터를 올리고, 우리 AWDL 주소의 ACK와 Block Ack을 하드웨어로 보내고, 상대의
//! A-MPDU를 재정렬하고, 우리 TCP ACK의 간격을 조절하고, 프레임을 루프백 UDP로 넘깁니다
//! ([crate::air] 참고). 올리기(펌웨어, 표, 보정)는 1초쯤 걸립니다.
use std::io::{self, BufRead, BufReader, Read};
use std::os::windows::io::AsRawHandle;
use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// usbfwd는 무선이 올라와 채널에 있으면 이 줄을 남깁니다.
const READY: &str = "usbfwd: ch ";

/// usbfwd를 돌리는 방법. 기본값은 TP-Link Archer T3U Nano에서 재서 가장 좋았던 것이고, usbfwd는
/// RTL8912AU(ipTIME BE6500UA)에도 같은 VHT 속도를 씁니다 (송신 출력 지수는 T3U에만 쓰고, RTL8912AU는
/// Windows의 국가 설정에 맞는 규제 한도를 따릅니다).
#[derive(Debug, Clone)]
pub struct UsbfwdOptions {
    /// usbfwd.exe.
    pub exe: PathBuf,
    /// 채널 폭(MHz): 20, 40, 80.
    pub bandwidth_mhz: u16,
    /// 유니캐스트 데이터 속도, VHT 스트림/MCS[/짧은 GI[/LDPC]] ("2/7/1/1"). -50 dBm보다 약한
    /// 상대에게는 usbfwd가 2/5로 낮춥니다. None이면 owly가 요청한 속도로 보냅니다.
    pub data_vht: Option<String>,
    /// 순수 TCP ACK용 VHT 속도 ("2/5").
    pub ack_vht: Option<String>,
    /// 송신 출력 지수 (0-63). 52가 가장 좋았고, 63은 링크가 끊깁니다.
    pub tx_power: Option<u8>,
    /// 수신 재정렬 구멍 시간 제한(ms) (usbfwd 기본값 80).
    pub reorder_ms: Option<u32>,
    /// TCP ACK 간격 조절: 이만큼 조용하면 잡아 둔 ACK를 보냅니다 (µs, 기본 200) ...
    pub ack_quiet_us: Option<u32>,
    /// ... 또는 이만큼 잡아 두었으면 (µs, 기본 2000).
    pub ack_hold_us: Option<u32>,
    /// 송신 A-MPDU, "tid/max[/density]".
    pub ampdu: Option<String>,
    /// USB 제조사와 제품 ID (usbfwd 기본값 2357:012E).
    pub usb_id: Option<(u16, u16)>,
    /// 루프백 UDP 포트: usbfwd의 것과 우리 것.
    pub radio_port: u16,
    pub host_port: u16,
    pub ready_timeout: Duration,
    /// usbfwd에 더 넘길 환경 변수.
    pub env: Vec<(String, String)>,
}

impl UsbfwdOptions {
    pub fn new(exe: impl Into<PathBuf>) -> Self {
        UsbfwdOptions {
            exe: exe.into(),
            bandwidth_mhz: 80,
            // 1 m 거리의 iPhone으로 잰 값: 수락 뒤 79.7 MB에 3.1~3.3초.
            data_vht: Some("2/7/1/1".into()),
            ack_vht: Some("2/5".into()),
            tx_power: Some(52),
            reorder_ms: None,
            ack_quiet_us: None,
            ack_hold_us: None,
            // 32: iPhone은 64까지 받겠다고 하지만, RTL8912AU에서 64로 묶으면 TCP 손실이 다시 늘어
            // 1.26 GB가 50.2 MB/s에서 24.0 MB/s로 떨어졌습니다.
            ampdu: Some("0/32/6".into()),
            usb_id: None,
            radio_port: 7770,
            host_port: 7771,
            ready_timeout: Duration::from_secs(30),
            env: Vec::new(),
        }
    }
}

/// 돌고 있는 usbfwd. 버리면 멈춥니다.
pub struct Usbfwd {
    child: Child,
    stop_file: PathBuf,
    /// 마지막에 선언해, Drop이 usbfwd에 멈추라고 한 뒤에 닫힙니다.
    _job: Option<KillOnClose>,
}

impl Usbfwd {
    /// usbfwd를 띄우고 무선이 채널에 올라올 때까지 기다립니다.
    pub fn start(opts: &UsbfwdOptions, mac: [u8; 6], channel: u8) -> io::Result<Usbfwd> {
        if !opts.exe.is_file() {
            return Err(io::Error::new(io::ErrorKind::NotFound, format!("{} not found", opts.exe.display())));
        }
        let stop_file = std::env::temp_dir().join(format!("wirdrop-usbfwd-{}.stop", std::process::id()));
        let _ = std::fs::remove_file(&stop_file);
        let mac_s = mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":");
        let mut cmd = Command::new(&opts.exe);
        cmd.args(["--mac", &mac_s, "--port", &opts.radio_port.to_string()])
            .args(["--peer", &format!("127.0.0.1:{}", opts.host_port)])
            .args(["--channel", &channel.to_string(), "--bw", &opts.bandwidth_mhz.to_string()])
            .arg("--stop-file")
            .arg(&stop_file)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW);
        let mut set = |k: &str, v: Option<String>| {
            if let Some(v) = v {
                cmd.env(k, v);
            }
        };
        set("USBFWD_DATA_VHT", opts.data_vht.clone());
        set("USBFWD_ACK_VHT", opts.ack_vht.clone());
        set("USBFWD_TX_POWER", opts.tx_power.map(|p| p.to_string()));
        set("USBFWD_REORDER_MS", opts.reorder_ms.map(|v| v.to_string()));
        set("USBFWD_ACK_QUIET_US", opts.ack_quiet_us.map(|v| v.to_string()));
        set("USBFWD_ACK_HOLD_US", opts.ack_hold_us.map(|v| v.to_string()));
        set("USBFWD_AMPDU", opts.ampdu.clone());
        set("USBFWD_VID", opts.usb_id.map(|(v, _)| format!("0x{v:04x}")));
        set("USBFWD_PID", opts.usb_id.map(|(_, p)| format!("0x{p:04x}")));
        for (k, v) in &opts.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn()?;
        let job = KillOnClose::new(&child)
            .map_err(|e| tracing::warn!("usbfwd may outlive us: no kill-on-close job ({e})"))
            .ok();
        let (ready_tx, ready_rx) = mpsc::channel();
        forward(child.stderr.take().unwrap(), "usbfwd", true, Some(ready_tx));
        forward(child.stdout.take().unwrap(), "usbfwd-out", false, None);
        let mut usbfwd = Usbfwd { child, stop_file, _job: job };
        let deadline = Instant::now() + opts.ready_timeout;
        loop {
            match ready_rx.recv_timeout(Duration::from_millis(200)) {
                Ok(line) => {
                    tracing::info!("{line}");
                    return Ok(usbfwd);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) | Err(mpsc::RecvTimeoutError::Timeout) if !usbfwd.is_running() => {
                    return Err(io::Error::other(
                        "usbfwd exited during bring-up; is the adapter plugged in and bound to WinUSB?",
                    ));
                }
                Err(_) if Instant::now() >= deadline => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "usbfwd did not bring the radio up in time"));
                }
                Err(_) => {}
            }
        }
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// usbfwd에 어댑터를 놓으라고 하고, 5초 안에 안 놓으면 죽입니다.
    pub fn stop(self) {}
}

impl Drop for Usbfwd {
    fn drop(&mut self) {
        if self.is_running() {
            let _ = std::fs::write(&self.stop_file, b"");
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.is_running() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(100));
            }
            if self.is_running() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
        let _ = std::fs::remove_file(&self.stop_file);
    }
}

/// 핸들이 닫히면 usbfwd를 끝내는 작업 개체: Usbfwd를 버릴 때, 또는 이 프로세스가 버리지 못하고 죽을
/// 때(강제 종료 등). 남겨진 usbfwd는 다음 실행이 어댑터를 못 쓰게 막습니다.
struct KillOnClose(windows_sys::Win32::Foundation::HANDLE);

// SAFETY: 작업 핸들은 어느 스레드에서든 쓰고 닫을 수 있습니다.
unsafe impl Send for KillOnClose {}

impl KillOnClose {
    fn new(child: &Child) -> io::Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JobObjectExtendedLimitInformation, SetInformationJobObject,
        };
        // SAFETY: 새 작업 개체를 설정하고 자식의 핸들을 줍니다. 실패하면 닫습니다.
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let set = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set == 0 || AssignProcessToJobObject(job, child.as_raw_handle()) == 0 {
                let e = io::Error::last_os_error();
                CloseHandle(job);
                return Err(e);
            }
            Ok(KillOnClose(job))
        }
    }
}

impl Drop for KillOnClose {
    fn drop(&mut self) {
        // SAFETY: 우리 핸들을 한 번 닫습니다.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

/// 자식의 줄을 로그에 남기고 (stderr는 info, stdout은 debug), 준비 줄을 ready에 넘깁니다.
fn forward(out: impl Read + Send + 'static, name: &'static str, info: bool, ready: Option<mpsc::Sender<String>>) {
    let _ = std::thread::Builder::new().name(name.into()).spawn(move || {
        let mut reader = BufReader::new(out);
        let mut buf = Vec::new();
        let mut ready = ready;
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return,
                Ok(_) => {}
            }
            let line = String::from_utf8_lossy(&buf).trim_end().to_string();
            if line.starts_with(READY)
                && let Some(tx) = ready.take()
            {
                let _ = tx.send(line);
                continue;
            }
            if info {
                tracing::info!(target: "wirdrop::usbfwd", "{name}: {line}");
            } else {
                tracing::debug!(target: "wirdrop::usbfwd", "{name}: {line}");
            }
        }
    });
}
