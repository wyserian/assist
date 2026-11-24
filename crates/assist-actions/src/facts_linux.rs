//! Battery facts from `/sys/class/power_supply`.

use std::fs;
use std::path::Path;

use assist_core::facts::Value;

const SUPPLIES: &str = "/sys/class/power_supply";

pub fn sample() -> Vec<(&'static str, Value)> {
    read(Path::new(SUPPLIES))
}

/// The first battery's charge, plugged in when a mains supply is online or the battery
/// says it charges or is full.
fn read(dir: &Path) -> Vec<(&'static str, Value)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let field = |path: &Path, name: &str| {
        fs::read_to_string(path.join(name))
            .map(|s| s.trim().to_owned())
            .unwrap_or_default()
    };
    let mut percent = None;
    let mut plugged = false;
    for entry in entries.flatten() {
        let path = entry.path();
        match field(&path, "type").as_str() {
            "Battery" if percent.is_none() => {
                percent = field(&path, "capacity").parse::<f64>().ok();
                let status = field(&path, "status");
                plugged |= status == "Charging" || status == "Full";
            }
            "Mains" => plugged |= field(&path, "online") == "1",
            _ => {}
        }
    }
    percent.map_or_else(Vec::new, |p| super::battery(p, plugged))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_battery_and_mains() {
        let dir = std::env::temp_dir().join(format!("assist-power-{}", std::process::id()));
        let write = |name: &str, files: &[(&str, &str)]| {
            let d = dir.join(name);
            fs::create_dir_all(&d).unwrap();
            for (file, text) in files {
                fs::write(d.join(file), text).unwrap();
            }
        };
        write(
            "BAT0",
            &[
                ("type", "Battery\n"),
                ("capacity", "42\n"),
                ("status", "Discharging\n"),
            ],
        );
        write("AC", &[("type", "Mains\n"), ("online", "1\n")]);
        let facts = read(&dir);
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            facts,
            [
                ("node.battery", Value::Number(42.0)),
                ("node.charging", Value::Bool(true))
            ]
        );
    }
}
