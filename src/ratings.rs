//! Ratings, flags and tags, stored as XMP sidecars.
//!
//! The sidecar for `photo.jpg` is `photo.jpg.xmp` (appended, not
//! `photo.jpg` with its extension replaced), so two files sharing a stem
//! keep separate sidecars. There are no ratings inside archives:
//! [`sidecar_path`] returns `None` there.
//!
//! The load-bearing property of this whole feature: **a round trip preserves
//! every byte it did not write.** [`merge`] is a byte-level attribute editor:
//! it replaces the value between the quotes of `xmp:Rating` / `xmp:Label` in
//! place (or inserts the attribute into the first `<rdf:Description>` tag when
//! absent) and swaps only our own `<iviewer:Keywords>` span, leaving every
//! other byte of the file — another tool's Camera Raw packet, histogram,
//! mask — identical. A sidecar is only ever constructed from scratch when
//! there is nothing worth preserving yet (absent, blank, or not XMP at all).

use std::path::{Path, PathBuf};

/// Our own keywords namespace. Namespaced under our own URI so it cannot
/// collide with another tool's element.
const IVIEWER_NS: &str = "https://image-viewer.local/ns/1.0/";
const XMP_NS: &str = "http://ns.adobe.com/xap/1.0/";

/// One image's star rating and pick flag.
///
/// `stars` is clamped to `0..=5` wherever it is *used* (the fields are public,
/// so no constructor can enforce it); the flag is stored as `xmp:Label`
/// `Pick`, which is what makes the file readable in Lightroom.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rating {
    pub stars: u8,
    pub flag: bool,
}

impl Default for Rating {
    fn default() -> Self {
        Rating { stars: 0, flag: false }
    }
}

impl Rating {
    fn clamped_stars(&self) -> u8 {
        self.stars.min(5)
    }
}

/// Everything the sidecar holds for one image.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ratings {
    pub rating: Rating,
    pub tags: Vec<String>,
}

/// The sidecar path for an image, or `None` inside an archive.
///
/// Appends `.xmp` (`photo.png` becomes `photo.png.xmp`) so files sharing a
/// stem do not share a sidecar.
pub fn sidecar_path(image: &Path) -> Option<PathBuf> {
    if crate::is_archive_path(image) {
        return None;
    }
    let mut s = image.as_os_str().to_os_string();
    s.push(".xmp");
    Some(PathBuf::from(s))
}

