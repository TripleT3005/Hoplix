// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Element {
    pub name: String,
    pub text: String,
    pub children: Vec<Element>,
}

// Queries

impl Element {
    pub(crate) fn child(&self, name: &str) -> Option<&Self> {
        self.children.iter().find(|child| child.name == name)
    }

    pub(crate) fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Self> {
        self.children.iter().filter(move |child| child.name == name)
    }

    pub(crate) fn text_of(&self, name: &str) -> Option<&str> {
        self.child(name).map(|child| child.text.as_str())
    }
}

// Parsing

pub(crate) fn parse(text: &str) -> Option<Element> {
    let mut stack: Vec<Element> = Vec::new();
    let mut root = None;
    let mut rest = text.trim_start_matches('\u{feff}');
    loop {
        let Some(start) = rest.find('<') else {
            if !rest.trim().is_empty() || !stack.is_empty() {
                return None;
            }
            break;
        };
        let content = &rest[..start];
        match stack.last_mut() {
            Some(top) => top.text.push_str(&decode(content)?),
            None if !content.trim().is_empty() => return None,
            None => {}
        }
        rest = &rest[start..];
        if let Some(after) = rest.strip_prefix("<?") {
            rest = &after[after.find("?>")? + 2..];
        } else if let Some(after) = rest.strip_prefix("<!--") {
            rest = &after[after.find("-->")? + 3..];
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after.find("]]>")?;
            stack.last_mut()?.text.push_str(&after[..end]);
            rest = &after[end + 3..];
        } else if rest.starts_with("<!") {
            return None;
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after.find('>')?;
            let name = local_name(after[..end].trim());
            let element = stack.pop()?;
            if element.name != name {
                return None;
            }
            attach(&mut stack, &mut root, element)?;
            rest = &after[end + 1..];
        } else {
            let end = tag_end(rest)?;
            let tag = &rest[1..end];
            let (tag, closed) = match tag.strip_suffix('/') {
                Some(tag) => (tag, true),
                None => (tag, false),
            };
            let name = tag
                .split(|c: char| c.is_ascii_whitespace())
                .next()
                .filter(|name| !name.is_empty())?;
            let element = Element {
                name: local_name(name).to_string(),
                ..Element::default()
            };
            if closed {
                attach(&mut stack, &mut root, element)?;
            } else {
                stack.push(element);
            }
            rest = &rest[end + 1..];
        }
    }
    root
}

fn attach(stack: &mut [Element], root: &mut Option<Element>, element: Element) -> Option<()> {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None if root.is_none() => *root = Some(element),
        None => return None,
    }
    Some(())
}

fn tag_end(text: &str) -> Option<usize> {
    let mut quote = None;
    for (index, character) in text.char_indices() {
        match (quote, character) {
            (None, '"' | '\'') => quote = Some(character),
            (Some(open), _) if open == character => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

fn local_name(name: &str) -> &str {
    name.rsplit_once(':').map_or(name, |(_, local)| local)
}

fn decode(text: &str) -> Option<String> {
    if !text.contains('&') {
        return Some(text.to_string());
    }
    let mut decoded = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        decoded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after.find(';')?;
        let entity = &after[..end];
        let character = match entity {
            "lt" => '<',
            "gt" => '>',
            "amp" => '&',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = entity.strip_prefix('#')?;
                let code = match number.strip_prefix(['x', 'X']) {
                    Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                    None => number.parse().ok()?,
                };
                char::from_u32(code)?
            }
        };
        decoded.push(character);
        rest = &after[end + 1..];
    }
    decoded.push_str(rest);
    Some(decoded)
}

pub(crate) fn escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '&' => escaped.push_str("&amp;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            '\r' => escaped.push_str("&#13;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_become_trees() {
        let root = parse(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- note -->\
             <a:Root xmlns:a=\"urn:x\" attr='1>2'><Name> spaced &amp; &lt;b&gt; &#x41;&#66; </Name>\
             <Empty/><Item>1</Item><Item><![CDATA[<raw>]]></Item></a:Root>",
        )
        .expect("parsing failed");
        assert_eq!(root.name, "Root");
        assert_eq!(root.text_of("Name"), Some(" spaced & <b> AB "));
        assert_eq!(root.text_of("Empty"), Some(""));
        let items: Vec<&str> = root
            .children_named("Item")
            .map(|item| item.text.as_str())
            .collect();
        assert_eq!(items, ["1", "<raw>"]);
        assert_eq!(root.child("Missing"), None);
    }

    #[test]
    fn malformed_documents_are_refused() {
        for text in [
            "",
            "<a>",
            "<a></b>",
            "<a/><b/>",
            "text<a/>",
            "<a>&unknown;</a>",
            "<a>&amp</a>",
            "<!DOCTYPE a [<!ENTITY x \"y\">]><a>&x;</a>",
            "<a><![CDATA[x</a>",
            "<>",
        ] {
            assert_eq!(parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn text_is_escaped_for_requests() {
        assert_eq!(escape("a<b>&\"c'\r"), "a&lt;b&gt;&amp;&quot;c&apos;&#13;");
        let document = format!("<Key>{}</Key>", escape("x & <y>"));
        assert_eq!(
            parse(&document).map(|root| root.text),
            Some("x & <y>".to_string())
        );
    }
}
