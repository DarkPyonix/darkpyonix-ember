//! The size limit of an offer (SPEC `FR-F10`).

/// Default limit per offer: 4 GiB.
pub const DEFAULT_MAX_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Why an offer was refused by size.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LimitError {
    #[error("the offer is {total} bytes, above the limit of {max} bytes")]
    TooLarge { total: u64, max: u64 },
    #[error("the sizes of the offer do not fit in 64 bits")]
    Overflow,
}

/// The per-device limit. It applies to the sum of an offer (all files of a folder together).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    pub max_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_bytes: DEFAULT_MAX_BYTES }
    }
}

impl Limits {
    /// Checks a total. Exactly the limit is accepted. Runs before any byte moves.
    pub fn check_total(&self, total: u64) -> Result<(), LimitError> {
        if total > self.max_bytes {
            Err(LimitError::TooLarge { total, max: self.max_bytes })
        } else {
            Ok(())
        }
    }

    /// Sums `sizes` (an overflow is a refusal, never a panic), checks the sum and returns it.
    pub fn check_offer(&self, sizes: &[u64]) -> Result<u64, LimitError> {
        let mut total: u64 = 0;
        for s in sizes {
            total = total.checked_add(*s).ok_or(LimitError::Overflow)?;
        }
        self.check_total(total)?;
        Ok(total)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GIB: u64 = 1024 * 1024 * 1024;

    #[test]
    fn fr_f10_default_is_4_gib() {
        assert_eq!(Limits::default().max_bytes, 4 * GIB);
    }

    #[test]
    fn fr_f10_exactly_the_limit_is_accepted() {
        let l = Limits::default();
        assert_eq!(l.check_offer(&[4 * GIB]), Ok(4 * GIB));
        assert!(l.check_total(4 * GIB + 1).is_err());
    }

    #[test]
    fn fr_f10_the_sum_counts() {
        let err = Limits::default().check_offer(&[3 * GIB, 3 * GIB]).unwrap_err();
        assert_eq!(err, LimitError::TooLarge { total: 6 * GIB, max: 4 * GIB });
        assert!(err.to_string().contains(&(4 * GIB).to_string()));
    }

    #[test]
    fn fr_f10_overflow_is_a_refusal() {
        assert_eq!(Limits::default().check_offer(&[u64::MAX, 1]), Err(LimitError::Overflow));
    }

    #[test]
    fn fr_f10_changing_the_limit_changes_the_next_decision() {
        let mut l = Limits::default();
        assert!(l.check_total(5 * GIB).is_err());
        l.max_bytes = 8 * GIB;
        assert!(l.check_total(5 * GIB).is_ok());
    }
}
