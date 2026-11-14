//! What an intent call returned, classified without trusting the value (ROADMAP P1 module 7).
//!
//! rustedscreep-wasm reads each `Creep` method's return value as an `f64` (a JS `undefined` arrives
//! as NaN, VERIFICATION_LOG V100, V112), not through screeps-game-api's typed methods: their
//! `result_from_i8` ends in `unwrap()` (V111; without the `unsafe-return-conversion` feature, which
//! this build does not enable, V87), so a code the crate does not list for that method would panic
//! and trap. Here any value that is not 0 or a known `ErrorCode` is [`Outcome::Unknown`].

use rustedscreep_consts::ErrorCode;

/// Every `ErrorCode` the API defines (screeps-game-api 0.23.5 `constants::small_enums`).
pub const ERROR_CODES: [ErrorCode; 15] = [
    ErrorCode::NotOwner,
    ErrorCode::NoPath,
    ErrorCode::NameExists,
    ErrorCode::Busy,
    ErrorCode::NotFound,
    ErrorCode::NotEnough,
    ErrorCode::InvalidTarget,
    ErrorCode::Full,
    ErrorCode::NotInRange,
    ErrorCode::InvalidArgs,
    ErrorCode::Tired,
    ErrorCode::NoBodypart,
    ErrorCode::RclNotEnough,
    ErrorCode::GclNotEnough,
    ErrorCode::AccessDenied,
];

/// A short name for an error code, for summaries.
pub const fn error_name(code: ErrorCode) -> &'static str {
    // Exhaustive: a code added by a crate bump fails to compile here.
    match code {
        ErrorCode::NotOwner => "not owner",
        ErrorCode::NoPath => "no path",
        ErrorCode::NameExists => "name exists",
        ErrorCode::Busy => "busy",
        ErrorCode::NotFound => "not found",
        ErrorCode::NotEnough => "not enough",
        ErrorCode::InvalidTarget => "invalid target",
        ErrorCode::Full => "full",
        ErrorCode::NotInRange => "not in range",
        ErrorCode::InvalidArgs => "invalid args",
        ErrorCode::Tired => "tired",
        ErrorCode::NoBodypart => "no bodypart",
        ErrorCode::RclNotEnough => "rcl not enough",
        ErrorCode::GclNotEnough => "gcl not enough",
        ErrorCode::AccessDenied => "access denied",
    }
}

/// The slot of an error code in [`ERROR_CODES`] (and in per-code counters).
pub fn error_index(code: ErrorCode) -> usize {
    ERROR_CODES
        .iter()
        .position(|&c| c == code)
        .unwrap_or(ERROR_CODES.len())
}

/// The result of one intent call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `OK`: the intent was recorded, and is billed unless the method is free (V34).
    Ok,
    /// A known error code: the game-side API refused, recorded nothing and billed nothing (V34).
    Failed(ErrorCode),
    /// Anything else (undefined, NaN, a fraction, an unknown code). Whether it was recorded is not
    /// known.
    Unknown,
    /// The call threw a JS exception, which the binding caught.
    Threw,
}

impl Outcome {
    /// Whether the call may have recorded an intent, so that it bills and blocks (V34, V45). Every
    /// game-side method records only on its OK path (V108), and its throw paths run before that, so
    /// a refusal or a throw recorded nothing. An unknown return counts as recorded: issuing an
    /// action it might block would risk a billed intent that does nothing.
    pub const fn recorded(self) -> bool {
        matches!(self, Self::Ok | Self::Unknown)
    }

    /// Classifies a return value read as `f64`.
    pub fn from_return(value: f64) -> Self {
        if value == 0.0 {
            return Self::Ok;
        }
        let fits = value.fract() == 0.0 && value >= f64::from(i8::MIN) && value < 0.0;
        if !fits {
            return Self::Unknown;
        }
        let code = value as i8;
        ERROR_CODES
            .iter()
            .copied()
            .find(|&c| c as i8 == code)
            .map_or(Self::Unknown, Self::Failed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ok_and_every_known_code_classify() {
        assert_eq!(Outcome::from_return(0.0), Outcome::Ok);
        assert_eq!(Outcome::from_return(-0.0), Outcome::Ok);
        for code in ERROR_CODES {
            assert_eq!(
                Outcome::from_return(f64::from(code as i8)),
                Outcome::Failed(code)
            );
        }
    }

    #[test]
    fn anything_else_is_unknown() {
        // -13 is not a code; 1 is not a return value of an action method.
        for v in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1.0,
            -13.0,
            -17.0,
            -128.0,
            -129.0,
            -1.5,
            0.5,
            f64::from(i32::MIN),
        ] {
            assert_eq!(Outcome::from_return(v), Outcome::Unknown, "{v}");
        }
    }

    /// Each name is its variant's name in words, so two names cannot be swapped unnoticed.
    #[test]
    fn names_follow_the_variants() {
        for code in ERROR_CODES {
            let name: String = error_name(code).split(' ').collect();
            assert_eq!(name, format!("{code:?}").to_lowercase(), "{code:?}");
        }
    }

    #[test]
    fn only_ok_and_unknown_count_as_recorded() {
        assert!(Outcome::Ok.recorded() && Outcome::Unknown.recorded());
        assert!(!Outcome::Threw.recorded());
        for code in ERROR_CODES {
            assert!(!Outcome::Failed(code).recorded());
        }
    }

    #[test]
    fn codes_are_distinct_and_named() {
        for (i, a) in ERROR_CODES.into_iter().enumerate() {
            assert_eq!(error_index(a), i);
            assert!((a as i8) < 0);
            assert!(!error_name(a).is_empty());
            for b in ERROR_CODES.into_iter().skip(i + 1) {
                assert_ne!(a as i8, b as i8);
                assert_ne!(error_name(a), error_name(b));
            }
        }
    }
}
