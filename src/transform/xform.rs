//! Module `xform`, moved out of `lib.rs` when it passed the
//! 1000-line target.
//!
//! The crate root re-exports everything here (`pub use xform::*;`),
//! so `image_viewer::name` keeps working as it did. See `AGENTS.md` and
//! `tests/source_size.rs`.

// ---------------------------------------------------------------------------
// Lossless transform: the algebra of the eight lattice symmetries
// ---------------------------------------------------------------------------

/// One of the eight square-lattice symmetries — the complete set that both an
/// EXIF orientation tag and a lossless JPEG transform can express.
///
/// This exists because "rotate the saved file to match the screen" is a
/// *composition* of two of them: the EXIF tag the camera wrote, and the quarter
/// turns the user has since pressed. Composing them by hand is how you get a
/// save that is mirrored, or transposed, or applied twice.
///
/// A value maps output pixels back to input pixels, so composing is
/// substitution: where does the final output pixel come from, through the
/// second transform and then the first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Xform {
    #[default]
    Identity,
    /// 90 degrees clockwise.
    Rot90,
    Rot180,
    /// 90 degrees counter-clockwise.
    Rot270,
    FlipH,
    FlipV,
    /// Mirror along the top-left to bottom-right axis.
    Transpose,
    /// Mirror along the top-right to bottom-left axis.
    Transverse,
}

impl Xform {
    pub const ALL: [Xform; 8] = [
        Xform::Identity,
        Xform::Rot90,
        Xform::Rot180,
        Xform::Rot270,
        Xform::FlipH,
        Xform::FlipV,
        Xform::Transpose,
        Xform::Transverse,
    ];

    /// Do the axes swap? True for everything that turns the picture on its side.
    pub fn swaps_axes(self) -> bool {
        matches!(self, Xform::Rot90 | Xform::Rot270 | Xform::Transpose | Xform::Transverse)
    }