/// The end of a markup span starting at `lt`: `-->` for comments, `?>` for
/// processing instructions, `]]>` for CDATA, `>` otherwise (quote-aware, so
/// a `>` inside an attribute value does not end the tag early).
fn tag_end(text: &str, lt: usize) -> Option<usize> {
    let rest = &text[lt..];
    if rest.starts_with("<!--") {
        return Some(lt + rest.find("-->")? + "-->".len());
    }
    if rest.starts_with("<?") {
        return Some(lt + rest.find("?>")? + "?>".len());
    }
    if rest.starts_with("<![CDATA[") {
        return Some(lt + rest.find("]]>")? + "]]>".len());
    }
    let bytes = text.as_bytes();
    let mut i = lt + 1;
    let mut quote: Option<u8> = None;
    while i < bytes.len() {
        let c = bytes[i];
        if let Some(q) = quote {
            if c == q {
                quote = None;
            }
        } else if c == b'"' || c == b'\'' {
            quote = Some(c);
        } else if c == b'>' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// The byte range of the value in `name="‹value›"`: `Some((value_start,
/// value_end))`. Only matches inside a real tag (`<…>`), preceded by
/// whitespace, with the match outside any quoted span — so a literal
/// `xmp:Rating="…"` sitting in element text, a comment or another attribute's
/// value is never mistaken for the attribute.
fn attr_value_range(text: &str, name: &str) -> Option<(usize, usize)> {
    let mut i = 0;
    while i < text.len() {
        let lt = text[i..].find('<')? + i;
        let gt = tag_end(text, lt)?;
        let rest = &text[lt..];
        let is_tag = !(rest.starts_with("<!--") || rest.starts_with("<?") || rest.starts_with("<![CDATA["));
        if is_tag {
            let tag = &text[lt + 1..gt - 1];
            let tag_bytes = tag.as_bytes();
            let mut j = 0;
            while j < tag_bytes.len() {
                let c = tag_bytes[j];
                if c == b'"' {
                    j += tag[j + 1..].find('"').map(|p| p + 1).unwrap_or(tag_bytes.len());
                    j += 1;
                    continue;
                }
                if c == b'\'' {
                    j += tag[j + 1..].find('\'').map(|p| p + 1).unwrap_or(tag_bytes.len());
                    j += 1;
                    continue;
                }
                if tag[j..].starts_with(name)
                    && j > 0
                    && tag_bytes[j - 1].is_ascii_whitespace()
                {
                    let mut k = j + name.len();
                    while k < tag_bytes.len() && tag_bytes[k].is_ascii_whitespace() {
                        k += 1;
                    }
                    if tag_bytes.get(k) == Some(&b'=') {
                        k += 1;
                        while k < tag_bytes.len() && tag_bytes[k].is_ascii_whitespace() {
                            k += 1;
                        }
                        if tag_bytes.get(k) == Some(&b'"') {
                            let vs = lt + 1 + k + 1;
                            // Bound the search to this tag: an unterminated
                            // quote must not match a quote lines away.
                            let ve = vs + text[vs..gt].find('"')?;
                            return Some((vs, ve));
                        }
                    }
                }
                j += 1;
            }
        }
        i = gt;
    }
    None
}

/// The span of the first `<rdf:Description …>` open tag: `(tag_start,
/// tag_end)` with `tag_end` just past the closing `>`. `<rdf:DescriptionFoo`
/// is a longer name, not our tag.
fn first_description_tag(text: &str) -> Option<(usize, usize)> {
    let mut search = 0;
    while let Some(lt) = text[search..].find("<rdf:Description").map(|p| p + search) {
        let after_name = lt + "<rdf:Description".len();
        match text.as_bytes().get(after_name) {
            Some(&c) if c.is_ascii_whitespace() || c == b'/' || c == b'>' => {
                return tag_end(text, lt).map(|gt| (lt, gt));
            }
            _ => search = after_name,
        }
    }
    None
}

/// Insert `addition` into the open tag spanning `lt..tag_end`, just before the
/// tag's `/>` or `>`.
fn insert_into_tag(text: &str, tag_end: usize, addition: &str) -> String {
    let bytes = text.as_bytes();
    let mut insert_at = tag_end - 1;
    if bytes[insert_at] == b'>' && insert_at > 0 && bytes[insert_at - 1] == b'/' {
        insert_at -= 1;
    }
    let mut out = String::with_capacity(text.len() + addition.len());
    out.push_str(&text[..insert_at]);
    out.push_str(addition);
    out.push_str(&text[insert_at..]);
    out
}

fn xmlns_uri(name: &str) -> &'static str {
    if name == "xmlns:iviewer" { IVIEWER_NS } else { XMP_NS }
}

/// Replace the value of `name` in place, or insert ` name="value"` into the
/// first `<rdf:Description>` tag when absent (adding the `xmlns` declaration
/// when the file lacks it). The caller guarantees a Description tag exists.
fn set_description_attr(text: &str, name: &str, value: &str, xmlns: &str) -> String {
    if let Some((vs, ve)) = attr_value_range(text, name) {
        let mut out = String::with_capacity(text.len());
        out.push_str(&text[..vs]);
        out.push_str(value);
        out.push_str(&text[ve..]);
        return out;
    }
    let Some((lt, tag_end)) = first_description_tag(text) else {
        return text.to_string();
    };
    let mut addition = String::new();
    if attr_value_range(&text[lt..tag_end], xmlns).is_none() {
        addition.push(' ');
        addition.push_str(xmlns);
        addition.push_str("=\"");
        addition.push_str(xmlns_uri(xmlns));
        addition.push('"');
    }
    addition.push(' ');
    addition.push_str(name);
    addition.push_str("=\"");
    addition.push_str(value);
    addition.push('"');
    insert_into_tag(text, tag_end, &addition)
}

/// Declare `xmlns` on the first Description tag when it is not already there.
/// The caller guarantees a Description tag exists.
fn ensure_xmlns(text: &str, xmlns: &str) -> String {
    let Some((lt, tag_end)) = first_description_tag(text) else {
        return text.to_string();
    };
    if attr_value_range(&text[lt..tag_end], xmlns).is_some() {
        return text.to_string();
    }
    let addition = format!(" {xmlns}=\"{}\"", xmlns_uri(xmlns));
    insert_into_tag(text, tag_end, &addition)
}

/// The span of our whole `<iviewer:Keywords>…</iviewer:Keywords>` element, or
/// of a self-closing `<iviewer:Keywords …/>`, if present.
fn keywords_span(text: &str) -> Option<(usize, usize)> {
    let mut search = 0;
    loop {
        let lt = text[search..].find("<iviewer:Keywords").map(|p| p + search)?;
        let after_name = lt + "<iviewer:Keywords".len();
        match text.as_bytes().get(after_name) {
            Some(&c) if c.is_ascii_whitespace() || c == b'/' || c == b'>' => {
                let open_end = lt + text[lt..].find('>')? + 1;
                if text[..open_end].trim_end().ends_with("/>") {
                    return Some((lt, open_end));
                }
                let close = text[open_end..].find("</iviewer:Keywords>")? + open_end;
                return Some((lt, close + "</iviewer:Keywords>".len()));
            }
            _ => search = after_name,
        }
    }
}

fn render_keywords(tags: &[String]) -> String {
    let mut out = String::from("<iviewer:Keywords><rdf:Bag>");
    for tag in tags {
        out.push_str("<rdf:li>");
        out.push_str(&escape(tag));
        out.push_str("</rdf:li>");
    }
    out.push_str("</rdf:Bag></iviewer:Keywords>");
    out
}

/// Build a minimal sidecar holding only our attributes. This is the only case
/// where anything is constructed from scratch: there is nothing worth
/// preserving yet.
fn minimal(ratings: &Ratings) -> String {
    let mut out = String::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
         <rdf:Description rdf:about=\"\"\n   \
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n   \
         xmlns:iviewer=\"https://image-viewer.local/ns/1.0/\"",
    );
    out.push_str(&format!("\n   xmp:Rating=\"{}\"", ratings.rating.clamped_stars()));
    if ratings.rating.flag {
        out.push_str("\n   xmp:Label=\"Pick\"");
    }
    out.push('>');
    out.push_str(&render_keywords(&ratings.tags));
    out.push_str("</rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
    out
}

/// Merge `new` into `existing` by string surgery, never a re-serialise: every
/// byte outside the edited spans is the input's.
///
/// - `xmp:Rating` is always written: replaced in place when present, inserted
///   into the first `<rdf:Description>` tag when absent.
/// - `xmp:Label` is written only when the flag changed (`Pick`/`Reject`); an
///   unchanged file keeps its bytes as-is, which is what preserves a `Second`
///   another tool made — and what keeps an absent label absent.
/// - `<iviewer:Keywords>` is ours, so its whole span is replaced wholesale;
///   when absent it is inserted before `</rdf:RDF>`.
fn merge(existing: &str, new: &Ratings) -> String {
    if existing.trim().is_empty() || !existing.contains("<x:xmpmeta") {
        // Absent, blank, or not XMP at all: nothing worth preserving.
        return minimal(new);
    }
    let old = parse(existing).unwrap_or_default();

    let mut out: String;
    if first_description_tag(existing).is_none() {
        // XMP around it but no Description to hang attributes on: give the
        // rating a home rather than dropping it, without touching anything
        // else. Keywords ride along so the insert-below finds its span.
        let mut block = String::from("<rdf:Description rdf:about=\"\" ");
        block.push_str(&format!("xmlns:xmp=\"{XMP_NS}\" "));
        block.push_str(&format!("xmlns:iviewer=\"{IVIEWER_NS}\" "));
        block.push_str(&format!("xmp:Rating=\"{}\"", new.rating.clamped_stars()));
        if new.rating.flag {
            block.push_str(" xmp:Label=\"Pick\"");
        }
        block.push('>');
        block.push_str(&render_keywords(&new.tags));
        block.push_str("</rdf:Description>");
        out = existing.to_string();
        if let Some(pos) = out.find("</rdf:RDF>") {
            out.insert_str(pos, &block);
        } else {
            out.push_str(&block);
        }
        return out;
    } else {
        out = set_description_attr(
            existing,
            "xmp:Rating",
            &new.rating.clamped_stars().to_string(),
            "xmlns:xmp",
        );
    }

    // Only touch the label when the flag changed, so a `Second` (or an
    // absent label) survives a rating-only edit byte-for-byte.
    if new.rating.flag != old.rating.flag {
        out = set_description_attr(
            &out,
            "xmp:Label",
            if new.rating.flag { "Pick" } else { "Reject" },
            "xmlns:xmp",
        );
    }

    // Our own element: wholesale span replace when present, insert before
    // `</rdf:RDF>` when absent (declaring the namespace on the way in).
    if let Some((s, e)) = keywords_span(&out) {
        out.replace_range(s..e, &render_keywords(&new.tags));
    } else {
        out = ensure_xmlns(&out, "xmlns:iviewer");
        if let Some(pos) = out.find("</rdf:RDF>") {
            out.insert_str(pos, &render_keywords(&new.tags));
        } else {
            out.push_str(&render_keywords(&new.tags));
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Parse sidecar text. `xmp:Rating` clamps to `0..=5` (unparseable is 0);
/// `xmp:Label` `Pick` is a set flag and anything else — `Reject`, `Second`,
/// absent — is unset; tags come from our own `<iviewer:Keywords>` element
/// only, so another tool's `dc:subject` is never adopted as ours. Text with
/// no `<x:xmpmeta` is not a sidecar and is an error.
fn parse(text: &str) -> Result<Ratings, String> {
    if !text.contains("<x:xmpmeta") {
        return Err("not an XMP sidecar: missing <x:xmpmeta".to_string());
    }
    let mut ratings = Ratings::default();
    if let Some((vs, ve)) = attr_value_range(text, "xmp:Rating") {
        ratings.rating.stars = text[vs..ve].trim().parse::<u8>().map(|n| n.min(5)).unwrap_or(0);
    }
    if let Some((vs, ve)) = attr_value_range(text, "xmp:Label") {
        ratings.rating.flag = text[vs..ve].trim() == "Pick";
    }
    if let Some((s, e)) = keywords_span(text) {
        let span = &text[s..e];
        let mut rest = span;
        while let Some(start) = rest.find("<rdf:li>") {
            let after = &rest[start + "<rdf:li>".len()..];
            if let Some(end) = after.find("</rdf:li>") {
                ratings.tags.push(unescape(after[..end].trim()));
                rest = &after[end + "</rdf:li>".len()..];
            } else {
                break;
            }
        }
    }
    Ok(ratings)
}

impl Ratings {
    /// Read the sidecar for `image`; an absent sidecar is all defaults.
    /// A damaged sidecar is an error naming the file — the image itself is
    /// untouched, since a rating is a file we made, not a property of the
    /// picture.
    pub fn read(image: &Path) -> Result<Ratings, String> {
        let sidecar = sidecar_path(image).ok_or_else(|| "no ratings inside archives".to_string())?;
        if !sidecar.exists() {
            return Ok(Ratings::default());
        }
        let text = std::fs::read_to_string(&sidecar).map_err(|e| e.to_string())?;
        parse(&text).map_err(|e| format!("{}: {e}", sidecar.display()))
    }

    /// Write the sidecar for `image` atomically (`.tmp` then rename).
    pub fn write(&self, image: &Path) -> Result<(), String> {
        let sidecar = sidecar_path(image).ok_or_else(|| "no ratings inside archives".to_string())?;
        let existing = std::fs::read_to_string(&sidecar).unwrap_or_default();
        let text = merge(&existing, self);
        let mut tmp = sidecar.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &sidecar).map_err(|e| e.to_string())?;
        Ok(())
    }
}
