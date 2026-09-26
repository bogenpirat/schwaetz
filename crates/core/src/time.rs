//! Wall-clock helpers: Unix milliseconds and local-time formatting (DST-aware on Windows).

use schwaetz_proto::tags::civil_from_days;
use std::cell::Cell;

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

thread_local! {
    /// (hour bucket, offset) — the UTC offset only changes on the hour, so cache per hour.
    static OFFSET_CACHE: Cell<(i64, i64)> = const { Cell::new((i64::MIN, 0)) };
}

/// Local UTC offset in milliseconds at `unix_ms`.
pub fn local_offset_ms(unix_ms: i64) -> i64 {
    let bucket = unix_ms.div_euclid(3_600_000);
    OFFSET_CACHE.with(|c| {
        let (b, off) = c.get();
        if b == bucket {
            return off;
        }
        let off = compute_offset(unix_ms);
        c.set((bucket, off));
        off
    })
}

#[cfg(windows)]
fn compute_offset(unix_ms: i64) -> i64 {
    use windows_sys::Win32::Foundation::{FILETIME, SYSTEMTIME};
    use windows_sys::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
    // FILETIME: 100ns ticks since 1601-01-01.
    let ticks = (unix_ms + 11_644_473_600_000) * 10_000;
    let ft = FILETIME { dwLowDateTime: ticks as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut utc: SYSTEMTIME = unsafe { std::mem::zeroed() };
    let mut local: SYSTEMTIME = unsafe { std::mem::zeroed() };
    // SAFETY: plain out-parameters.
    unsafe {
        if FileTimeToSystemTime(&ft, &mut utc) == 0
            || SystemTimeToTzSpecificLocalTime(std::ptr::null(), &utc, &mut local) == 0
        {
            return 0;
        }
    }
    let to_ms = |s: &SYSTEMTIME| {
        let days = days_from_civil(s.wYear as i64, s.wMonth as i64, s.wDay as i64);
        ((days * 24 + s.wHour as i64) * 60 + s.wMinute as i64) * 60_000 + s.wSecond as i64 * 1000
    };
    to_ms(&local) - to_ms(&utc)
}

#[cfg(not(windows))]
fn compute_offset(_unix_ms: i64) -> i64 {
    0
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    era * 146_097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719_468
}

/// Broken-down local time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalTime {
    pub year: i64,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    /// 0 = Monday
    pub weekday: u8,
}

pub fn local(unix_ms: i64) -> LocalTime {
    breakdown(unix_ms + local_offset_ms(unix_ms))
}

pub fn breakdown(ms: i64) -> LocalTime {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    LocalTime {
        year,
        month: month as u8,
        day: day as u8,
        hour: (rem / 3600) as u8,
        minute: ((rem / 60) % 60) as u8,
        second: (rem % 60) as u8,
        weekday: (days + 3).rem_euclid(7) as u8,
    }
}

/// Local day number, for day-separator lines.
pub fn local_day(unix_ms: i64) -> i64 {
    (unix_ms + local_offset_ms(unix_ms)).div_euclid(86_400_000)
}

const WEEKDAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// Formats with a small strftime subset: `%H %M %S %d %m %Y %y %A %B %%`.
pub fn format(fmt: &str, t: LocalTime) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(fmt.len() + 8);
    let mut chars = fmt.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        let _ = match chars.next() {
            Some('H') => write!(out, "{:02}", t.hour),
            Some('M') => write!(out, "{:02}", t.minute),
            Some('S') => write!(out, "{:02}", t.second),
            Some('d') => write!(out, "{:02}", t.day),
            Some('m') => write!(out, "{:02}", t.month),
            Some('Y') => write!(out, "{}", t.year),
            Some('y') => write!(out, "{:02}", t.year % 100),
            Some('A') => write!(out, "{}", WEEKDAYS[t.weekday as usize % 7]),
            Some('B') => write!(out, "{}", MONTHS[(t.month as usize).saturating_sub(1) % 12]),
            Some('%') => write!(out, "%"),
            Some(other) => write!(out, "%{other}"),
            None => write!(out, "%"),
        };
    }
    out
}

/// Human duration: `3d 4h`, `5m 10s`.
pub fn duration(secs: u64) -> String {
    let (d, h, m, s) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
    match (d, h, m) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, _) => format!("{m}m {s}s"),
        (0, _, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting() {
        let t = breakdown(1_700_000_000_000); // 2023-11-14 22:13:20 UTC, a Tuesday
        assert_eq!(format("%Y-%m-%d %H:%M:%S %A", t), "2023-11-14 22:13:20 Tuesday");
        assert_eq!(duration(3725), "1h 2m");
        assert_eq!(duration(59), "59s");
    }

    #[test]
    fn offset_is_sane() {
        let off = local_offset_ms(now_ms());
        assert!(off.abs() <= 14 * 3_600_000);
        assert_eq!(off % 60_000, 0);
    }
}
