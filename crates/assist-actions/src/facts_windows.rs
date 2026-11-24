//! Battery facts from WinRT `PowerManager`.

use assist_core::facts::Value;
use windows::System::Power::{BatteryStatus, PowerManager, PowerSupplyStatus};

pub fn sample() -> Vec<(&'static str, Value)> {
    let present = PowerManager::BatteryStatus().is_ok_and(|s| s != BatteryStatus::NotPresent);
    let percent = PowerManager::RemainingChargePercent();
    match (present, percent) {
        (true, Ok(percent)) => {
            let plugged =
                PowerManager::PowerSupplyStatus().is_ok_and(|s| s != PowerSupplyStatus::NotPresent);
            super::battery(f64::from(percent), plugged)
        }
        _ => Vec::new(),
    }
}
