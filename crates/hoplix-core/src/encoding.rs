// Windows-1252

const HIGH_CONTROLS: [char; 32] = [
    '\u{20AC}', '\u{0081}', '\u{201A}', '\u{0192}', '\u{201E}', '\u{2026}', '\u{2020}', '\u{2021}',
    '\u{02C6}', '\u{2030}', '\u{0160}', '\u{2039}', '\u{0152}', '\u{008D}', '\u{017D}', '\u{008F}',
    '\u{0090}', '\u{2018}', '\u{2019}', '\u{201C}', '\u{201D}', '\u{2022}', '\u{2013}', '\u{2014}',
    '\u{02DC}', '\u{2122}', '\u{0161}', '\u{203A}', '\u{0153}', '\u{009D}', '\u{017E}', '\u{0178}',
];

pub fn decode_windows_1252(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            0x80..=0x9F => HIGH_CONTROLS[usize::from(byte - 0x80)],
            _ => char::from(byte),
        })
        .collect()
}

pub fn encode_windows_1252(text: &str) -> Option<Vec<u8>> {
    text.chars().map(encode_char).collect()
}

fn encode_char(character: char) -> Option<u8> {
    if let Some(index) = HIGH_CONTROLS.iter().position(|&known| known == character) {
        return u8::try_from(index).ok().map(|index| index + 0x80);
    }
    let code = u32::from(character);
    match code {
        0x00..=0x7F | 0xA0..=0xFF => u8::try_from(code).ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latin_letters_and_symbols_round_trip() {
        let text = "Grüße – café €5 „quoted“ œuvre™ Ÿ";
        let bytes = encode_windows_1252(text).expect("representable");
        assert_eq!(bytes.len(), text.chars().count());
        assert_eq!(decode_windows_1252(&bytes), text);
        assert_eq!(encode_windows_1252("café"), Some(b"caf\xE9".to_vec()));
        assert_eq!(encode_windows_1252("€"), Some(vec![0x80]));
        assert_eq!(decode_windows_1252(b"\x93Hi\x94 \x96 \xFC"), "“Hi” – ü");
    }

    #[test]
    fn every_byte_decodes_and_encodes_back() {
        let all: Vec<u8> = (0..=255).collect();
        let text = decode_windows_1252(&all);
        assert_eq!(text.chars().count(), 256);
        assert_eq!(encode_windows_1252(&text), Some(all));
    }

    #[test]
    fn characters_outside_the_code_page_are_rejected() {
        assert_eq!(encode_windows_1252("日本"), None);
        assert_eq!(encode_windows_1252("emoji 😀"), None);
        assert_eq!(encode_windows_1252("\u{0100}"), None);
        assert_eq!(encode_windows_1252(""), Some(Vec::new()));
    }
}
