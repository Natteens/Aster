//! Narrow runtime boundary for math-domain and integer-arithmetic failures.

use crate::{
    ExecutionContext,
    string::{AsterStrHeader, decode_str},
};

/// Target-independent `SplitMix64` output permutation. The standard library
/// owns state advancement; this narrow helper supplies the bit operations
/// that are intentionally absent from ASTER's current source surface.
#[must_use]
pub extern "C" fn aster_rt_random_mix(mut value: u64) -> u64 {
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

/// Reads 64 bits through `fill`, which stands for the operating system's random
/// source. Kept separate from the exported function so tests can inject a
/// provider, including one that fails. There is no fallback: a failure is
/// returned, never replaced by a clock, a counter, or a constant.
fn entropy_seed_with<E: std::fmt::Display>(
    fill: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> Result<u64, String> {
    let mut bytes = [0_u8; 8];
    fill(&mut bytes).map_err(|error| {
        format!("Random.FromEntropy could not read operating-system entropy: {error}")
    })?;
    Ok(u64::from_le_bytes(bytes))
}

/// Draws a seed from `fill` or records the first controlled runtime error in
/// `context`. The returned value is only meaningful when no error was recorded
/// (generated code stops at the recorded error), so the failure value `0` is
/// never observed as a seed.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
fn seed_or_fail<E: std::fmt::Display>(
    context: *mut ExecutionContext,
    fill: impl FnOnce(&mut [u8]) -> Result<(), E>,
) -> u64 {
    match entropy_seed_with(fill) {
        Ok(seed) => seed,
        Err(message) => {
            if !context.is_null() {
                // SAFETY: JIT-generated calls receive the live host-owned context as
                // their hidden first argument and cannot retain it after the call.
                #[allow(unsafe_code)]
                let context = unsafe { &mut *context };
                context.fail(message);
            }
            0
        }
    }
}

/// Fills `bytes` from the operating system's random source for this target:
/// `getrandom(2)` on Linux and `BCryptGenRandom` with the system-preferred RNG
/// on Windows. Both are the platform's documented random-byte APIs and both
/// report failure, which is returned. Other targets have no supported source
/// and fail the same way instead of inventing one.
fn operating_system_entropy(bytes: &mut [u8]) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_entropy(bytes)
    }
    #[cfg(target_os = "linux")]
    {
        linux_entropy(bytes)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = bytes;
        Err("this platform has no supported operating-system entropy source".to_owned())
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_entropy(bytes: &mut [u8]) -> Result<(), String> {
    use windows_sys::Win32::Security::Cryptography::{
        BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom,
    };
    let length =
        u32::try_from(bytes.len()).map_err(|_| "entropy request is too large".to_owned())?;
    // SAFETY: `bytes` is a live, writable buffer of exactly `length` bytes. A null
    // algorithm handle together with BCRYPT_USE_SYSTEM_PREFERRED_RNG selects the
    // system RNG, as documented for BCryptGenRandom.
    let status = unsafe {
        BCryptGenRandom(
            std::ptr::null_mut(),
            bytes.as_mut_ptr(),
            length,
            BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(format!(
            "BCryptGenRandom failed with NTSTATUS 0x{status:08X}"
        ))
    }
}

/// Fills `bytes` by calling `read` until nothing remains. `read` returns how many
/// bytes it wrote. A zero-byte success while bytes remain is an error, never a
/// retry, so a misbehaving source cannot make this loop forever; an interrupted
/// call is retried.
#[cfg(any(target_os = "linux", test))]
fn fill_completely(
    bytes: &mut [u8],
    mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
) -> Result<(), String> {
    let mut filled = 0;
    while filled < bytes.len() {
        match read(&mut bytes[filled..]) {
            Ok(0) => {
                return Err("getrandom returned no bytes while more were requested".to_owned());
            }
            Ok(count) => filled += count.min(bytes.len() - filled),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(format!("getrandom failed: {error}")),
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn linux_entropy(bytes: &mut [u8]) -> Result<(), String> {
    fill_completely(bytes, |remaining| {
        // SAFETY: `remaining` is a live, writable buffer of `remaining.len()` bytes.
        // Flag 0 waits until the kernel entropy pool is initialized, so the bytes are
        // never drawn from an unseeded pool.
        let read = unsafe { libc::getrandom(remaining.as_mut_ptr().cast(), remaining.len(), 0) };
        usize::try_from(read).map_err(|_| std::io::Error::last_os_error())
    })
}

/// A fresh 64-bit seed from the operating system's random source, for
/// `Random.FromEntropy()`. The read is fallible: if the OS source is
/// unavailable the first controlled runtime error is recorded in `context` and
/// no Rust panic crosses the C ABI. The seed is for a non-cryptographic
/// generator; the operating system, not `SplitMix64`, supplies the entropy.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_random_entropy_seed(context: *mut ExecutionContext) -> u64 {
    seed_or_fail(context, operating_system_entropy)
}

/// Record a structured equality assertion failure without exposing a Rust
/// panic or an ungoverned ASTER allocation path.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_assert_equal(
    context: *mut ExecutionContext,
    expected: *const AsterStrHeader,
    actual: *const AsterStrHeader,
) {
    if context.is_null() {
        return;
    }
    // SAFETY: generated code supplies live immutable ASTER strings for the
    // duration of this call. Invalid internal pointers degrade to a controlled
    // message instead of being dereferenced by the host.
    #[allow(unsafe_code)]
    let (expected, actual) = unsafe { (decode_str(expected), decode_str(actual)) };
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    match (expected, actual) {
        (Some(expected), Some(actual)) => context.fail(format!(
            "assertion failed: values are not equal\nexpected: {expected}\nactual:   {actual}"
        )),
        _ => context.fail("assertion failed: values are not equal"),
    }
}

/// Execute one fixed unary floating-point operation exposed by
/// `aster.math.Math`. Operation codes are compiler-private and invalid
/// adulterated values fail closed to NaN.
#[must_use]
pub extern "C" fn aster_rt_math_unary_float(value: f32, operation: i32) -> f32 {
    match operation {
        0 => value.sqrt(),
        1 => value.floor(),
        2 => value.ceil(),
        3 => value.round_ties_even(),
        4 => value.sin(),
        5 => value.cos(),
        6 => value.tan(),
        7 => value.trunc(),
        8 => value.exp(),
        9 => value.ln(),
        10 => value.log2(),
        11 => value.log10(),
        12 => value.asin(),
        13 => value.acos(),
        14 => value.atan(),
        15 => value.sinh(),
        16 => value.cosh(),
        17 => value.tanh(),
        _ => f32::NAN,
    }
}

/// `double` counterpart of [`aster_rt_math_unary_float`].
#[must_use]
pub extern "C" fn aster_rt_math_unary_double(value: f64, operation: i32) -> f64 {
    match operation {
        0 => value.sqrt(),
        1 => value.floor(),
        2 => value.ceil(),
        3 => value.round_ties_even(),
        4 => value.sin(),
        5 => value.cos(),
        6 => value.tan(),
        7 => value.trunc(),
        8 => value.exp(),
        9 => value.ln(),
        10 => value.log2(),
        11 => value.log10(),
        12 => value.asin(),
        13 => value.acos(),
        14 => value.atan(),
        15 => value.sinh(),
        16 => value.cosh(),
        17 => value.tanh(),
        _ => f64::NAN,
    }
}

#[must_use]
pub extern "C" fn aster_rt_math_pow_float(value: f32, exponent: f32) -> f32 {
    value.powf(exponent)
}

#[must_use]
pub extern "C" fn aster_rt_math_pow_double(value: f64, exponent: f64) -> f64 {
    value.powf(exponent)
}

/// IEEE `fmod` for `float` `%`: sign of the dividend, NaN for a zero divisor or
/// infinite dividend, the dividend for an infinite divisor.
#[must_use]
pub extern "C" fn aster_rt_fmod_f32(left: f32, right: f32) -> f32 {
    left % right
}

/// `double` counterpart of [`aster_rt_fmod_f32`].
#[must_use]
pub extern "C" fn aster_rt_fmod_f64(left: f64, right: f64) -> f64 {
    left % right
}

#[must_use]
pub extern "C" fn aster_rt_math_binary_float(left: f32, right: f32, operation: i32) -> f32 {
    match operation {
        0 => left.atan2(right),
        _ => f32::NAN,
    }
}

#[must_use]
pub extern "C" fn aster_rt_math_binary_double(left: f64, right: f64, operation: i32) -> f64 {
    match operation {
        0 => left.atan2(right),
        _ => f64::NAN,
    }
}

#[must_use]
pub extern "C" fn aster_rt_math_predicate_float(value: f32, operation: i32) -> i8 {
    i8::from(match operation {
        0 => value.is_nan(),
        1 => value.is_infinite(),
        2 => value.is_finite(),
        _ => false,
    })
}

#[must_use]
pub extern "C" fn aster_rt_math_predicate_double(value: f64, operation: i32) -> i8 {
    i8::from(match operation {
        0 => value.is_nan(),
        1 => value.is_infinite(),
        2 => value.is_finite(),
        _ => false,
    })
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_math_domain_error(context: *mut ExecutionContext, code: i32) {
    if context.is_null() {
        return;
    }
    // SAFETY: JIT-generated calls receive the live host-owned context as their
    // hidden first argument and cannot retain it after the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let message = match code {
        0 => "Math.Abs cannot represent the magnitude of the minimum int value",
        1 => "Math.Abs cannot represent the magnitude of the minimum long value",
        2 => "Math.Clamp requires min to be less than or equal to max",
        3 => "assertion failed: expected condition to be true",
        4 => "assertion failed: expected condition to be false",
        5 => "assertion failed: values are not equal",
        6 => "Math.Sign does not accept NaN",
        7 => "collection range is outside the valid bounds",
        8 => "random range requires minInclusive to be less than maxExclusive",
        9 => "an abstract method was called: the object's class does not implement it",
        10 => "an interface conversion found no method table in the object's class",
        _ => "unknown runtime error",
    };
    context.fail(message);
}

#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn aster_rt_integer_arithmetic_error(context: *mut ExecutionContext, code: i32) {
    if context.is_null() {
        return;
    }
    // SAFETY: JIT-generated calls receive the live host-owned context as their
    // hidden first argument and cannot retain it after the invocation.
    #[allow(unsafe_code)]
    let context = unsafe { &mut *context };
    let message = match code {
        0 => "integer division by zero",
        1 => "integer remainder by zero",
        2 => "signed integer division overflow",
        3 => "signed integer remainder overflow",
        _ => "unknown integer arithmetic error",
    };
    context.fail(message);
}

#[cfg(test)]
mod tests {
    use super::{
        aster_rt_integer_arithmetic_error, aster_rt_math_domain_error, aster_rt_math_pow_double,
        aster_rt_math_pow_float, aster_rt_math_unary_double, aster_rt_math_unary_float,
        aster_rt_random_entropy_seed, aster_rt_random_mix, entropy_seed_with, fill_completely,
        seed_or_fail,
    };

    #[test]
    fn entropy_seed_is_the_little_endian_value_of_the_provider_bytes() {
        let seed = entropy_seed_with(|bytes: &mut [u8]| -> Result<(), &str> {
            bytes.copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
            Ok(())
        })
        .expect("a succeeding provider");
        assert_eq!(seed, 0x0807_0605_0403_0201);
        // The provider is asked for exactly 64 bits.
        entropy_seed_with(|bytes: &mut [u8]| -> Result<(), &str> {
            assert_eq!(bytes.len(), 8);
            Ok(())
        })
        .expect("a succeeding provider");
    }

    #[test]
    fn entropy_failure_is_a_controlled_error_with_no_fallback_seed() {
        let mut context = ExecutionContext::new();
        let mut attempts = 0;
        let seed = seed_or_fail(&raw mut context, |_: &mut [u8]| -> Result<(), &str> {
            attempts += 1;
            Err("the OS source is unavailable")
        });
        // One attempt only: nothing is retried or replaced by a predictable value.
        assert_eq!(attempts, 1);
        assert_eq!(seed, 0);
        assert_eq!(
            context.take_error().as_deref(),
            Some(
                "Random.FromEntropy could not read operating-system entropy: the OS source is unavailable"
            )
        );
        // A failing provider cannot panic across the C ABI even without a context.
        let seed = seed_or_fail(std::ptr::null_mut(), |_: &mut [u8]| -> Result<(), &str> {
            Err("unavailable")
        });
        assert_eq!(seed, 0);
        // A succeeding provider records no error.
        let mut context = ExecutionContext::new();
        let seed = seed_or_fail(&raw mut context, |bytes: &mut [u8]| -> Result<(), &str> {
            bytes.fill(0xAB);
            Ok(())
        });
        assert_eq!(seed, 0xABAB_ABAB_ABAB_ABAB);
        assert_eq!(context.take_error(), None);
    }

    #[test]
    fn the_exported_function_reads_the_operating_system_and_reports_no_error() {
        let mut context = ExecutionContext::new();
        let mut drawn = std::collections::HashSet::new();
        for _ in 0..256 {
            drawn.insert(aster_rt_random_entropy_seed(&raw mut context));
        }
        // Fresh samples may collide in principle; a source stuck on one value is the bug.
        assert!(drawn.len() > 1, "the entropy source returned a constant");
        assert_eq!(context.take_error(), None);
    }

    #[test]
    fn a_zero_byte_success_is_an_error_not_a_loop() {
        let mut bytes = [0_u8; 8];
        let mut calls = 0;
        let result = fill_completely(&mut bytes, |_| {
            calls += 1;
            Ok(0)
        });
        assert!(result.expect_err("zero bytes").contains("no bytes"));
        assert_eq!(calls, 1);

        let mut calls = 0;
        let result = fill_completely(&mut bytes, |buffer| {
            calls += 1;
            if calls == 1 {
                buffer[..3].fill(1);
                Ok(3)
            } else {
                Ok(0)
            }
        });
        assert!(result.is_err());
        assert_eq!(calls, 2);
    }

    #[test]
    fn partial_reads_and_interruptions_are_completed() {
        let mut bytes = [0_u8; 8];
        let mut calls = 0;
        fill_completely(&mut bytes, |buffer| {
            calls += 1;
            match calls {
                1 => Err(std::io::ErrorKind::Interrupted.into()),
                2 => {
                    buffer[..5].fill(7);
                    Ok(5)
                }
                _ => {
                    buffer.fill(9);
                    Ok(buffer.len())
                }
            }
        })
        .expect("completes");
        assert_eq!(bytes, [7, 7, 7, 7, 7, 9, 9, 9]);
        let failed = fill_completely(&mut bytes, |_| Err(std::io::ErrorKind::Other.into()));
        assert!(failed.is_err());
    }

    #[test]
    fn splitmix64_mixer_matches_known_answers() {
        assert_eq!(
            aster_rt_random_mix(0x9e37_79b9_7f4a_7c15),
            16_294_208_416_658_607_535
        );
        assert_eq!(
            aster_rt_random_mix(11_400_714_819_323_198_608),
            13_032_462_758_197_477_675
        );
    }
    use crate::ExecutionContext;

    #[test]
    fn records_a_controlled_domain_error() {
        let mut context = ExecutionContext::new();
        aster_rt_math_domain_error(&raw mut context, 2);
        assert!(context.take_error().unwrap().contains("min"));
    }

    #[test]
    fn records_controlled_integer_arithmetic_errors() {
        for (code, expected) in [
            (0, "integer division by zero"),
            (1, "integer remainder by zero"),
            (2, "signed integer division overflow"),
            (3, "signed integer remainder overflow"),
        ] {
            let mut context = ExecutionContext::new();
            aster_rt_integer_arithmetic_error(&raw mut context, code);
            assert_eq!(context.take_error().as_deref(), Some(expected));
        }
    }

    #[test]
    #[allow(clippy::cast_possible_truncation, clippy::float_cmp)]
    fn floating_helpers_preserve_ieee_classification_and_ties_to_even() {
        assert_eq!(
            aster_rt_math_unary_double(0.0, 0).to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(
            aster_rt_math_unary_double(-0.0, 0).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(aster_rt_math_unary_double(f64::INFINITY, 0), f64::INFINITY);
        assert!(aster_rt_math_unary_double(-1.0, 0).is_nan());
        assert!(aster_rt_math_unary_double(f64::NAN, 0).is_nan());

        for (value, expected) in [
            (0.5, 0.0),
            (1.5, 2.0),
            (2.5, 2.0),
            (-0.5, -0.0),
            (-1.5, -2.0),
            (-2.5, -2.0),
        ] {
            let (value, expected): (f64, f64) = (value, expected);
            assert_eq!(
                aster_rt_math_unary_double(value, 3).to_bits(),
                expected.to_bits()
            );
            assert_eq!(
                aster_rt_math_unary_float(value as f32, 3).to_bits(),
                (expected as f32).to_bits()
            );
        }
        for operation in [1, 2, 3] {
            assert!(aster_rt_math_unary_double(f64::NAN, operation).is_nan());
            assert_eq!(
                aster_rt_math_unary_double(f64::INFINITY, operation),
                f64::INFINITY
            );
            assert_eq!(
                aster_rt_math_unary_double(f64::NEG_INFINITY, operation),
                f64::NEG_INFINITY
            );
            assert_eq!(
                aster_rt_math_unary_double(-0.0, operation).to_bits(),
                (-0.0_f64).to_bits()
            );
        }

        assert_eq!(aster_rt_math_pow_double(-2.0, 3.0), -8.0);
        assert!(aster_rt_math_pow_double(-2.0, 0.5).is_nan());
        assert!(aster_rt_math_pow_double(f64::NAN, 2.0).is_nan());
        assert_eq!(aster_rt_math_pow_double(2.0, -3.0), 0.125);
        assert_eq!(aster_rt_math_pow_double(f64::INFINITY, -1.0), 0.0);
        assert_eq!(aster_rt_math_pow_float(-2.0, 3.0), -8.0);

        assert_eq!(
            aster_rt_math_unary_double(0.0, 4).to_bits(),
            0.0_f64.to_bits()
        );
        assert_eq!(
            aster_rt_math_unary_double(-0.0, 6).to_bits(),
            (-0.0_f64).to_bits()
        );
        assert_eq!(aster_rt_math_unary_double(0.0, 5), 1.0);
        assert!(aster_rt_math_unary_double(f64::INFINITY, 4).is_nan());
        assert!(aster_rt_math_unary_double(f64::INFINITY, 5).is_nan());
        assert!(aster_rt_math_unary_double(f64::INFINITY, 6).is_nan());
        assert!((aster_rt_math_unary_double(std::f64::consts::FRAC_PI_2, 4) - 1.0).abs() < 1e-15);
    }

    #[test]
    fn fmod_follows_ieee_remainder_rules() {
        assert_eq!(
            super::aster_rt_fmod_f64(-7.5, 2.0).to_bits(),
            (-1.5_f64).to_bits()
        );
        assert!(super::aster_rt_fmod_f64(1.0, 0.0).is_nan());
        assert!(super::aster_rt_fmod_f64(f64::INFINITY, 2.0).is_nan());
        assert_eq!(
            super::aster_rt_fmod_f64(3.0, f64::INFINITY).to_bits(),
            3.0_f64.to_bits()
        );
        assert!(super::aster_rt_fmod_f64(-0.0, 5.0).is_sign_negative());
        assert_eq!(
            super::aster_rt_fmod_f32(7.5, -2.0).to_bits(),
            1.5_f32.to_bits()
        );
        assert!(super::aster_rt_fmod_f32(1.0, 0.0).is_nan());
    }
}
