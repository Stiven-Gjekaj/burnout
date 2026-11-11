//! A small reader for the XML that a WIM file holds.
//!
//! The XML of a WIM file has elements, attributes, text and the five named
//! entities, and no more. So this reader takes those, and skips a comment,
//! a declaration and a processing instruction.

/// One element, with the elements and the text inside it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Element {
    pub name: String,
    pub attributes: Vec<(String, String)>,
    pub children: Vec<Element>,
    /// The text directly inside the element, with the space at each end
    /// taken away.
    pub text: String,
}

impl Element {
    /// The first child with this name, which an upper or lower case does not
    /// change.
    pub fn child(&self, name: &str) -> Option<&Element> {
        self.children
            .iter()
            .find(|c| c.name.eq_ignore_ascii_case(name))
    }

    /// The children with this name.
    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> {
        self.children
            .iter()
            .filter(move |c| c.name.eq_ignore_ascii_case(name))
    }

    /// The text of the element at the end of `path`, a list of child names.
    pub fn text_at(&self, path: &[&str]) -> Option<&str> {
        let mut at = self;
        for name in path {
            at = at.child(name)?;
        }
        Some(at.text.as_str())
    }

    pub fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Read the top element of `text`. The error says what is wrong.
pub fn parse(text: &str) -> Result<Element, String> {
    let mut stack: Vec<Element> = Vec::new();
    let mut top: Option<Element> = None;
    let mut rest = text;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            add_text(&mut stack, rest)?;
            break;
        };
        add_text(&mut stack, &rest[..lt])?;
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = skip_past(r, "-->")?;
        } else if let Some(r) = rest.strip_prefix("<?") {
            rest = skip_past(r, "?>")?;
        } else if let Some(r) = rest.strip_prefix("<!") {
            rest = skip_past(r, ">")?;
        } else if let Some(r) = rest.strip_prefix("</") {
            let gt = r.find('>').ok_or("an end tag has no >")?;
            let name = r[..gt].trim();
            let element = stack
                .pop()
                .ok_or_else(|| format!("the end tag </{name}> has no start tag"))?;
            if element.name != name {
                return Err(format!(
                    "the end tag </{name}> closes the element <{}>",
                    element.name
                ));
            }
            close(&mut stack, &mut top, element)?;
            rest = &r[gt + 1..];
        } else {
            let gt = rest.find('>').ok_or("a start tag has no >")?;
            let inside = &rest[1..gt];
            let (inside, empty) = match inside.strip_suffix('/') {
                Some(i) => (i, true),
                None => (inside, false),
            };
            let element = start_tag(inside)?;
            if empty {
                close(&mut stack, &mut top, element)?;
            } else {
                stack.push(element);
            }
            rest = &rest[gt + 1..];
        }
    }
    if let Some(open) = stack.last() {
        return Err(format!("the element <{}> has no end tag", open.name));
    }
    top.ok_or_else(|| "it holds no element".to_string())
}

fn skip_past<'a>(text: &'a str, end: &str) -> Result<&'a str, String> {
    text.find(end)
        .map(|at| &text[at + end.len()..])
        .ok_or_else(|| format!("a part has no {end}"))
}

fn add_text(stack: &mut [Element], text: &str) -> Result<(), String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    let open = stack
        .last_mut()
        .ok_or("it holds text outside of any element")?;
    let decoded = decode(text)?;
    if !open.text.is_empty() {
        open.text.push(' ');
    }
    open.text.push_str(&decoded);
    Ok(())
}

fn close(stack: &mut [Element], top: &mut Option<Element>, element: Element) -> Result<(), String> {
    match stack.last_mut() {
        Some(parent) => parent.children.push(element),
        None if top.is_none() => *top = Some(element),
        None => return Err("it holds more than one top element".to_string()),
    }
    Ok(())
}

