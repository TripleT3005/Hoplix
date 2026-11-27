use std::time::SystemTime;

use super::date::parse_http_date;
use crate::{Error, ErrorKind, Result};

// Constants

const DAV: &str = "DAV:";
const OWNCLOUD: &str = "http://owncloud.org/ns";
const MAX_DEPTH: usize = 128;

// Types

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Element {
    namespace: String,
    name: String,
    text: String,
    children: Vec<Self>,
}

struct Open {
    element: Element,
    qualified: String,
    declarations: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Resource {
    pub href: String,
    pub display_name: Option<String>,
    pub length: Option<u64>,
    pub modified: Option<SystemTime>,
    pub collection: bool,
    pub etag: Option<String>,
    pub owncloud: bool,
    pub status: Option<u16>,
    pub prop_statuses: Vec<u16>,
}

// Elements

impl Element {
    fn is(&self, namespace: &str, name: &str) -> bool {
        self.namespace == namespace && self.name == name
    }

    fn child(&self, namespace: &str, name: &str) -> Option<&Self> {
        self.children.iter().find(|child| child.is(namespace, name))
    }

    fn children_named<'a>(
        &'a self,
        namespace: &'a str,
        name: &'a str,
    ) -> impl Iterator<Item = &'a Self> + 'a {
        self.children
            .iter()
            .filter(move |child| child.is(namespace, name))
    }
}

// Parsing

fn parse_document(text: &str) -> Result<Element> {
    let mut stack: Vec<Open> = Vec::new();
    let mut scopes: Vec<(String, String)> = Vec::new();
    let mut root = None;
    let mut rest = text.strip_prefix('\u{feff}').unwrap_or(text);
    while !rest.is_empty() {
        let Some(start) = rest.find('<') else {
            append_text(&mut stack, &unescape(rest));
            break;
        };
        append_text(&mut stack, &unescape(&rest[..start]));
        rest = &rest[start..];
        if let Some(after) = rest.strip_prefix("<?") {
            rest = skip_past(after, "?>")?;
        } else if let Some(after) = rest.strip_prefix("<!--") {
            rest = skip_past(after, "-->")?;
        } else if let Some(after) = rest.strip_prefix("<![CDATA[") {
            let end = after
                .find("]]>")
                .ok_or_else(|| malformed("unclosed CDATA"))?;
            append_text(&mut stack, &after[..end]);
            rest = &after[end + 3..];
        } else if rest.starts_with("<!") {
            return Err(malformed("document type declarations are not allowed"));
        } else if let Some(after) = rest.strip_prefix("</") {
            let end = after
                .find('>')
                .ok_or_else(|| malformed("unclosed end tag"))?;
            let name = after[..end].trim();
            let open = stack.pop().ok_or_else(|| malformed("unexpected end tag"))?;
            if open.qualified != name {
                return Err(malformed("mismatched end tag"));
            }
            scopes.truncate(scopes.len() - open.declarations);
            close(&mut stack, &mut root, open.element)?;
            rest = &after[end + 1..];
        } else {
            let (tag, after, self_closing) = split_tag(&rest[1..])?;
            let (qualified, attributes) = parse_tag(tag)?;
            let mut declarations = 0;
            for (name, value) in &attributes {
                let prefix = if name == "xmlns" {
                    Some(String::new())
                } else {
                    name.strip_prefix("xmlns:").map(str::to_string)
                };
                if let Some(prefix) = prefix {
                    scopes.push((prefix, value.clone()));
                    declarations += 1;
                }
            }
            let (prefix, name) = match qualified.split_once(':') {
                Some((prefix, name)) => (prefix, name),
                None => ("", qualified.as_str()),
            };
            let namespace = scopes
                .iter()
                .rev()
                .find(|(declared, _)| declared == prefix)
                .map(|(_, uri)| uri.clone())
                .unwrap_or_default();
            let element = Element {
                namespace,
                name: name.to_string(),
                ..Element::default()
            };
            if self_closing {
                scopes.truncate(scopes.len() - declarations);
                close(&mut stack, &mut root, element)?;
            } else {
                if stack.len() >= MAX_DEPTH {
                    return Err(malformed("too deeply nested"));
                }
                stack.push(Open {
                    element,
                    qualified,
                    declarations,
                });
            }
            rest = after;
        }
    }
    if !stack.is_empty() {
        return Err(malformed("unclosed element"));
    }
    root.ok_or_else(|| malformed("no root element"))
}

