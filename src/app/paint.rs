//! Module `app_paint`, moved out of `main.rs` when it passed the
//! 1000-line target.
//!
//! `main.rs` is the binary's crate root: the imports, the type definitions,
//! `fn main` and the module list. The methods here are `pub(crate)` because a
//! method's visibility is scoped to the module that holds its `impl`; the
//! fields are `pub(crate)` for the same reason. See `AGENTS.md` and
//! `tests/source_size.rs`.

use crate::*;

/// One gallery-grid cell: thumbnail (or placeholder) + file name. Requests the
/// thumbnail when it is missing, then returns true if the cell was clicked.
///
/// Free function (not a method) so `gallery_grid` can hand out split borrows
/// and avoid cloning the whole file list every frame.
pub(crate) fn grid_cell(
    ui: &mut egui::Ui,
    path: &Path,
    cell: f32,
    selected: bool,
    clock: u64,
    thumbs: &mut HashMap<PathBuf, Thumb>,
    tries: &mut HashMap<PathBuf, u8>,
    loader: &Loader,
    ratings: &HashMap<PathBuf, image_viewer::Ratings>,
) -> egui::Response {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

    // Ask for the thumbnail whenever it is missing. `Loader::push` dedups
    // against queued + in-flight work, so this costs one lock per visible cell
    // per frame, and it self-heals after `schedule()` wipes the queue.
    // `tries` counts *completed* attempts, so jobs lost to a queue wipe cost
    // nothing; only real decode failures count against the budget.
    // **No thumbnail job for an archive, ever.** An archive has no pixels to
    // decode; asking produces a failure that spends one of `THUMB_TRIES`' three
    // attempts, and a cell that can never draw. The cell is drawn from the
    // member count instead, which is a header read.
    let is_archive = is_archive_path(path);
    if !is_archive
        && !thumbs.contains_key(path)
        && tries.get(path).copied().unwrap_or(0) < THUMB_TRIES
    {
        loader.push(Job {
            // A grid cell is one picture, so it is always the file's first page.
            key: ImgKey::first(path),
            kind: Kind::Thumb,
            prio: PRIO_THUMB,
            target_dim: THUMB_DIM,
        });
    }
    if let Some(t) = thumbs.get_mut(path) {
        t.used = clock; // LRU stamp
    }

    let fill = if selected {
        egui::Color32::from_rgb(28, 58, 118)
    } else {
        egui::Color32::from_rgb(38, 38, 38)
    };
    let stroke = if selected {
        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(120, 170, 255))
    } else {
        egui::Stroke::NONE
    };

    // Allocate the cell *ourselves* with click sense. `Frame::show` would
    // size itself to the rest of the row and only register hover, which both
    // pushed the following cells offscreen and made clicks dead.
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(cell, cell + GRID_LABEL_H), egui::Sense::click());
    if ui.is_rect_visible(rect) {
        ui.painter().rect(rect, 0.0, fill, stroke, egui::StrokeKind::Inside);
    }

    let inner = rect.shrink(4.0);
    // `Ui::child_ui(rect, layout)` (0.27) is gone. The migration target is
    // `Ui::new_child(UiBuilder)`, NOT `Ui::scope_builder(..)`: both create a Ui
    // bounded to `inner`, but `scope_builder` additionally calls
    // `advance_cursor_after_rect` on *this* `ui` afterwards. That is invisible
    // for a scope that owns the parent's space, and wrong here — the cell was
    // already allocated above, so the extra advance doubled up and ate 4pt of
    // `item_spacing` per cell (cells visibly tightened from 8pt to 4pt).
    // `new_child` returns an owned `Ui` and leaves the parent cursor alone,
    // which is exactly what 0.27's `child_ui` did.
    let mut child = ui.new_child(
        egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Center)),
    );

    // Thumbnail area: full width, cell height. Only the *rect* is computed
    // here; egui still does the painting. Widgets place at the cursor, and a
    // `Layout::top_down(Align::Center)` does not move them (a top-down flow's
    // initial cursor runs to +infinity on the main axis, so
    // `allocate_exact_size` only centres on the cross axis), hence the
    // explicit rect.
    let img_area = egui::Rect::from_min_size(
        egui::Pos2::new(inner.min.x, inner.min.y),
        egui::vec2(inner.width(), cell),
    );
    child.allocate_exact_size(img_area.size(), egui::Sense::hover());
    match thumbs.get(path) {
        Some(t) if t.texels[0] > 0 && t.texels[1] > 0 => {
            // Texels -> points, so a 256px thumb looks the same size whatever
            // the display's pixel ratio. `texels` is the decoded buffer's size,
            // recorded at upload time: the ground truth, with no dependency on
            // texture-manager state.
            let ppp = ui.ctx().pixels_per_point();
            let natural = egui::vec2(t.texels[0] as f32, t.texels[1] as f32) / ppp;
            let scale =
                fit_scale_no_upscale((natural.x, natural.y), (img_area.width(), img_area.height()));
            let draw_size = (natural * scale).max(egui::Vec2::splat(1.0)).min(img_area.size());
            let draw = egui::Rect::from_center_size(img_area.center(), draw_size);
            if ui.is_rect_visible(draw) {
                // Give the widget a Ui bounded to the centred rect: that is
                // what centres it. Do NOT paint the texture by hand here
                // (`painter().image`) — that shipped once and turned every
                // thumbnail into a solid block.
                let mut img_ui = child.new_child(
                    egui::UiBuilder::new()
                        .max_rect(draw)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                img_ui.add(egui::Image::new(&t.tex).max_size(draw_size));
            }
        }
        _ => {
            // An archive's cell draws its **member count** where a thumbnail
            // would go, because that is the one fact that tells the reader what
            // arrowing onto it will do. The count is a header read; the cell is
            // the reason it is allowed to be.
            //
            // `None` (unreadable container) says so rather than showing a
            // number, and the ellipsis is left for a cell that is genuinely
            // still decoding — which an archive never is, so a reader can tell
            // the two apart without being told.
            //
            // A cell whose thumbnail failed all `THUMB_TRIES` attempts gets the
            // same "?" as an unreadable container, for the same reason: its
            // ellipsis stopped meaning "decoding" three attempts ago, and a
            // mark that cannot resolve reads as a hang. `?` is already the
            // "cannot show this" mark, so no new glyph is introduced — and it
            // has to be ASCII, because the default font renders anything else
            // as a blank box.
            let text = if is_archive {
                match archive_image_count(path) {
                    Some(n) => format!("{n} images"),
                    None => "?".to_string(),
                }
            } else if tries.get(path).copied().unwrap_or(0) >= THUMB_TRIES {
                "?".to_string()
            } else {
                "\u{2026}".to_string()
            };
            // Centred like a real thumbnail would be.
            child.painter().text(
                img_area.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(if is_archive { 11.0 } else { 16.0 }),
                ui.visuals().weak_text_color(),
            );
        }
    }
    child.add(
        egui::Label::new(
            // Stars in the label strip, read from the map the grid pass
            // filled — never a sidecar read here, which would be a filesystem
            // read per cell per frame. `*` and `!` are ASCII, so no glyph can
            // be missing from the default font.
            egui::RichText::new(&{
                let mut label = name.clone();
                if let Some(r) = ratings.get(path) {
                    if r.rating.flag {
                        label = format!("! {label}");
                    }
                    if r.rating.stars > 0 {
                        label.push(' ');
                        label.push_str(&"*".repeat(r.rating.stars.min(5) as usize));
                    }
                }
                label
            })
            .small()
            .color(egui::Color32::from_gray(200)),
        )
        .truncate(),
    );

    resp
}

