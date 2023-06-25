use chrono::{offset::Local, DateTime, Duration};
use matrix_sdk::ruma::MilliSecondsSinceUnixEpoch;
use std::time::SystemTime;

pub trait ToLocal {
    fn localtime(&self) -> Option<String>;
}
impl ToLocal for MilliSecondsSinceUnixEpoch {
    fn localtime(&self) -> Option<String> {
        let datetime: DateTime<Local> = self
            .to_system_time()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .into();
        // empty if within 10s, just hour/min/sec if < 12h from now, else full date
        let now = Local::now();
        if datetime < now - Duration::hours(12) {
            Some(datetime.format("%Y-%m-%d %H:%M:%S").to_string())
        } else if datetime < now - Duration::seconds(10) {
            Some(datetime.format("%H:%M:%S").to_string())
        } else if datetime < now + Duration::seconds(10) {
            None
        } else {
            // date in the future?!
            Some(datetime.format("%Y-%m-%d %H:%M:%S").to_string())
        }
    }
}