fn append_text(stack: &mut [Open], text: &str) {
    if let Some(open) = stack.last_mut() {
        open.element.text.push_str(text);
    }
}

fn close(stack: &mut [Open], root: &mut Option<Element>, element: Element) -> Result<()> {
    match stack.last_mut() {
        Some(parent) => parent.element.children.push(element),
        None if root.is_none() => *root = Some(element),
        None => return Err(malformed("several root elements")),
    }
    Ok(())
}

fn skip_past<'a>(text: &'a str, end: &str) -> Result<&'a str> {
    text.find(end)
        .map(|index| &text[index + end.len()..])
        .ok_or_else(|| malformed("unclosed markup"))
}

fn split_tag(text: &str) -> Result<(&str, &str, bool)> {
    let mut quote = None;
    for (index, c) in text.char_indices() {
        match (quote, c) {
            (Some(open), _) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '>') => {
                let tag = &text[..index];
                let rest = &text[index + 1..];
                return Ok(match tag.strip_suffix('/') {
                    Some(tag) => (tag, rest, true),
                    None => (tag, rest, false),
                });
            }
            (None, '<') => return Err(malformed("unexpected <")),
            _ => {}
        }
    }
    Err(malformed("unclosed tag"))
}

fn parse_tag(tag: &str) -> Result<(String, Vec<(String, String)>)> {
    let tag = tag.trim();
    let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let name = &tag[..name_end];
    if name.is_empty() {
        return Err(malformed("empty tag"));
    }
    let mut attributes = Vec::new();
    let mut rest = tag[name_end..].trim_start();
    while !rest.is_empty() {
        let equals = rest.find('=').ok_or_else(|| malformed("attribute"))?;
        let key = rest[..equals].trim().to_string();
        let after = rest[equals + 1..].trim_start();
        let quote = after
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')
            .ok_or_else(|| malformed("attribute value"))?;
        let value_end = after[1..]
            .find(quote)
            .ok_or_else(|| malformed("attribute value"))?;
        attributes.push((key, unescape(&after[1..=value_end])));
        rest = after[value_end + 2..].trim_start();
    }
    Ok((name.to_string(), attributes))
}

fn unescape(text: &str) -> String {
    if !text.contains('&') {
        return text.to_string();
    }
    let mut result = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        result.push_str(&rest[..start]);
        rest = &rest[start..];
        let decoded = rest.find(';').filter(|end| *end <= 12).and_then(|end| {
            let entity = &rest[1..end];
            let c = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .map(|hex| u32::from_str_radix(hex, 16))
                    .or_else(|| entity.strip_prefix('#').map(str::parse::<u32>))
                    .and_then(std::result::Result::ok)
                    .and_then(char::from_u32),
            };
            c.map(|c| (c, end))
        });
        match decoded {
            Some((c, end)) => {
                result.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                result.push('&');
                rest = &rest[1..];
            }
        }
    }
    result.push_str(rest);
    result
}

// Multistatus

pub(crate) fn parse_multistatus(data: &[u8]) -> Result<Vec<Resource>> {
    let text = std::str::from_utf8(data).map_err(|_| malformed("not UTF-8"))?;
    let root = parse_document(text)?;
    if !root.is(DAV, "multistatus") {
        return Err(malformed("not a multistatus response"));
    }
    Ok(root
        .children_named(DAV, "response")
        .filter_map(resource)
        .collect())
}

