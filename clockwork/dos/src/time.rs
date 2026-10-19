//! Calendar arithmetic for the emulated wall clock (virtual, never the host's).

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    pub fn new(year: u16, month: u8, day: u8, hour: u8, minute: u8, second: u8) -> Self {
        Self { year, month, day, hour, minute, second }
    }

    /// Seconds since 1970-01-01 00:00:00.
    pub fn to_epoch_seconds(self) -> i64 {
        let days = days_from_civil(i64::from(self.year), i64::from(self.month), i64::from(self.day));
        days * 86_400 + i64::from(self.hour) * 3600 + i64::from(self.minute) * 60 + i64::from(self.second)
    }

    pub fn from_epoch_seconds(seconds: i64) -> Self {
        let days = seconds.div_euclid(86_400);
        let rest = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        Self {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            hour: (rest / 3600) as u8,
            minute: (rest / 60 % 60) as u8,
            second: (rest % 60) as u8,
        }
    }

    /// 0 = Sunday.
    pub fn weekday(self) -> u8 {
        let days = days_from_civil(i64::from(self.year), i64::from(self.month), i64::from(self.day));
        (days + 4).rem_euclid(7) as u8
    }

    pub fn seconds_since_midnight(self) -> u32 {
        u32::from(self.hour) * 3600 + u32::from(self.minute) * 60 + u32::from(self.second)
    }
}

// Howard Hinnant's algorithms for the proleptic Gregorian calendar.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_weekdays() {
        let t = DateTime::new(1994, 1, 1, 8, 0, 0);
        assert_eq!(DateTime::from_epoch_seconds(t.to_epoch_seconds()), t);
        assert_eq!(t.weekday(), 6, "1994-01-01 was a Saturday");
        let leap = DateTime::new(1996, 2, 28, 23, 59, 59);
        assert_eq!(DateTime::from_epoch_seconds(leap.to_epoch_seconds() + 1), DateTime::new(1996, 2, 29, 0, 0, 0));
        assert_eq!(DateTime::new(1970, 1, 1, 0, 0, 0).to_epoch_seconds(), 0);
    }
}
