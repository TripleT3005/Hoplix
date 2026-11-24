// Constants

const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];

// Sizes

pub fn size(bytes: u64, decimal_separator: &str) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes;
    let mut unit = 0;
    while value >= 1024 * 1024 && unit < UNITS.len() - 1 {
        value /= 1024;
        unit += 1;
    }
    let tenths = (u128::from(value) * 10 + 512) / 1024;
    let (whole, fraction) = (tenths / 10, tenths % 10);
    if whole >= 1024 && unit < UNITS.len() - 1 {
        return format!("1{decimal_separator}0 {}", UNITS[unit + 1]);
    }
    format!("{whole}{decimal_separator}{fraction} {}", UNITS[unit])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_use_binary_units() {
        assert_eq!(size(0, "."), "0 B");
        assert_eq!(size(1023, "."), "1023 B");
        assert_eq!(size(1024, "."), "1.0 KiB");
        assert_eq!(size(1536, ","), "1,5 KiB");
        assert_eq!(size(5 * 1024 * 1024, "."), "5.0 MiB");
        assert_eq!(size(1024 * 1024 - 1, "."), "1.0 MiB");
        assert_eq!(size(u64::MAX, "."), "16.0 EiB");
    }
}