fn resource(response: &Element) -> Option<Resource> {
    let href = response.child(DAV, "href")?.text.trim().to_string();
    let mut resource = Resource {
        href,
        status: response
            .child(DAV, "status")
            .and_then(|status| status_code(&status.text)),
        ..Resource::default()
    };
    for propstat in response.children_named(DAV, "propstat") {
        let Some(status) = propstat
            .child(DAV, "status")
            .and_then(|status| status_code(&status.text))
        else {
            continue;
        };
        resource.prop_statuses.push(status);
        if !(200..300).contains(&status) {
            continue;
        }
        let Some(prop) = propstat.child(DAV, "prop") else {
            continue;
        };
        for property in &prop.children {
            let value = property.text.trim();
            match (property.namespace.as_str(), property.name.as_str()) {
                (DAV, "getcontentlength") => resource.length = value.parse().ok(),
                (DAV, "getlastmodified") => resource.modified = parse_http_date(value),
                (DAV, "resourcetype") => {
                    resource.collection = property.child(DAV, "collection").is_some();
                }
                (DAV, "displayname") if !value.is_empty() => {
                    resource.display_name = Some(property.text.clone());
                }
                (DAV, "getetag") if !value.is_empty() => {
                    resource.etag = Some(value.to_string());
                }
                (OWNCLOUD, "id" | "fileid") => resource.owncloud = true,
                _ => {}
            }
        }
    }
    Some(resource)
}

fn status_code(line: &str) -> Option<u16> {
    line.split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
}

