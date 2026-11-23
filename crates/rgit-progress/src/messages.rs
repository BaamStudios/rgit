// SPDX-License-Identifier: MIT
//! The texts of the progress meter (observed from git 2.56.1).

/// Appended to the last line of a finished meter.
pub const DONE: &str = ", done.";

/// A byte count as git shows it: `512 bytes`, `1.50 KiB`, `3.00 MiB`,
/// `1.00 GiB` (two decimals, truncated).
pub fn bytes(n: u64) -> String {
    humanise(n, "")
}

/// A rate in bytes per second, like [`bytes`] with `/s`.
pub fn rate(n: u64) -> String {
    humanise(n, "/s")
}

// A value is shown in a unit only when it is larger than one of it
// (exactly 1 MiB is `1024.00 KiB`).
fn humanise(n: u64, suffix: &str) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    let scaled = |unit: u64, name: &str| {
        let whole = n / unit;
        let frac = (n % unit) * 100 / unit;
        format!("{whole}.{frac:02} {name}{suffix}")
    };
    if n > GIB {
        scaled(GIB, "GiB")
    } else if n > MIB {
        scaled(MIB, "MiB")
    } else if n > KIB {
        scaled(KIB, "KiB")
    } else if n == 1 {
        format!("1 byte{suffix}")
    } else {
        format!("{n} bytes{suffix}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(bytes(1228800), "1.17 MiB");
        assert_eq!(bytes(1024000), "1000.00 KiB");
        assert_eq!(rate(1024 * 1537), "1.50 MiB/s");
        assert_eq!(bytes(12), "12 bytes");
    }
}
