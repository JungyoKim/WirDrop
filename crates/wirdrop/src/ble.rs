//! 받는 기기를 찾는 동안 주변 iPhone의 AWDL을 깨우는 AirDrop BLE 광고. "모든 사람" 모드의 폰은 어떤
//! 해시에도(0이어도) 깨고, "연락처만" 모드의 폰은 자기가 아는 Apple ID 신원의 해시에 깹니다. PC에
//! Apple ID가 여럿이면 하나마다 광고를 따로 해, 폰은 아는 것에 깹니다. 깬 폰도 mDNS로 자신을 알리지
//! 않을 수 있는데, 그런 폰은 [crate::airdrop::Discovery::knock_peers]가 찾습니다.
use std::io;
use std::time::Duration;

use owly_airdrop::proto::ble::{APPLE_COMPANY_ID, Advert};
use windows::Devices::Bluetooth::Advertisement::{
    BluetoothLEAdvertisementPublisher, BluetoothLEAdvertisementPublisherStatus, BluetoothLEManufacturerData,
};
use windows::Storage::Streams::DataWriter;

use crate::identity::Account;

/// 살아 있는 동안 광고합니다.
pub struct BleWake {
    publishers: Vec<BluetoothLEAdvertisementPublisher>,
}

impl BleWake {
    /// 새 무작위 태그와 설치된 Apple ID 신원의 해시로 광고를 시작합니다.
    pub fn start() -> io::Result<BleWake> {
        Self::start_for(&[Account::global()])
    }

    /// 계정마다 그 해시로 광고를 시작합니다 (신원이 없는 계정이나 계정이 없으면 해시 0). 첫 광고를
    /// 못 시작할 때만 실패합니다. 여럿은 Windows가 번갈아 보냅니다.
    pub fn start_for(accounts: &[Account]) -> io::Result<BleWake> {
        // WinRT에는 COM이 필요합니다: 어느 스레드가 시작했든 광고기를 위해 프로세스의 다중 스레드
        // 아파트를 살려 둡니다.
        static MTA: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        // SAFETY: 평범한 COM 호출. 쿠키는 일부러 놓지 않습니다.
        MTA.get_or_init(|| unsafe { windows::Win32::System::Com::CoIncrementMTAUsage() }.is_ok());
        let identities: Vec<_> = accounts.iter().map(Account::get).collect();
        let mut publishers = Vec::new();
        for identity in identities {
            let payload = Advert::of(crate::identity::random::<3>()?, identity.as_deref()).manufacturer_data();
            match publish(&payload) {
                Ok(p) => publishers.push(p),
                Err(e) if publishers.is_empty() => return Err(e),
                Err(e) => tracing::warn!("a second BLE advert did not start: {e}"),
            }
        }
        Ok(BleWake { publishers })
    }
}

fn publish(payload: &[u8]) -> io::Result<BluetoothLEAdvertisementPublisher> {
    let publisher = BluetoothLEAdvertisementPublisher::new().map_err(win)?;
    let data = BluetoothLEManufacturerData::new().map_err(win)?;
    data.SetCompanyId(APPLE_COMPANY_ID).map_err(win)?;
    let writer = DataWriter::new().map_err(win)?;
    writer.WriteBytes(payload).map_err(win)?;
    data.SetData(&writer.DetachBuffer().map_err(win)?).map_err(win)?;
    publisher
        .Advertisement()
        .map_err(win)?
        .ManufacturerData()
        .map_err(win)?
        .Append(&data)
        .map_err(win)?;
    publisher.Start().map_err(win)?;
    // 시작은 비동기이고, Bluetooth가 꺼져 있으면 중단됩니다.
    for _ in 0..50 {
        let status = publisher.Status().map_err(win)?;
        if status == BluetoothLEAdvertisementPublisherStatus::Started {
            tracing::info!("BLE AirDrop advert started: {}", hex(payload));
            return Ok(publisher);
        }
        if status == BluetoothLEAdvertisementPublisherStatus::Aborted {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let status = publisher.Status().map_err(win)?;
    let _ = publisher.Stop();
    Err(io::Error::other(format!("the BLE advert did not start ({status:?}); is Bluetooth on?")))
}

impl Drop for BleWake {
    fn drop(&mut self) {
        for p in &self.publishers {
            let _ = p.Stop();
        }
    }
}

fn win(e: windows::core::Error) -> io::Error {
    io::Error::other(e.to_string())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    /// Bluetooth 어댑터가 필요합니다: cargo test -- --ignored ble
    #[test]
    #[ignore]
    fn ble_advert_starts() {
        let wake = std::thread::spawn(super::BleWake::start).join().unwrap().unwrap();
        std::thread::sleep(std::time::Duration::from_secs(1));
        let status = wake.publishers[0].Status().unwrap();
        assert_eq!(status, super::BluetoothLEAdvertisementPublisherStatus::Started);
    }
}