fn malformed(detail: &str) -> Error {
    Error::new(
        ErrorKind::Other,
        format!("invalid WebDAV response: {detail}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    const APACHE: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:" xmlns:ns0="DAV:">
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://apache.org/dav/props/">
<D:href>/dav/demo/</D:href>
<D:propstat>
<D:prop>
<lp1:resourcetype><D:collection/></lp1:resourcetype>
<lp1:getlastmodified>Sun, 15 Mar 2026 13:00:00 GMT</lp1:getlastmodified>
<lp1:getetag>"1000-5b1"</lp1:getetag>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
<D:propstat>
<D:prop>
<ns0:displayname/>
<ns0:getcontentlength/>
</D:prop>
<D:status>HTTP/1.1 404 Not Found</D:status>
</D:propstat>
</D:response>
<D:response xmlns:lp1="DAV:" xmlns:lp2="http://apache.org/dav/props/">
<D:href>/dav/demo/a%20b%20%26%20c%23.txt</D:href>
<D:propstat>
<D:prop>
<lp1:resourcetype/>
<lp1:getcontentlength>1234</lp1:getcontentlength>
<lp1:getlastmodified>Sun, 15 Mar 2026 12:00:00 GMT</lp1:getlastmodified>
<lp1:getetag>"4d2-5b1"</lp1:getetag>
</D:prop>
<D:status>HTTP/1.1 200 OK</D:status>
</D:propstat>
</D:response>
</D:multistatus>
"#;

    const NEXTCLOUD: &str = r#"<?xml version="1.0"?>
<d:multistatus xmlns:d="DAV:" xmlns:s="http://sabredav.org/ns" xmlns:oc="http://owncloud.org/ns" xmlns:nc="http://nextcloud.org/ns"><d:response><d:href>/remote.php/dav/files/demo/Documents/</d:href><d:propstat><d:prop><d:getlastmodified>Sun, 15 Mar 2026 13:00:00 GMT</d:getlastmodified><d:resourcetype><d:collection/></d:resourcetype><d:getetag>&quot;65f4a2b1c3d4e&quot;</d:getetag><oc:id>00000042ocabcdef</oc:id></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat><d:propstat><d:prop><d:getcontentlength/><d:displayname/></d:prop><d:status>HTTP/1.1 404 Not Found</d:status></d:propstat></d:response><d:response><d:href>/remote.php/dav/files/demo/Documents/Gr%c3%bc%c3%9fe.md</d:href><d:propstat><d:prop><d:getlastmodified>Sun, 15 Mar 2026 12:00:00 GMT</d:getlastmodified><d:getcontentlength>42</d:getcontentlength><d:resourcetype/><d:getetag>&quot;abc&quot;</d:getetag><d:displayname>Grüße.md</d:displayname></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#;

    fn at(seconds: u64) -> Option<SystemTime> {
        UNIX_EPOCH.checked_add(Duration::from_secs(seconds))
    }

    #[test]
    fn apache_listings_are_parsed() {
        let resources = parse_multistatus(APACHE.as_bytes()).expect("valid");
        assert_eq!(resources.len(), 2);
        let folder = &resources[0];
        assert_eq!(folder.href, "/dav/demo/");
        assert!(folder.collection && !folder.owncloud);
        assert_eq!(folder.length, None);
        assert_eq!(folder.display_name, None);
        assert_eq!(folder.modified, at(1_773_579_600));
        assert_eq!(folder.prop_statuses, [200, 404]);
        let file = &resources[1];
        assert_eq!(file.href, "/dav/demo/a%20b%20%26%20c%23.txt");
        assert!(!file.collection);
        assert_eq!(file.length, Some(1234));
        assert_eq!(file.etag.as_deref(), Some("\"4d2-5b1\""));
        assert_eq!(file.modified, at(1_773_576_000));
    }

    #[test]
    fn nextcloud_listings_are_parsed() {
        let resources = parse_multistatus(NEXTCLOUD.as_bytes()).expect("valid");
        assert_eq!(resources.len(), 2);
        assert!(resources[0].owncloud && resources[0].collection);
        assert_eq!(resources[0].etag.as_deref(), Some("\"65f4a2b1c3d4e\""));
        assert_eq!(resources[1].display_name.as_deref(), Some("Grüße.md"));
        assert_eq!(resources[1].length, Some(42));
    }

    #[test]
    fn namespaces_decide_not_prefixes() {
        let text = r#"<multistatus xmlns="DAV:"><response><href>/a</href>
            <propstat><prop xmlns:x="urn:other">
            <x:getcontentlength>99</x:getcontentlength>
            <getcontentlength xmlns="DAV:">7</getcontentlength>
            <resourcetype><x:collection/></resourcetype>
            </prop><status>HTTP/1.1 200 OK</status></propstat></response>
            <x:response xmlns:x="urn:other"><href>/ignored</href></x:response>
            </multistatus>"#;
        let resources = parse_multistatus(text.as_bytes()).expect("valid");
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].length, Some(7));
        assert!(!resources[0].collection);
    }

    #[test]
    fn entities_cdata_comments_and_missing_props() {
        let text = "<?xml version='1.0'?><!-- comment --><D:multistatus xmlns:D='DAV:'>\
            <D:response><D:href><![CDATA[/a&b/]]></D:href></D:response>\
            <D:response><D:href>/x&amp;y&#47;&#x7a;</D:href><D:status>HTTP/1.1 404 Not Found</D:status></D:response>\
            <D:response><D:propstat><D:status>HTTP/1.1 200 OK</D:status></D:propstat></D:response>\
            </D:multistatus>";
        let resources = parse_multistatus(text.as_bytes()).expect("valid");
        assert_eq!(resources.len(), 2);
        assert_eq!(resources[0].href, "/a&b/");
        assert_eq!(resources[0].length, None);
        assert_eq!(resources[1].href, "/x&y/z");
        assert_eq!(resources[1].status, Some(404));
        assert_eq!(unescape("a &unknown; & b &#xZZ;"), "a &unknown; & b &#xZZ;");
    }

    #[test]
    fn broken_documents_are_refused() {
        for text in [
            "",
            "plain text",
            "<D:multistatus xmlns:D=\"DAV:\">",
            "<a></b>",
            "<a/><b/>",
            "<!DOCTYPE x [<!ENTITY e \"boom\">]><a>&e;</a>",
            "<a x=1/>",
            "<multistatus xmlns=\"urn:other\"/>",
            "<a><!-- open",
        ] {
            assert!(parse_multistatus(text.as_bytes()).is_err(), "{text}");
        }
        assert!(parse_multistatus(&[0xff, 0xfe]).is_err());
        let deep = "<a>".repeat(MAX_DEPTH + 1);
        assert!(parse_document(&deep).is_err());
    }

    #[test]
    fn attributes_may_hold_angle_brackets() {
        let root = parse_document("<a title='x > y' other=\"&lt;\"><b/></a>").expect("valid");
        assert_eq!(root.name, "a");
        assert_eq!(root.children.len(), 1);
    }
}
