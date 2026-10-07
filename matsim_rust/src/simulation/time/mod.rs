use std::fmt::{Display, Formatter};
use std::num::NonZeroU32;
use std::ops::{Add, AddAssign, Sub, SubAssign};
use std::time::Duration;

pub mod time_interpretation;

const NANOS_PER_SECOND: u128 = 1_000_000_000;
const NANOS_PER_SECOND_U64: u64 = 1_000_000_000;
const MAX_SIMTIME_NANOS: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct Tick(u64);

impl Tick {
    pub fn zero() -> Self {
        Self(0)
    }

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    pub fn is_multiple_of(self, rhs: u64) -> bool {
        self.0.is_multiple_of(rhs)
    }

    pub fn checked_add(self, rhs: Self) -> Option<Self> {
        self.0.checked_add(rhs.0).map(Self)
    }

    pub fn saturating_add(self, rhs: Self) -> Self {
        Self(self.0.saturating_add(rhs.0))
    }

    pub fn saturating_sub(self, rhs: Self) -> Self {
        Self(self.0.saturating_sub(rhs.0))
    }
}

impl Add for Tick {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        Self(self.0 + rhs.0)
    }
}

impl From<u64> for Tick {
    fn from(value: u64) -> Self {
        Self::new(value)
    }
}

impl From<u32> for Tick {
    fn from(value: u32) -> Self {
        Self::new(value as u64)
    }
}

impl From<i32> for Tick {
    fn from(value: i32) -> Self {
        assert!(value >= 0, "tick values must be non-negative");
        Self::new(value as u64)
    }
}

impl From<usize> for Tick {
    fn from(value: usize) -> Self {
        Self::new(value as u64)
    }
}

impl From<Tick> for u64 {
    fn from(value: Tick) -> Self {
        value.value()
    }
}

impl PartialEq<u32> for Tick {
    fn eq(&self, other: &u32) -> bool {
        self.value() == *other as u64
    }
}

impl PartialEq<Tick> for u32 {
    fn eq(&self, other: &Tick) -> bool {
        *self as u64 == other.value()
    }
}

impl AddAssign for Tick {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for Tick {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self::Output {
        Self(self.0 - rhs.0)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
/// `SimTime` is intentionally bounded to `u64` nanoseconds.
///
/// Even at nanosecond precision that still covers roughly 584 years of simulated time, which
/// is far beyond any scenario we expect to support while keeping the rest of the simulation on
/// a single integer time representation.
pub struct SimTime(Duration);

impl SimTime {
    pub fn max() -> Self {
        // note that this is smaller than Duration::MAX, since we allow at most u64::MAX nanoseconds
        Self::from_nanos(MAX_SIMTIME_NANOS)
    }

    pub fn from_duration(duration: Duration) -> Self {
        assert_duration_fits_simtime(duration);
        Self(duration)
    }

    pub fn from_millis(millis: u64) -> Self {
        Self::from_duration(Duration::from_millis(millis))
    }

    pub fn from_nanos(nanos: u64) -> Self {
        Self(Duration::from_nanos(nanos))
    }

    pub fn from_secs(seconds: u64) -> Self {
        Self::from_duration(Duration::from_secs(seconds))
    }

    pub fn as_nanos(self) -> u64 {
        self.0
            .as_nanos()
            .try_into()
            .expect("SimTime invariant violated: nanoseconds exceed u64::MAX")
    }

    pub fn as_millis(self) -> u64 {
        self.0
            .as_millis()
            .try_into()
            .expect("SimTime invariant violated: milliseconds exceed u64::MAX")
    }

    pub fn as_secs(self) -> u64 {
        self.0.as_secs()
    }

    pub fn as_duration(self) -> Duration {
        self.0
    }

    pub fn duration_since(self, earlier: SimTime) -> Duration {
        self.0.saturating_sub(earlier.0)
    }

    pub fn saturating_add(self, duration: impl Into<Duration>) -> Self {
        Self::from_duration(self.0.saturating_add(duration.into()))
    }

    pub fn saturating_sub(self, duration: impl Into<Duration>) -> Self {
        Self::from_duration(self.0.saturating_sub(duration.into()))
    }