/// A small 2x2-square checker texture used as the transparency backdrop.
/// Painted with repeat-wrapped UVs so it tiles at any image size.
/// The file's **stored** pixel dimensions, recovered from a preview's
/// already-oriented `orig`.
///
/// Three places need this — drawing a frame, reading one back out, and moving
/// one — and they all have to agree about which space a rectangle is in.
/// Repeating the derivation three times is three chances to crop a valid picture
/// of the wrong part of the photo, which is exactly the failure this codebase
/// has already paid for once. `Xform::from_exif` is the same source of truth the
/// decode used, rather than a second opinion about which tags swap axes.
pub(crate) fn stored_dims(pv: &Preview) -> (u32, u32) {
    if Xform::from_exif(pv.exif_tag).unwrap_or(Xform::Identity).swaps_axes() {
        (pv.orig.1, pv.orig.0)
    } else {
        pv.orig
    }
}

pub(crate) fn make_checker_texture(ctx: &Context) -> TextureHandle {
    const N: usize = 64; // texture side in px
    const SQ: usize = 32; // square side in px
    let mut pixels = Vec::with_capacity(N * N);
    for y in 0..N {
        for x in 0..N {
            let even = ((x / SQ) + (y / SQ)) % 2 == 0;
            let g = if even { 82 } else { 118 };
            pixels.push(Color32::from_rgb(g, g, g));
        }
    }
    ctx.load_texture(
        "checker",
        ColorImage { size: [N, N], source_size: egui::vec2(N as f32, N as f32), pixels },
        TextureOptions::NEAREST_REPEAT,
    )
}

/// Put RGBA pixels on the Windows clipboard, from any thread.
///
/// Takes the buffer **by value** so arboard can take ownership of it. A 24 MP
/// photo is 96 MB, and cloning it to hand it to a function that was going to
/// clone it again is not a detail: this is the one call on this path that is
/// unavoidably a copy, and the point is that it happens once.
pub(crate) fn set_clipboard_rgba(width: u32, height: u32, rgba: Vec<u8>) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_image(arboard::ImageData {
        width: width as usize,
        height: height as usize,
        bytes: std::borrow::Cow::Owned(rgba),
    })
    .map_err(|e| e.to_string())
}