fn start_tag(inside: &str) -> Result<Element, String> {
    let inside = inside.trim();
    let name_end = inside
        .find(|c: char| c.is_whitespace())
        .unwrap_or(inside.len());
    let name = &inside[..name_end];
    if name.is_empty() {
        return Err("a start tag has no name".to_string());
    }
    let mut attributes = Vec::new();
    let mut rest = inside[name_end..].trim_start();
    while !rest.is_empty() {
        let eq = rest
            .find('=')
            .ok_or_else(|| format!("an attribute of <{name}> has no value"))?;
        let key = rest[..eq].trim().to_string();
        let after = rest[eq + 1..].trim_start();
        let quote = after
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'')
            .ok_or_else(|| format!("the attribute {key} of <{name}> has no quote"))?;
        let close = after[1..]
            .find(quote)
            .ok_or_else(|| format!("the attribute {key} of <{name}> has no end quote"))?;
        attributes.push((key, decode(&after[1..1 + close])?));
        rest = after[close + 2..].trim_start();
    }
    Ok(Element {
        name: name.to_string(),
        attributes,
        ..Element::default()
    })
}

fn decode(text: &str) -> Result<String, String> {
    if !text.contains('&') {
        return Ok(text.to_string());
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let semi = rest[amp..].find(';').ok_or("an entity has no ;")?;
        let entity = &rest[amp + 1..amp + semi];
        let c = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let code = if let Some(hex) = entity.strip_prefix("#x") {
                    u32::from_str_radix(hex, 16).ok()
                } else if let Some(dec) = entity.strip_prefix('#') {
                    dec.parse().ok()
                } else {
                    None
                };
                code.and_then(char::from_u32)
                    .ok_or_else(|| format!("the entity &{entity}; is not known"))?
            }
        };
        out.push(c);
        rest = &rest[amp + semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_attributes_and_text_come_through() {
        let e = parse(
            "<?xml version=\"1.0\"?><!-- a note --><WIM><IMAGE INDEX=\"1\" X='a &amp; b'>\
             <NAME> Windows 10 Home </NAME><EMPTY/><VERSION><BUILD>19041</BUILD></VERSION>\
             </IMAGE><IMAGE INDEX=\"2\"/></WIM>",
        )
        .unwrap();
        assert_eq!(e.name, "WIM");
        let images: Vec<_> = e.children_named("image").collect();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].attribute("index"), Some("1"));
        assert_eq!(images[0].attribute("X"), Some("a & b"));
        assert_eq!(images[0].text_at(&["NAME"]), Some("Windows 10 Home"));
        assert_eq!(images[0].text_at(&["VERSION", "BUILD"]), Some("19041"));
        assert_eq!(images[0].text_at(&["EMPTY"]), Some(""));
        assert_eq!(images[0].text_at(&["NOTHING"]), None);
        assert_eq!(images[1].attribute("INDEX"), Some("2"));
    }

    #[test]
    fn the_entities_are_decoded() {
        let e = parse("<A>&lt;x&gt; &quot;&apos; &#65;&#x42;</A>").unwrap();
        assert_eq!(e.text, "<x> \"' AB");
        assert!(parse("<A>&nope;</A>").unwrap_err().contains("&nope;"));
        assert!(parse("<A>&amp</A>").unwrap_err().contains("no ;"));
    }

    #[test]
    fn a_broken_document_says_what_is_wrong() {
        for (text, says) in [
            ("<A><B></A>", "</A> closes the element <B>"),
            ("<A>", "<A> has no end tag"),
            ("</A>", "has no start tag"),
            ("text", "outside of any element"),
            ("<A/><B/>", "more than one top element"),
            ("", "holds no element"),
            ("<A x=1/>", "has no quote"),
            ("<A x/>", "has no value"),
            ("<A", "has no >"),
        ] {
            let e = parse(text).unwrap_err();
            assert!(e.contains(says), "{text:?} gave {e:?}");
        }
    }
}
