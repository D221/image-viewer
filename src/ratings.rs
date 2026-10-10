//! Ratings, flags and tags, stored as XMP sidecars.
//!
//! The sidecar for `photo.jpg` is `photo.jpg.xmp` (appended, not
//! `photo.jpg` with its extension replaced), so two files sharing a stem
//! keep separate sidecars. There are no ratings inside archives:
//! [`sidecar_path`] returns `None` there.
//!
//! The load-bearing property of this whole feature: **a round trip preserves
//! every byte it did not write.** The [`merge`] below is the minimal version
//! Task 2 replaces with a surgical editor: it emits a whole sidecar and so
//! must only ever be called when there is nothing worth preserving yet.

use std::path::{Path, PathBuf};

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

/// Build the sidecar text for `ratings`, ignoring `existing`.
///
/// Minimal whole-file emitter: Task 2 replaces this with a surgical editor
/// that preserves bytes it did not write. Until then this is only correct
/// for sidecars this function itself wrote.
fn merge(ratings: &Ratings, _existing: &str) -> String {
    let mut out = String::from(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n \
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  \
         <rdf:Description rdf:about=\"\"\n   \
         xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\"\n   \
         xmlns:dc=\"http://purl.org/dc/elements/1.1/\"",
    );
    out.push_str(&format!("\n   xmp:Rating=\"{}\"", ratings.rating.clamped_stars()));
    if ratings.rating.flag {
        out.push_str("\n   xmp:Label=\"Pick\"");
    }
    out.push_str(">\n   <dc:subject>\n    <rdf:Bag>\n");
    for tag in &ratings.tags {
        out.push_str("     <rdf:li>");
        out.push_str(&escape(tag));
        out.push_str("</rdf:li>\n");
    }
    out.push_str("    </rdf:Bag>\n   </dc:subject>\n  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n");
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

/// Parse sidecar text produced by [`merge`].
fn parse(text: &str) -> Result<Ratings, String> {
    let mut ratings = Ratings::default();
    if let Some(v) = attr(text, "xmp:Rating") {
        let stars: u8 = v.trim().parse().map_err(|_| format!("bad xmp:Rating {v:?}"))?;
        ratings.rating.stars = stars.min(5);
    }
    if let Some(label) = attr(text, "xmp:Label") {
        ratings.rating.flag = label == "Pick";
    }
    let mut rest = text;
    while let Some(start) = rest.find("<rdf:li>") {
        let after = &rest[start + "<rdf:li>".len()..];
        if let Some(end) = after.find("</rdf:li>") {
            ratings.tags.push(unescape(after[..end].trim()));
            rest = &after[end + "</rdf:li>".len()..];
        } else {
            break;
        }
    }
    Ok(ratings)
}

/// The value of `name="..."` in `text`, if present.
fn attr<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let needle = format!("{name}=\"");
    let start = text.find(&needle)? + needle.len();
    let end = text[start..].find('"')?;
    Some(&text[start..start + end])
}

impl Ratings {
    /// Read the sidecar for `image`; an absent sidecar is all defaults.
    pub fn read(image: &Path) -> Result<Ratings, String> {
        let sidecar = sidecar_path(image).ok_or_else(|| "no ratings inside archives".to_string())?;
        if !sidecar.exists() {
            return Ok(Ratings::default());
        }
        let text = std::fs::read_to_string(&sidecar).map_err(|e| e.to_string())?;
        parse(&text)
    }

    /// Write the sidecar for `image` atomically (`.tmp` then rename).
    pub fn write(&self, image: &Path) -> Result<(), String> {
        let sidecar = sidecar_path(image).ok_or_else(|| "no ratings inside archives".to_string())?;
        let existing = std::fs::read_to_string(&sidecar).unwrap_or_default();
        let text = merge(self, &existing);
        let mut tmp = sidecar.as_os_str().to_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        std::fs::write(&tmp, text.as_bytes()).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &sidecar).map_err(|e| e.to_string())?;
        Ok(())
    }
}
