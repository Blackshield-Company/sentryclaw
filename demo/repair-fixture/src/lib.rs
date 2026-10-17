//! Tiny utility crate with planted defects, used as the target for the
//! `sentryclaw repair` demo. Each function has a test that pins down the
//! intended behaviour.

/// Sum of all integers from 1 to `n` inclusive.
pub fn sum_to(n: u64) -> u64 {
    (1..n).sum()
}

/// Gregorian leap year rule.
pub fn is_leap_year(year: u32) -> bool {
    year % 4 == 0 && year % 100 != 0
}

/// Arithmetic mean, or `None` for an empty slice.
pub fn average(values: &[f64]) -> Option<f64> {
    let total: f64 = values.iter().sum();
    Some(total / values.len() as f64)
}

/// Clamp `value` into the inclusive range `[lo, hi]`.
pub fn clamp(value: i64, lo: i64, hi: i64) -> i64 {
    if value < lo {
        lo
    } else if value > hi {
        hi
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sum_to_is_inclusive() {
        assert_eq!(sum_to(0), 0);
        assert_eq!(sum_to(1), 1);
        assert_eq!(sum_to(10), 55);
    }

    #[test]
    fn leap_years() {
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(1900));
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(2023));
    }

    #[test]
    fn average_of_values() {
        assert_eq!(average(&[1.0, 2.0, 3.0]), Some(2.0));
    }

    #[test]
    fn average_of_empty_is_none() {
        assert_eq!(average(&[]), None);
    }

    #[test]
    fn clamp_works() {
        assert_eq!(clamp(5, 0, 10), 5);
        assert_eq!(clamp(-1, 0, 10), 0);
        assert_eq!(clamp(11, 0, 10), 10);
    }
}