    /// Dimensions of the result, given the input.
    pub fn out_dims(self, w: u32, h: u32) -> (u32, u32) {
        if self.swaps_axes() {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Which input pixel ends up at output pixel `(ox, oy)`.
    pub fn map(self, ox: u32, oy: u32, w: u32, h: u32) -> (u32, u32) {
        match self {
            Xform::Identity => (ox, oy),
            // Output is h x w. A CW turn takes the input's left edge to the top.
            Xform::Rot90 => (oy, h - 1 - ox),
            Xform::Rot180 => (w - 1 - ox, h - 1 - oy),
            Xform::Rot270 => (w - 1 - oy, ox),
            Xform::FlipH => (w - 1 - ox, oy),
            Xform::FlipV => (ox, h - 1 - oy),
            Xform::Transpose => (oy, ox),
            // Anti-diagonal mirror. The output is h x w, so `ox` ranges over
            // [0, h) and indexes the *height*: getting this pair the wrong way
            // round underflows as soon as the image is wider than it is tall.
            Xform::Transverse => (w - 1 - oy, h - 1 - ox),
        }
    }

    /// The linear part of the **input -> output** map, row-major, for painting.
    ///
    /// [`Self::map`] goes the other way (output pixel -> input pixel), which is
    /// what a sampler wants; drawing a texture needs the opposite, and taking the
    /// transpose is the whole of the difference. Every one of the eight is a
    /// *signed permutation* — entries in `{0, +-1}` with one per row and column —
    /// so the inverse is the transpose, and no division or inversion is involved.
    ///
    /// Derived by probing `map` and transposing, for the same reason `then` is:
    /// transcribing this by hand is eight matrices of sign errors, and each sign
    /// error produces a picture that looks plausible. Because it comes from `map`,
    /// the screen and the saved file cannot disagree about what the transform is —
    /// `the_paint_matrix_reproduces_the_old_quarter_turn_behaviour` pins the four
    /// rotations against the `Rot2` this replaced.
    pub fn paint_matrix(self) -> [[f32; 2]; 2] {
        // Any dimensions work: the differences are taken before the axes matter.
        // Two pixels of slack in each axis is all `map` needs.
        let (w, h) = (9u32, 5u32);
        let p0 = self.map(0, 0, w, h);
        let px = self.map(1, 0, w, h);
        let py = self.map(0, 1, w, h);
        // `map` is output -> input, so these two differences are the *columns* of
        // that matrix.
        let cx = (px.0 as i64 - p0.0 as i64, px.1 as i64 - p0.1 as i64);
        let cy = (py.0 as i64 - p0.0 as i64, py.1 as i64 - p0.1 as i64);
        let a = [
            [cx.0 as f32, cy.0 as f32], // row 0
            [cx.1 as f32, cy.1 as f32], // row 1
        ];
        // Transposed: input -> output.
        [[a[0][0], a[1][0]], [a[0][1], a[1][1]]]
    }

    /// A human-readable name, for the save confirmation and the status readout.
    pub fn label(self) -> &'static str {
        match self {
            Xform::Identity => "no transform",
            Xform::Rot90 => "rotated 90° CW",
            Xform::Rot180 => "rotated 180°",
            Xform::Rot270 => "rotated 270° CW",
            Xform::FlipH => "mirrored left-right",
            Xform::FlipV => "mirrored top-bottom",
            Xform::Transpose => "transposed",
            Xform::Transverse => "transversed",
        }
    }

    /// The transform an EXIF orientation tag describes, or `None` for 0 and for
    /// tags outside 1-=8.
    ///
    /// The mapping is not invented here: it is checked against
    /// [`apply_exif_orientation`] by a test, so the two cannot drift.
    pub fn from_exif(tag: u16) -> Option<Xform> {
        Some(match tag {
            1 => Xform::Identity,
            2 => Xform::FlipH,
            3 => Xform::Rot180,
            4 => Xform::FlipV,
            5 => Xform::Transpose,
            6 => Xform::Rot90,
            7 => Xform::Transverse,
            8 => Xform::Rot270,
            _ => return None,
        })
    }

    /// `turns` quarter turns clockwise — what the rotate buttons produce.
    pub fn quarters_cw(turns: u8) -> Xform {
        match turns & 3 {
            0 => Xform::Identity,
            1 => Xform::Rot90,
            2 => Xform::Rot180,
            _ => Xform::Rot270,
        }
    }

    /// `self` first, then `next`.
    ///
    /// Found by *probing* rather than by transcribing a multiplication table: the
    /// eight ops form a group, so the composition is whichever single op maps the
    /// output to the same input pixels. Working this out structurally is four
    /// pages of sign analysis and gets it wrong in a way that only shows up as a
    /// mirrored saved photo; this is twenty lines and a test proves it.
    pub fn then(self, next: Xform) -> Xform {
        // Deliberately not square, so a transpose cannot masquerade as a
        // rotation: both preserve an area but swap the axes, and only the
        // corners tell them apart on a square.
        let (iw, ih) = (7u32, 5u32);
        let (mw, mh) = self.out_dims(iw, ih); // `self`'s output = `next`'s input
        let (ow, oh) = next.out_dims(mw, mh); // the composition's output
                                              // `map` takes the *input* dimensions, which is the easy thing to get
                                              // wrong here and produces underflow rather than a wrong answer.
        let probe = |ox: u32, oy: u32| {
            let (bx, by) = next.map(ox, oy, mw, mh);
            self.map(bx, by, iw, ih)
        };
        let mut found = None;
        for cand in Xform::ALL {
            if cand.out_dims(iw, ih) != (ow, oh) {
                continue;
            }
            if cand.map(0, 0, iw, ih) == probe(0, 0)
                && cand.map(ow - 1, oh - 1, iw, ih) == probe(ow - 1, oh - 1)
                && cand.map(0, oh - 1, iw, ih) == probe(0, oh - 1)
                && cand.map(ow - 1, 0, iw, ih) == probe(ow - 1, 0)
            {
                debug_assert!(found.is_none(), "two transforms match one composition");
                found = Some(cand);
            }
        }
        found.expect("the eight transforms are closed under composition")
    }

    /// The transform that undoes `self`.
    ///
    /// Needed because a crop is chosen on the **displayed** picture and applied to
    /// the **stored** one, so getting from one to the other is an inverse. Each of
    /// the eight is a bijection, so an inverse exists and is one of the eight; it
    /// is found by probing, exactly as [`Self::then`] composes, rather than
    /// transcribed. A sign error here does not corrupt the image — it crops the
    /// wrong region, which is a perfectly good picture of the wrong part of the
    /// photo.
    pub fn inverse(self) -> Xform {
        Xform::ALL
            .into_iter()
            .find(|cand| cand.then(self) == Xform::Identity)
            .expect("the eight transforms are closed under inversion")
    }

    /// The same operation as a TurboJPEG lossless transform.
    pub fn as_turbojpeg(self) -> turbojpeg::TransformOp {
        match self {
            Xform::Identity => turbojpeg::TransformOp::None,
            Xform::Rot90 => turbojpeg::TransformOp::Rot90,
            Xform::Rot180 => turbojpeg::TransformOp::Rot180,
            Xform::Rot270 => turbojpeg::TransformOp::Rot270,
            Xform::FlipH => turbojpeg::TransformOp::Hflip,
            Xform::FlipV => turbojpeg::TransformOp::Vflip,
            Xform::Transpose => turbojpeg::TransformOp::Transpose,
            Xform::Transverse => turbojpeg::TransformOp::Transverse,
        }
    }

    /// Apply to an RGBA buffer. Only for the formats that have to be re-encoded.
    pub fn apply_rgba(self, w: u32, h: u32, rgba: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
        let (ow, oh) = self.out_dims(w, h);
        let want_in = (w as usize) * (h as usize) * 4;
        let want_out = (ow as usize) * (oh as usize) * 4;
        if rgba.len() != want_in {
            return None;
        }
        let mut out = vec![0u8; want_out];
        for oy in 0..oh {
            for ox in 0..ow {
                let (ix, iy) = self.map(ox, oy, w, h);
                let src = ((iy as usize) * (w as usize) + ix as usize) * 4;
                let dst = ((oy as usize) * (ow as usize) + ox as usize) * 4;
                out[dst..dst + 4].copy_from_slice(&rgba[src..src + 4]);
            }
        }
        Some((ow, oh, out))
    }
}
