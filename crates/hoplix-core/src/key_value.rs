// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Section {
    pub name: String,
    pub pairs: Vec<(String, String)>,
}

impl Section {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

// Parsing

pub fn parse(text: &str) -> Vec<(String, String)> {
    text.trim_start_matches('\u{feff}')
        .lines()
        .filter_map(parse_line)
        .collect()
}

pub fn parse_sections(text: &str) -> Vec<Section> {
    let mut sections = vec![Section::default()];
    for line in text.trim_start_matches('\u{feff}').lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            sections.push(Section {
                name: name.trim().to_string(),
                pairs: Vec::new(),
            });
        } else if let Some(pair) = parse_line(line)
            && let Some(section) = sections.last_mut()
        {
            section.pairs.push(pair);
        }
    }
    sections
}

fn parse_line(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let (key, value) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty()).then(|| (key.to_string(), unescape(value.trim())))
}

// Writing

pub fn write(pairs: &[(&str, String)]) -> String {
    pairs
        .iter()
        .map(|(key, value)| format!("{key} = {}\n", escape(value)))
        .collect()
}

// Hex

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

// Helpers

fn unescape(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            result.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => result.push('\n'),
            Some('\\') => result.push('\\'),
            Some(other) => {
                result.push('\\');
                result.push(other);
            }
            None => result.push('\\'),
        }
    }
    result
}

fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('\n', "\\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pairs_and_skips_comments() {
        let text = "\u{feff}# header\n\n key = value \nbroken line\n=empty key\nmulti = a\\nb\npath = C:\\\\data\n";
        assert_eq!(
            parse(text),
            [
                ("key".to_string(), "value".to_string()),
                ("multi".to_string(), "a\nb".to_string()),
                ("path".to_string(), "C:\\data".to_string()),
            ]
        );
    }

    #[test]
    fn written_pairs_parse_back() {
        let pairs = [("a", "line\nbreak".to_string()), ("b", "C:\\x".to_string())];
        let parsed = parse(&write(&pairs));
        assert_eq!(parsed[0], ("a".to_string(), "line\nbreak".to_string()));
        assert_eq!(parsed[1], ("b".to_string(), "C:\\x".to_string()));
    }

    #[test]
    fn sections_group_their_pairs() {
        let sections = parse_sections("top = 1\n[site]\nname = A\n\n[ site ]\nname = B\n# c\n");
        assert_eq!(sections.len(), 3);
        assert_eq!(sections[0].name, "");
        assert_eq!(sections[0].get("top"), Some("1"));
        assert_eq!(sections[1].name, "site");
        assert_eq!(sections[1].get("name"), Some("A"));
        assert_eq!(sections[2].name, "site");
        assert_eq!(sections[2].get("name"), Some("B"));
        assert_eq!(sections[2].get("missing"), None);
    }

    #[test]
    fn hex_round_trips_and_rejects_garbage() {
        assert_eq!(to_hex(&[0, 15, 255]), "000fff");
        assert_eq!(from_hex("000fff"), Some(vec![0, 15, 255]));
        assert_eq!(from_hex("abc"), None);
        assert_eq!(from_hex("zz"), None);
        assert_eq!(from_hex(""), Some(Vec::new()));
    }
}
