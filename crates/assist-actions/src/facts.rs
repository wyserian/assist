//! Fact sources for this device (PLAN.md section 3.10): `node.battery` (percent) and
//! `node.charging` (plugged in), and on Windows `app.focused` (the foreground app's file
//! name) and `user.idle` (seconds since the last input). A fact this device cannot tell,
//! such as the battery of a desktop, is left out.

use assist_core::facts::Value;

#[cfg(target_os = "linux")]
#[path = "facts_linux.rs"]
mod os;
#[cfg(windows)]
#[path = "facts_windows.rs"]
mod os;

/// This device's facts, sampled now. Blocking (it reads OS state); call off the async
/// runtime.
///
/// shortcut: on Linux `app.focused` and `user.idle` need X11 or Wayland libraries; add them
/// when Linux desktops use focus conditions.
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