    /// Parses a time string in the format "HH:MM:SS" or "HH:MM:SS.<up to 9 digits>" into a SimTime.
    pub fn parse(input: &str) -> Result<Self, String> {
        let mut parts = input.split(':');
        let hours = parts
            .next()
            .ok_or_else(|| "missing hours".to_string())?
            .parse::<u64>()
            .map_err(|_| "invalid hours".to_string())?;
        let minutes = parts
            .next()
            .ok_or_else(|| "missing minutes".to_string())?
            .parse::<u64>()
            .map_err(|_| "invalid minutes".to_string())?;
        let seconds_part = parts.next().ok_or_else(|| "missing seconds".to_string())?;
        if parts.next().is_some() {
            return Err("too many ':' separators".to_string());
        }
        let seconds_part_as_decimal = if seconds_part.contains('e') || seconds_part.contains('E') {
            // If the seconds part is in scientific notation, we need to convert it to a decimal string
            let seconds_as_f64 = seconds_part
                .parse::<f64>()
                .map_err(|_| "invalid seconds".to_string())?;
            format!("{:.9}", seconds_as_f64)
        } else {
            seconds_part.to_string()
        };

        let (seconds, nanos) = Self::parse_seconds_truncate_nanos(&seconds_part_as_decimal)?;

        let total_seconds = hours
            .checked_mul(60)
            .and_then(|total_minutes| total_minutes.checked_add(minutes))
            .and_then(|total_minutes| total_minutes.checked_mul(60))
            .and_then(|whole_seconds| whole_seconds.checked_add(seconds))
            .ok_or_else(|| simtime_overflow_error("time components"))?;
        let total_nanos = checked_total_nanos(total_seconds, nanos)?;
        Ok(Self::from_nanos(total_nanos))
    }

    pub fn parse_decimal_seconds(input: &str) -> Result<Self, String> {
        let (seconds, nanos) = Self::parse_seconds_truncate_nanos(input)?;

        Ok(Self::from_nanos(checked_total_nanos(seconds, nanos)?))
    }

    /// Parses a seconds string with optional fractional part; fractional digits beyond 9 are truncated.
    fn parse_seconds_truncate_nanos(input: &str) -> Result<(u64, u64), String> {
        let (seconds, nanos) = match input.split_once('.') {
            Some((seconds, nanos)) => {
                let seconds = seconds
                    .parse::<u64>()
                    .map_err(|_| "invalid seconds".to_string())?;
                let nanos = normalize_nanos_truncate(nanos)?;
                (seconds, nanos)
            }
            None => (
                input
                    .parse::<u64>()
                    .map_err(|_| "invalid seconds".to_string())?,
                0,
            ),
        };
        Ok((seconds, nanos))
    }

    pub fn format_decimal_seconds(self) -> String {
        let total_nanos = self.as_nanos();
        let seconds = total_nanos / NANOS_PER_SECOND_U64;
        let nanos = total_nanos % NANOS_PER_SECOND_U64;
        if nanos == 0 {
            seconds.to_string()
        } else {
            format!("{seconds}.{nanos:09}")
                .trim_end_matches('0')
                .to_string()
        }
    }

