//! A minimal ISO-BMFF / HEIF container, assembled box by box.
//!
//! Nothing in this dependency set can **write** a HEIF — `heic` 0.1.6 decodes
//! and exposes no encoder, and no other crate here emits ISO-BMFF either — so the
//! interesting half of `colr` handling (a profile that classifies, versus `nclx`
//! parameters that do not) has no fixture unless one is built by hand. This is the
//! same situation as the APNG fixture in `tests/multiframe.rs`, for the same
//! reason: the crate we test with cannot produce the file.
//!
//! It is small because the reader that matters only walks the **item property**
//! tree. `DecoderConfig::extract_icc` parses the container, finds the primary
//! item and reads its `colr` box; it never resolves pixel data. So there is no
//! image payload here, no `iloc` extents and no HEVC parameter set — none of them
//! are on the path to the colour box, and adding them would make the fixture
//! harder to read for no extra coverage.
//!
//! Layouts are version 0/2 throughout, which is what the parser expects:
//! `pitm` and `infe` carry 2-byte item ids, `ipma` uses 8-bit property indices,
//! and `infe` version 2 puts the item type after the protection index.

/// One `colr` box: an ICC payload, or `nclx` parameters.
pub enum Colour<'a> {
    /// `colr` with colour_type `prof`: the profile is the rest of the box.
    Icc(&'a [u8]),
    /// `colr` with colour_type `nclx`: primaries, transfer and matrix, which is
    /// how a file states its colour with no profile at all.
    Nclx { primaries: u16, transfer: u16, matrix: u16 },
}

/// A HEIF whose primary item declares its colour with an ICC profile.
///
/// `Display P3` is (12, 13, 0) in `nclx` terms, which is what the sibling
/// builder below emits — so the two fixtures differ only in the colour box and a
/// test can say "same file, profile instead of parameters" and mean it.
pub fn heif_with_icc(icc: &[u8]) -> Vec<u8> {
    heif(Colour::Icc(icc))
}

/// A HEIF whose primary item states Display P3 with `nclx` parameters and no
/// profile: colour_primaries 12 (Display P3), transfer 13 (sRGB), matrix 0 (RGB
/// identity). This is the shape a phone HEIC takes, and the reason `None` must
/// not be read as "sRGB".
pub fn heif_with_nclx_display_p3() -> Vec<u8> {
    heif(Colour::Nclx { primaries: 12, transfer: 13, matrix: 0 })
}

fn heif(colour: Colour<'_>) -> Vec<u8> {
    // ftyp: major brand heic, minor 0, compatible with heic and mif1.
    let mut ftyp = Vec::new();
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"heic");
    ftyp.extend_from_slice(b"mif1");

    // hdlr: handler type 'pict'. A decoder wants to see this before anything.
    let mut hdlr = vec![0u8; 4]; // pre_defined
    hdlr.extend_from_slice(b"pict");
    hdlr.extend_from_slice(&[0u8; 12]); // reserved
    hdlr.push(0); // empty name, NUL-terminated

    // pitm: the primary item is id 1.
    let pitm = 1u16.to_be_bytes().to_vec();

    // iinf: one entry, then the infe for it. `full_box` writes the version and
    // flags, so these payloads start at the item id.
    let mut infe = 1u16.to_be_bytes().to_vec(); // item_ID
    infe.extend_from_slice(&0u16.to_be_bytes()); // item_protection_index
    infe.extend_from_slice(b"hvc1"); // item_type
    infe.push(0); // empty item_name
    let mut iinf = 1u16.to_be_bytes().to_vec(); // entry_count
    iinf.extend_from_slice(&full_box(b"infe", 2, 0, &infe));

    // iprp -> ipco: the colour box. It is the only property, so it is index 1.
    let colr = match colour {
        Colour::Icc(icc) => {
            let mut c = b"prof".to_vec();
            c.extend_from_slice(icc);
            c
        }
        Colour::Nclx { primaries, transfer, matrix } => {
            let mut c = b"nclx".to_vec();
            c.extend_from_slice(&primaries.to_be_bytes());
            c.extend_from_slice(&transfer.to_be_bytes());
            c.extend_from_slice(&matrix.to_be_bytes());
            c.push(0); // full_range flag
            c
        }
    };
    let ipco = box_(b"ipco", &box_(b"colr", &colr));

    // ipma: item 1 has one association, to property 1. Version 0 with flags 0
    // means 8-bit property indices and a 2-byte item id.
    let mut ipma = 1u32.to_be_bytes().to_vec(); // entry_count
    ipma.extend_from_slice(&1u16.to_be_bytes()); // item_ID
    ipma.push(1); // association_count
    ipma.push(1); // essential = 0, property index = 1
    let iprp =
        box_(b"iprp", &[ipco.as_slice(), full_box(b"ipma", 0, 0, &ipma).as_slice()].concat());

    let mut meta = Vec::new();
    meta.extend_from_slice(&full_box(b"hdlr", 0, 0, &hdlr));
    meta.extend_from_slice(&full_box(b"pitm", 0, 0, &pitm));
    meta.extend_from_slice(&full_box(b"iinf", 0, 0, &iinf));
    meta.extend_from_slice(&iprp);

    let mut out = box_(b"ftyp", &ftyp);
    out.extend_from_slice(&full_box(b"meta", 0, 0, &meta));
    out
}

/// A box: 4-byte size, 4-byte type, content.
fn box_(kind: &[u8; 4], content: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(8 + content.len());
    v.extend_from_slice(&((content.len() + 8) as u32).to_be_bytes());
    v.extend_from_slice(kind);
    v.extend_from_slice(content);
    v
}

/// A full box: version and flags before the content.
fn full_box(kind: &[u8; 4], version: u8, flags: u32, content: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(4 + content.len());
    c.push(version);
    c.extend_from_slice(&flags.to_be_bytes()[1..]);
    c.extend_from_slice(content);
    box_(kind, &c)
}
