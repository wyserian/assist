//! Battery facts from WinRT `PowerManager`; the focused app and idle time from Win32.

use assist_core::facts::Value;
use windows::System::Power::{BatteryStatus, PowerManager, PowerSupplyStatus};

#[path = "facts_win32.rs"]
mod win32;

pub fn sample() -> Vec<(&'static str, Value)> {
    let mut facts = battery();
    if let Some(app) = win32::focused_app() {
        facts.push(("app.focused", Value::Text(app)));
    }
    if let Some(idle) = win32::idle_seconds() {
        facts.push(("user.idle", Value::Number(idle as f64)));
    }
    facts
}

fn battery() -> Vec<(&'static str, Value)> {
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
