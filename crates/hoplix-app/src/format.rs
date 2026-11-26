// Constants

const UNITS: [&str; 6] = ["KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
const ELLIPSIS: char = '…';
const ROUTE_ARROW: &str = " → ";

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

// Shortening

pub fn shorten_end(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let kept: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{kept}{ELLIPSIS}")
}

fn shorten_start(text: &str, max: usize) -> String {
    let count = text.chars().count();
    if count <= max {
        return text.to_string();
    }
    let kept: String = text.chars().skip(count + 1 - max.max(1)).collect();
    format!("{ELLIPSIS}{kept}")
}

pub fn shorten_path(path: &str, max: usize) -> String {
    if path.chars().count() <= max {
        return path.to_string();
    }
    let Some(separator) = path.rfind(['/', '\\']) else {
        return shorten_start(path, max);
    };
    let tail = &path[separator..];
    let tail_count = tail.chars().count();
    if tail_count + 2 > max {
        return shorten_start(path, max);
    }
    let head: String = path.chars().take(max - 1 - tail_count).collect();
    format!("{head}{ELLIPSIS}{tail}")
}

pub fn route(source: &str, target: &str) -> String {
    format!("{source}{ROUTE_ARROW}{target}")
}

pub fn shorten_route(source: &str, target: &str, max: usize) -> String {
    let budget = max.saturating_sub(ROUTE_ARROW.chars().count());
    let source_count = source.chars().count();
    let target_count = target.chars().count();
    if source_count + target_count <= budget {
        return route(source, target);
    }
    let half = budget / 2;
    let (source_max, target_max) = if source_count <= half {
        (source_count, budget - source_count)
    } else if target_count <= budget - half {
        (budget - target_count, target_count)
    } else {
        (half, budget - half)
    };
    route(
        &shorten_path(source, source_max),
        &shorten_path(target, target_max),
    )
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

    #[test]
    fn long_text_is_cut_at_the_end() {
        assert_eq!(shorten_end("short", 10), "short");
        assert_eq!(shorten_end("a rather long message", 8), "a rathe…");
    }

    #[test]
    fn long_paths_keep_the_file_name() {
        let path = r"C:\Users\demo\Documents\big-upload.bin";
        assert_eq!(shorten_path(path, 60), path);
        let short = shorten_path(path, 25);
        assert_eq!(short, r"C:\Users\…\big-upload.bin");
        assert_eq!(short.chars().count(), 25);
        assert_eq!(shorten_path("/srv/big-upload.bin", 8), "…oad.bin");
        assert_eq!(shorten_path("big-upload.bin", 6), "…d.bin");
    }

    #[test]
    fn routes_share_the_room_between_both_sides() {
        let source = r"C:\Users\demo\Documents\Projects\big-upload.bin";
        let target = "demo@files.example.test:/home/demo/uploads/archive/big-upload.bin";
        assert_eq!(shorten_route("a.txt", "b.txt", 40), "a.txt → b.txt");
        let short = shorten_route(source, target, 70);
        assert_eq!(short.chars().count(), 70);
        assert!(short.contains("big-upload.bin → "));
        assert!(short.ends_with("/big-upload.bin"));
        assert!(short.starts_with(r"C:\"));
        let local = shorten_route(r"D:\a.txt", target, 40);
        assert!(local.starts_with(r"D:\a.txt → "));
        assert_eq!(local.chars().count(), 40);
    }
}
