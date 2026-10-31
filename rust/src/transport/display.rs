use serde_json::Value;
use std::time::Duration;

pub(super) fn duration(duration: Duration) -> String {
    let nanos = duration.as_nanos();
    if nanos == 0 {
        return "0s".into();
    }
    if nanos < 1_000_000_000 {
        let (unit, divisor) = if nanos < 1000 {
            ("ns", 1)
        } else if nanos < 1_000_000 {
            ("µs", 1000)
        } else {
            ("ms", 1_000_000)
        };
        return format!("{}{unit}", decimal(nanos, divisor));
    }
    let hours = nanos / 3_600_000_000_000;
    let minutes = nanos / 60_000_000_000 % 60;
    let seconds = decimal(nanos % 60_000_000_000, 1_000_000_000);
    let mut text = String::new();
    if hours > 0 {
        text.push_str(&format!("{hours}h"));
    }
    if hours > 0 || minutes > 0 {
        text.push_str(&format!("{minutes}m"));
    }
    text.push_str(&format!("{seconds}s"));
    text
}
fn decimal(value: u128, divisor: u128) -> String {
    if value % divisor == 0 {
        return (value / divisor).to_string();
    }
    let places = divisor.ilog10() as usize;
    format!("{}.{:0places$}", value / divisor, value % divisor)
        .trim_end_matches('0')
        .to_owned()
}
pub(super) fn scheduled(
    config: &Value,
    region: Option<&crate::terminal::RegionHandle>,
    attempt: u32,
    delay: Duration,
    reason: &str,
) {
    if config["_display"] == true {
        let milliseconds = ((delay.as_nanos() + 500_000) / 1_000_000) as i64;
        crate::render::retry_scheduled(region, i64::from(attempt), milliseconds, reason);
    }
}
pub(super) fn started(
    config: &Value,
    region: Option<&crate::terminal::RegionHandle>,
    attempt: u32,
) {
    if config["_display"] == true {
        crate::render::retry_started(region, i64::from(attempt));
    }
}
