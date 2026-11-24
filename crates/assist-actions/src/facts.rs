//! Fact sources for this device (PLAN.md section 3.10): `node.battery` (percent) and
//! `node.charging` (plugged in). A device without a battery reports neither.

use assist_core::facts::Value;

#[cfg(target_os = "linux")]
#[path = "facts_linux.rs"]
mod os;
#[cfg(windows)]
#[path = "facts_windows.rs"]
mod os;

/// The battery facts, sampled now. Blocking (it reads OS state); call off the async runtime.
///
/// shortcut: `app.focused` and `user.idle` need Win32 calls that are `unsafe`, which the
/// workspace forbids, and X11 on Linux; add them with an audited exception or a safe crate.
pub fn sample() -> Vec<(&'static str, Value)> {
    #[cfg(any(target_os = "linux", windows))]
    {
        os::sample()
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Vec::new()
    }
}

/// The two battery facts from a charge in percent and whether power is connected.
#[cfg_attr(not(any(target_os = "linux", windows)), allow(dead_code))]
fn battery(percent: f64, plugged: bool) -> Vec<(&'static str, Value)> {
    vec![
        ("node.battery", Value::Number(percent.clamp(0.0, 100.0))),
        ("node.charging", Value::Bool(plugged)),
    ]
}
