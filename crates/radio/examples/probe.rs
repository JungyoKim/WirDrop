//! 어댑터를 올리고 잠시 듣는 것을 셉니다.
//! cargo run -p wirdrop-radio --example probe -- [seconds] [mac]
use std::time::{Duration, Instant};

use wirdrop_radio::radio::{AWDL_BSSID, mac_str};
use wirdrop_radio::{Config, Radio};

fn main() -> std::io::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).with_target(false).init();
    let args: Vec<String> = std::env::args().collect();
    let secs: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(10);
    let mut cfg = Config {
        tx_power: Some(52),
        ..Config::default()
    };
    if let Some(m) = args.get(2) {
        let b: Vec<u8> = m.split(':').filter_map(|x| u8::from_str_radix(x, 16).ok()).collect();
        cfg.mac.copy_from_slice(&b[..6]);
    }
    let radio = Radio::open(cfg)?;
    let mac = radio.cfg.mac;
    let mut rx = radio.receiver(8)?;
    let end = Instant::now() + Duration::from_secs(secs);
    let (mut all, mut bad, mut awdl, mut to_us, mut transfers) = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut types = [0u64; 4];
    let mut rates = std::collections::BTreeMap::new();
    let mut shown = 0;
    let mut last = Instant::now();
    while Instant::now() < end {
        rx.next(|buf| {
            transfers += 1;
            Radio::parse(buf, |f| {
                all += 1;
                if f.crc_err {
                    bad += 1;
                    return;
                }
                let d = f.data;
                if d.len() < 10 {
                    return;
                }
                types[((d[0] >> 2) & 3) as usize] += 1;
                *rates.entry(f.rate).or_insert(0u64) += 1;
                if d.len() >= 22 && d[16..22] == AWDL_BSSID {
                    awdl += 1;
                    if shown < 5 {
                        shown += 1;
                        eprintln!(
                            "AWDL fc {:02x}{:02x} from {} len {} rate {} phy {:?}",
                            d[0],
                            d[1],
                            mac_str(&d[10..16]),
                            d.len(),
                            f.rate,
                            f.phy
                        );
                    }
                }
                if d[4..10] == mac {
                    to_us += 1;
                }
            });
        })?;
        if last.elapsed() > Duration::from_secs(2) {
            last = Instant::now();
            eprintln!(
                "rx {all} (bad {bad}) in {transfers} transfers | mgmt/ctrl/data {}/{}/{} | AWDL {awdl} | to us {to_us} | FA {}",
                types[0],
                types[1],
                types[2],
                radio.chip.false_alarms().unwrap_or(0)
            );
        }
    }
    eprintln!("rates: {rates:?}");
    drop(rx);
    radio.close();
    Ok(())
}