    pub fn format_hh_mm_ss_trimmed(self) -> String {
        let total_nanos = self.as_nanos();
        let total_seconds = total_nanos / NANOS_PER_SECOND_U64;
        let nanos = total_nanos % NANOS_PER_SECOND_U64;
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        if nanos == 0 {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        } else {
            format!("{hours:02}:{minutes:02}:{seconds:02}.{nanos:09}")
                .trim_end_matches('0')
                .to_string()
        }
    }
}

impl Display for SimTime {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.format_hh_mm_ss_trimmed())
    }
}

impl Into<Duration> for SimTime {
    fn into(self) -> Duration {
        self.as_duration()
    }
}

impl From<Duration> for SimTime {
    fn from(duration: Duration) -> Self {
        SimTime(duration)
    }
}

impl Add for SimTime {
    type Output = SimTime;
    fn add(self, rhs: Self) -> Self::Output {
        SimTime::from_duration(self.0 + rhs.0)
    }
}

impl AddAssign for SimTime {
    fn add_assign(&mut self, rhs: Self) {
        self.0 += rhs.0;
    }
}

impl Sub for SimTime {
    type Output = SimTime;
    fn sub(self, rhs: Self) -> Self::Output {
        SimTime::from_duration(self.0 - rhs.0)
    }
}

impl SubAssign for SimTime {
    fn sub_assign(&mut self, rhs: Self) {
        self.0 -= rhs.0;
    }
}

fn assert_duration_fits_simtime(duration: Duration) {
    assert!(
        duration.as_nanos() <= MAX_SIMTIME_NANOS as u128,
        "SimTime supports at most u64::MAX nanoseconds (about 584 years)"
    );
}

fn checked_total_nanos(seconds: u64, nanos: u64) -> Result<u64, String> {
    seconds
        .checked_mul(NANOS_PER_SECOND_U64)
        .and_then(|whole_nanos| whole_nanos.checked_add(nanos))
        .ok_or_else(|| simtime_overflow_error("nanoseconds"))
}

fn simtime_overflow_error(source: &str) -> String {
    format!(
        "SimTime overflow while converting {source}: values above u64::MAX nanoseconds are unsupported"
    )
}

fn normalize_nanos_truncate(input: &str) -> Result<u64, String> {
    if input.is_empty() || !input.chars().all(|c| c.is_ascii_digit()) {
        return Err("expected decimal digits for nanoseconds".to_string());
    }

    let mut nanos = input.chars().take(9).collect::<String>();
    while nanos.len() < 9 {
        nanos.push('0');
    }
    nanos
        .parse::<u64>()
        .map_err(|_| "invalid nanoseconds".to_string())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// Struct for transformation of internal simulation time (SimTime) to discrete ticks (Tick) and back, based on a specified number of ticks per second.
pub(crate) struct SimClock {
    ticks_per_second: NonZeroU32,
}

impl SimClock {
    pub(crate) fn new(ticks_per_second: u32) -> Self {
        Self {
            ticks_per_second: NonZeroU32::new(ticks_per_second)
                .expect("ticks_per_second must be > 0"),
        }
    }

    pub(crate) fn ticks_per_second(self) -> u32 {
        self.ticks_per_second.get()
    }

    pub(crate) fn tick_length(self) -> Duration {
        Duration::from_secs_f64(1.0 / self.ticks_per_second() as f64)
    }

    pub(crate) fn time_to_tick(self, time: SimTime) -> Tick {
        let ticks = div_ceil(
            u128::from(time.as_nanos()) * self.ticks_per_second() as u128,
            NANOS_PER_SECOND,
        );
        Tick(
            ticks
                .try_into()
                .expect("tick overflow while converting bounded SimTime"),
        )
    }

    pub(crate) fn tick_to_time(self, tick: Tick) -> SimTime {
        let nanos = tick.value() as u128 * NANOS_PER_SECOND / self.ticks_per_second() as u128;
        SimTime::from_nanos(
            nanos
                .try_into()
                .expect("simulation time overflow while converting tick to SimTime"),
        )
    }

    pub(crate) fn secs_to_tick(self, seconds: u64) -> Tick {
        self.time_to_tick(SimTime::from_secs(seconds))
    }

    pub(crate) fn tick_to_secs(self, tick: Tick) -> u64 {
        self.tick_to_time(tick).as_secs()
    }

    #[cfg(test)]
    pub(crate) fn time_to_secs(self, time: SimTime) -> u64 {
        time.as_secs()
    }

    #[allow(unused)]
    pub(crate) fn secs_to_ticks_ceil(self, seconds: f64) -> Tick {
        Tick((seconds * self.ticks_per_second() as f64).ceil() as u64)
    }

    pub(crate) fn secs_to_ticks_floor(self, seconds: f64) -> Tick {
        Tick((seconds * self.ticks_per_second() as f64).floor().max(0.0) as u64)
    }
}

fn div_ceil(left: u128, right: u128) -> u128 {
    left.div_ceil(right)
}

#[cfg(test)]
mod tests {
    use super::{NANOS_PER_SECOND_U64, SimClock, SimTime, Tick};
    use std::time::Duration;

    #[test]
    fn round_trip_with_one_tick_per_second() {
        let clock = SimClock::new(1);
        let tick = clock.secs_to_tick(42);
        assert_eq!(tick, Tick::new(42));
        assert_eq!(clock.tick_to_secs(tick), 42);
        assert_eq!(clock.tick_to_time(tick), SimTime::from_secs(42u64));
    }

    #[test]
    fn converts_subsecond_time_to_tick_and_back() {
        let clock = SimClock::new(10);
        let time = SimTime::from_duration(Duration::from_millis(350));
        let tick = clock.time_to_tick(time);
        assert_eq!(tick, Tick::new(4));
        assert_eq!(clock.tick_to_secs(tick), 0);
        assert_eq!(
            clock.tick_to_time(tick),
            SimTime::from_duration(Duration::from_millis(400))
        );
    }

    #[test]
    fn outward_seconds_are_truncated() {
        let clock = SimClock::new(10);
        let time = SimTime::from_duration(Duration::from_millis(1999));
        assert_eq!(clock.time_to_secs(time), 1);
    }

    #[test]
    fn travel_ticks_preserve_observed_link_duration() {
        let clock = SimClock::new(1);
        assert_eq!(clock.secs_to_ticks_floor(10_000.0 / 27.78), Tick::new(359));
        assert_eq!(clock.secs_to_ticks_floor(100.0), Tick::new(100));
        assert_eq!(clock.secs_to_ticks_floor(0.5), Tick::zero());

        let subsecond_clock = SimClock::new(10);
        assert_eq!(
            subsecond_clock.secs_to_ticks_floor(10_000.0 / 27.78),
            Tick::new(3599)
        );
        assert_eq!(subsecond_clock.secs_to_ticks_floor(0.3), Tick::new(3));
    }

    #[test]
    fn formats_decimal_seconds_without_fraction_for_full_seconds() {
        let time = SimTime::from_secs(42u64);
        assert_eq!(time.format_decimal_seconds(), "42");
    }

    #[test]
    fn formats_decimal_seconds_with_trimmed_nanos() {
        let nanos = SimTime::from_nanos(42_000_000_001);
        let precise = SimTime::from_nanos(42_123_456_789);
        let trailing_zero = SimTime::from_nanos(42_120_000_000);

        assert_eq!(nanos.format_decimal_seconds(), "42.000000001");
        assert_eq!(precise.format_decimal_seconds(), "42.123456789");
        assert_eq!(trailing_zero.format_decimal_seconds(), "42.12");
    }

    #[test]
    fn formats_hh_mm_ss_without_fraction_for_full_seconds() {
        let time = SimTime::from_secs(7 * 3600 + 30 * 60 + 15u64);
        assert_eq!(time.format_hh_mm_ss_trimmed(), "07:30:15");
    }

    #[test]
    fn formats_hh_mm_ss_with_trimmed_nanos() {
        let nanos = SimTime::from_nanos(
            (7 * 3600 + 30 * 60 + 15) as u64 * NANOS_PER_SECOND_U64 + 250_000_000,
        );
        let precise = SimTime::from_nanos(
            (7 * 3600 + 30 * 60 + 15) as u64 * NANOS_PER_SECOND_U64 + 123_456_789,
        );
        let trailing_zero = SimTime::from_nanos(
            (7 * 3600 + 30 * 60 + 15) as u64 * NANOS_PER_SECOND_U64 + 120_000_000,
        );

        assert_eq!(nanos.format_hh_mm_ss_trimmed(), "07:30:15.25");
        assert_eq!(precise.format_hh_mm_ss_trimmed(), "07:30:15.123456789");
        assert_eq!(trailing_zero.format_hh_mm_ss_trimmed(), "07:30:15.12");
    }

    #[test]
    fn parses_decimal_seconds_with_up_to_nine_digits() {
        assert_eq!(
            SimTime::parse_decimal_seconds("42.1").unwrap(),
            SimTime::from_nanos(42_100_000_000)
        );
        assert_eq!(
            SimTime::parse_decimal_seconds("42.123456789").unwrap(),
            SimTime::from_nanos(42_123_456_789)
        );
    }

    #[test]
    fn parses_and_truncates_decimal_seconds_to_nanos() {
        let parsed = SimTime::parse_decimal_seconds("42.1234567899").unwrap();
        assert_eq!(parsed, SimTime::from_nanos(42_123_456_789));
    }

    #[test]
    fn parses_hh_mm_ss_with_subseconds() {
        let parsed = SimTime::parse("07:30:15.250000001").unwrap();
        assert_eq!(
            parsed,
            SimTime::from_nanos(
                (7 * 3600 + 30 * 60 + 15) as u64 * NANOS_PER_SECOND_U64 + 250_000_001,
            )
        );
    }

    #[test]
    fn parses_max_decimal_seconds() {
        let parsed = SimTime::parse_decimal_seconds("18446744073.709551615").unwrap();
        assert_eq!(parsed, SimTime::from_nanos(u64::MAX));
        assert_eq!(parsed.format_decimal_seconds(), "18446744073.709551615");
    }

    #[test]
    fn parses_max_hh_mm_ss() {
        let parsed = SimTime::parse("5124095:34:33.709551615").unwrap();
        assert_eq!(parsed, SimTime::from_nanos(u64::MAX));
        assert_eq!(parsed.format_hh_mm_ss_trimmed(), "5124095:34:33.709551615");
    }

    #[test]
    fn rejects_decimal_seconds_beyond_u64_nanos() {
        let err = SimTime::parse_decimal_seconds("18446744073.709551616").unwrap_err();
        assert!(err.contains("u64::MAX nanoseconds"));
    }

    #[test]
    fn rejects_hh_mm_ss_beyond_u64_nanos() {
        let err = SimTime::parse("5124095:34:33.709551616").unwrap_err();
        assert!(err.contains("u64::MAX nanoseconds"));
    }

    #[test]
    #[should_panic(expected = "SimTime supports at most u64::MAX nanoseconds")]
    fn rejects_duration_larger_than_u64_nanos() {
        SimTime::from_duration(Duration::MAX);
    }

    #[test]
    #[should_panic(expected = "SimTime supports at most u64::MAX nanoseconds")]
    fn rejects_milliseconds_larger_than_u64_nanos() {
        SimTime::from_millis(u64::MAX);
    }
}