/// What went on the clipboard, said in the terms the user acted in.
///
/// A crop or a turn is worth naming in the confirmation. "Copied 6000x4000 image
/// to clipboard" when the user framed a quarter of the photo is true and useless;
/// the size alone does not say which quarter.
pub(crate) fn copied_notice(w: u32, h: u32, edit: &image_viewer::Edit) -> String {
    if edit.is_identity() {
        format!("Copied {w}x{h} image to clipboard")
    } else {
        format!("Copied {w}x{h} to clipboard ({})", edit.label())
    }
}

/// Paint a texture into `rect`, turned by `xform` about the rect's centre.
///
/// `painter.image` cannot rotate, so build a mesh and move its vertices — the same
/// trick egui's own `Image::paint_at` uses for `ImageOptions::rotation`. A
/// quarter turn keeps everything axis-aligned, so 1:1 stays pixel-sharp and
/// the caller can keep working in unrotated image space.
///
/// The matrix comes from [`Xform::paint_matrix`], i.e. it is derived from the same
/// `Xform::map` that decides what the *saved* file contains. That is the point:
/// a hand-written rotation matrix here is a second, independent statement of what
/// the transform is, and the day the two disagree the screen shows one picture and
/// the saved file contains another, with nothing to notice. Going through `map`
/// makes that impossible.
///
/// `Mesh::transform` is no use — `TSTransform` is a scale and a translation, with
/// no room for a rotation or a reflection. The vertices are moved directly, which
/// is what `Mesh::rotate` does internally anyway.
pub(crate) fn paint_tex(
    painter: &egui::Painter,
    tex_id: egui::TextureId,
    rect: egui::Rect,
    uv: egui::Rect,
    xform: Xform,
) {
    if xform == Xform::Identity {
        painter.image(tex_id, rect, uv, Color32::WHITE);
        return;
    }
    let m = xform.paint_matrix();
    let origin = rect.center();
    let mut mesh = egui::epaint::Mesh::with_texture(tex_id);
    mesh.add_rect_with_uv(rect, uv, Color32::WHITE);
    for v in &mut mesh.vertices {
        let d = v.pos - origin;
        v.pos = origin + Vec2::new(m[0][0] * d.x + m[0][1] * d.y, m[1][0] * d.x + m[1][1] * d.y);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// `TextureOptions` for a viewer content texture — the preview, the zoom
/// tiles, the animation frames: linear until Pixel art is on, then
/// nearest-on-magnify, the binary's spelling of the decision
/// [`image_viewer::pixel_art_sampling`] returns. The choice names the two
/// sampler axes, so a picture magnified by zooming past 100% (or simply by
/// being a sprite fitted on the panel) samples nearest, and the same texture
/// minified by fitting a photo samples linear — one upload serves both.
///
/// Deliberately **not** handed to the grid thumbnails: a cell is 150 pt of a
/// 256-texel image and the grid also browses photos, nearest-at-anything
/// buys a photo nothing there. The checkerboard keeps `NEAREST_REPEAT` for
/// the same class of reason: it is artwork, not content.
pub(crate) fn content_texture_options(pixel_art: bool) -> TextureOptions {
    match image_viewer::pixel_art_sampling(pixel_art) {
        image_viewer::Sampling::Linear => TextureOptions::LINEAR,
        image_viewer::Sampling::NearestMagnify => TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            wrap_mode: egui::TextureWrapMode::ClampToEdge,
            mipmap_mode: None,
        },
    }
}

/// Copy one tile out of the full-res RGBA buffer and upload it. Tiles are cut
/// only where the on-screen density outruns the preview's — the tile is
/// itself magnified where it is drawn — so `options` is decided once per
/// frame by the caller and carries whatever the pixel-art toggle asked for.
pub(crate) fn make_tile(
    ctx: &Context,
    data: &ImageData,
    tx: u32,
    ty: u32,
    options: TextureOptions,
) -> TextureHandle {
    let (fw, fh) = (data.width as usize, data.height as usize);
    let (x0, y0) = ((tx * TILE) as usize, (ty * TILE) as usize);
    let (w, h) = ((TILE as usize).min(fw - x0), (TILE as usize).min(fh - y0));
    let stride = fw * 4;

    let mut pixels: Vec<Color32> = Vec::with_capacity(w * h);
    for y in y0..y0 + h {
        let row = &data.rgba[y * stride + x0 * 4..y * stride + (x0 + w) * 4];
        pixels.extend(
            row.chunks_exact(4).map(|p| Color32::from_rgba_unmultiplied(p[0], p[1], p[2], p[3])),
        );
    }
    let img = ColorImage { size: [w, h], source_size: egui::vec2(w as f32, h as f32), pixels };
    ctx.load_texture(format!("tile {tx},{ty}"), img, options)
}

// ---------------------------------------------------------------------------
// eframe glue
// ---------------------------------------------------------------------------
