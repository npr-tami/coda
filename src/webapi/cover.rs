//! Turn any picture into a playlist cover Spotify accepts: a square JPEG, at most 640x640, whose
//! base64 encoding fits in 256 KB.

use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};
use image::codecs::jpeg::JpegEncoder;
use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{DynamicImage, ImageDecoder, ImageError, ImageReader, Rgb, RgbImage};

/// Spotify's documented limit for the base64 body of `PUT /playlists/{id}/images`.
pub const MAX_COVER_BASE64_LEN: usize = 256 * 1024;
/// Coda aims a little lower so a different reading of "256 KB" can't reject the upload.
const TARGET_BASE64_LEN: usize = 256 * 1000;
const MAX_SIDE: u32 = 640;
/// Fallback sizes when even low quality doesn't fit (very noisy images).
const FALLBACK_SIDES: &[u32] = &[560, 480, 400, 320, 240, 160];
const QUALITIES: &[u8] = &[85, 80, 75, 70, 65, 60, 55, 50, 45, 40];
/// Refuse absurdly large files before trying to decode them.
const MAX_FILE_BYTES: u64 = 200 * 1024 * 1024;

/// Load an image file and produce JPEG bytes whose base64 encoding is <= 256 KB
/// (square-crops to the centre, max 640x640, lowers quality until it fits).
pub fn prepare_cover_jpeg(image_path: &Path) -> Result<Vec<u8>> {
    let image = load_cover_picture(image_path)?;
    let square = square_working_copy(&image)?;
    drop(image);
    fit_jpeg(&square)
}

/// Opens a picture the user picked for a cover (size checks, EXIF rotation applied), with the
/// same plain errors as an upload.
pub fn load_cover_picture(image_path: &Path) -> Result<DynamicImage> {
    let meta = std::fs::metadata(image_path).map_err(|e| {
        log::warn!("cover {}: {e}", image_path.display());
        anyhow!("coda couldn't open the picture \"{}\".", file_label(image_path))
    })?;
    if !meta.is_file() {
        bail!("\"{}\" isn't a picture file.", file_label(image_path));
    }
    if meta.len() > MAX_FILE_BYTES {
        bail!("That picture file is too large. Please choose one under 200 MB.");
    }
    decode(image_path)
}

/// The square the user chose in the crop step, as fractions of the picture, so it holds for the
/// small preview and the full picture alike: `left` of the width, `top` of the height, `size` of
/// the shorter side (1 = the largest square that fits).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CoverCrop {
    pub left: f32,
    pub top: f32,
    pub size: f32,
}

impl CoverCrop {
    /// The square in pixels of a `w` x `h` picture: (x, y, side), always inside it.
    pub fn pixels(self, w: u32, h: u32) -> (u32, u32, u32) {
        let short = w.min(h).max(1);
        let side = ((self.size.clamp(0.0, 1.0) * short as f32).round() as u32).clamp(1, short);
        let x = ((self.left.max(0.0) * w as f32).round() as u32).min(w.saturating_sub(side));
        let y = ((self.top.max(0.0) * h as f32).round() as u32).min(h.saturating_sub(side));
        (x, y, side)
    }
}

/// A small copy of the picture for the crop step: flattened onto white as the upload will be,
/// its longer side at most `max_side`.
pub fn cover_preview(image: &DynamicImage, max_side: u32) -> RgbImage {
    let (w, h) = (image.width(), image.height());
    if w.max(h) > max_side {
        flatten_to_rgb(image.thumbnail(max_side, max_side))
    } else {
        flatten_to_rgb(image.clone())
    }
}

/// The chosen square of the picture, flattened onto white and at most twice the final size, ready
/// to be saved and then uploaded (whose own centre crop then leaves it as it is).
pub fn crop_cover(image: &DynamicImage, crop: CoverCrop) -> Result<RgbImage> {
    if image.width() == 0 || image.height() == 0 {
        bail!("That picture is empty.");
    }
    let (x, y, side) = crop.pixels(image.width(), image.height());
    let rgb = flatten_to_rgb(image.crop_imm(x, y, side, side));
    let max_working = MAX_SIDE * 2;
    if side > max_working {
        Ok(imageops::resize(&rgb, max_working, max_working, FilterType::Lanczos3))
    } else {
        Ok(rgb)
    }
}

/// Base64 length (standard alphabet, padded) of `n` bytes.
pub fn base64_len(n: usize) -> usize {
    n.div_ceil(3) * 4
}

fn file_label(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string())
}

fn decode(path: &Path) -> Result<DynamicImage> {
    let reader = ImageReader::open(path)
        .with_context(|| format!("coda couldn't open the picture \"{}\".", file_label(path)))?
        .with_guessed_format()
        .with_context(|| format!("coda couldn't read the picture \"{}\".", file_label(path)))?;
    if reader.format().is_none() {
        bail!("That file isn't a picture coda can read. Please choose a JPEG, PNG or WebP image.");
    }
    let mut decoder = reader.into_decoder().map_err(|e| friendly_image_error(path, e))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).map_err(|e| friendly_image_error(path, e))?;
    // Phone photos are often stored sideways with an EXIF rotation flag.
    image.apply_orientation(orientation);
    Ok(image)
}

fn friendly_image_error(path: &Path, e: ImageError) -> anyhow::Error {
    log::warn!("cover {}: {e}", path.display());
    match e {
        ImageError::Unsupported(_) => {
            anyhow!("That picture format isn't supported. Please choose a JPEG, PNG or WebP image.")
        }
        ImageError::Limits(_) => anyhow!("That picture is too large to process. Please choose a smaller one."),
        ImageError::IoError(_) => anyhow!("coda couldn't read the picture \"{}\".", file_label(path)),
        _ => anyhow!("The picture \"{}\" seems to be damaged and can't be opened.", file_label(path)),
    }
}

/// Centre square crop, flattened onto white if transparent, pre-shrunk to at most 2x the final
/// size with a fast area filter so the high-quality resize stays cheap.
fn square_working_copy(image: &DynamicImage) -> Result<RgbImage> {
    let (w, h) = (image.width(), image.height());
    let side = w.min(h);
    if side == 0 {
        bail!("That picture is empty.");
    }
    let (x, y) = ((w - side) / 2, (h - side) / 2);
    let cropped = image.crop_imm(x, y, side, side);
    let rgb = flatten_to_rgb(cropped);
    let max_working = MAX_SIDE * 2;
    if side > max_working {
        Ok(imageops::thumbnail(&rgb, max_working, max_working))
    } else {
        Ok(rgb)
    }
}

fn flatten_to_rgb(image: DynamicImage) -> RgbImage {
    if !image.color().has_alpha() {
        return image.into_rgb8();
    }
    let rgba = image.into_rgba8();
    let mut out = RgbImage::new(rgba.width(), rgba.height());
    for (dst, src) in out.pixels_mut().zip(rgba.pixels()) {
        let a = u32::from(src[3]);
        let blend = |c: u8| ((u32::from(c) * a + 255 * (255 - a) + 127) / 255) as u8;
        *dst = Rgb([blend(src[0]), blend(src[1]), blend(src[2])]);
    }
    out
}

/// Encode at the largest size and best quality whose base64 fits.
fn fit_jpeg(square: &RgbImage) -> Result<Vec<u8>> {
    let first = square.width().min(MAX_SIDE);
    let sides = std::iter::once(first).chain(FALLBACK_SIDES.iter().copied().filter(|&s| s < first));
    for side in sides {
        let resized;
        let img = if side == square.width() {
            square
        } else {
            resized = imageops::resize(square, side, side, FilterType::Lanczos3);
            &resized
        };
        for &quality in QUALITIES {
            let jpeg = encode_jpeg(img, quality)?;
            let encoded = base64_len(jpeg.len());
            if encoded <= TARGET_BASE64_LEN {
                log::info!("cover prepared: {side}x{side}, quality {quality}, {encoded} bytes as base64");
                return Ok(jpeg);
            }
            log::debug!("cover {side}px q{quality}: {encoded} bytes base64, too large");
        }
    }
    bail!("coda couldn't make this picture small enough for Spotify (covers must be under 256 KB). Please try a different picture.")
}

fn encode_jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(128 * 1024);
    JpegEncoder::new_with_quality(&mut out, quality)
        .encode_image(img)
        .map_err(|e| {
            log::warn!("JPEG encoding failed: {e}");
            anyhow!("coda couldn't convert the picture to JPEG.")
        })?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::prelude::{BASE64_STANDARD, Engine as _};
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::path::PathBuf;

    struct TempFile(PathBuf);

    impl TempFile {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("coda-cover-test-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir.join(name))
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// Deterministic xorshift noise.
    fn noise(seed: &mut u64) -> u8 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        (*seed >> 24) as u8
    }

    fn check_cover(jpeg: &[u8], expected_side: u32) {
        assert!(base64_len(jpeg.len()) <= MAX_COVER_BASE64_LEN);
        assert_eq!(BASE64_STANDARD.encode(jpeg).len(), base64_len(jpeg.len()));
        let decoded = image::load_from_memory_with_format(jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (expected_side, expected_side));
    }

    #[test]
    fn base64_length_formula() {
        for n in 0..50 {
            assert_eq!(base64_len(n), BASE64_STANDARD.encode(vec![0u8; n]).len());
        }
    }

    #[test]
    fn large_noisy_image_still_fits() {
        let mut seed = 0x9E37_79B9_7F4A_7C15u64;
        let img = RgbImage::from_fn(1800, 1200, |_, _| Rgb([noise(&mut seed), noise(&mut seed), noise(&mut seed)]));
        let file = TempFile::new("noise.png");
        img.save_with_format(&file.0, ImageFormat::Png).unwrap();
        let jpeg = prepare_cover_jpeg(&file.0).unwrap();
        assert!(base64_len(jpeg.len()) <= MAX_COVER_BASE64_LEN);
        let decoded = image::load_from_memory_with_format(&jpeg, ImageFormat::Jpeg).unwrap();
        assert_eq!(decoded.width(), decoded.height());
        assert!(decoded.width() <= MAX_SIDE);
    }

    #[test]
    fn smooth_landscape_is_cropped_to_640_square() {
        let img = RgbImage::from_fn(2000, 1000, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
        let file = TempFile::new("landscape.jpg");
        img.save_with_format(&file.0, ImageFormat::Jpeg).unwrap();
        check_cover(&prepare_cover_jpeg(&file.0).unwrap(), 640);
    }

    #[test]
    fn crop_keeps_the_centre() {
        // Red left/right thirds, green centre: the square crop must be all green.
        let img = RgbImage::from_fn(300, 100, |x, _| if (100..200).contains(&x) { Rgb([0, 255, 0]) } else { Rgb([255, 0, 0]) });
        let square = square_working_copy(&DynamicImage::ImageRgb8(img)).unwrap();
        assert_eq!(square.dimensions(), (100, 100));
        assert!(square.pixels().all(|p| p[1] == 255 && p[0] == 0));
    }

    #[test]
    fn crop_pixels_stay_inside_the_picture() {
        let whole = CoverCrop { left: 0.0, top: 0.0, size: 1.0 };
        assert_eq!(whole.pixels(300, 100), (0, 0, 100));
        // Off the right edge: pulled back in.
        assert_eq!(CoverCrop { left: 0.9, top: 0.0, size: 1.0 }.pixels(300, 100), (200, 0, 100));
        // Half the short side, from the middle of a portrait picture.
        assert_eq!(CoverCrop { left: 0.25, top: 0.375, size: 0.5 }.pixels(200, 400), (50, 150, 100));
        // Nonsense values never give an empty or outside square.
        assert_eq!(CoverCrop { left: -1.0, top: 5.0, size: 0.0 }.pixels(10, 10), (0, 9, 1));
        assert_eq!(CoverCrop { left: 0.0, top: 0.0, size: 9.0 }.pixels(10, 20), (0, 0, 10));
    }

    #[test]
    fn crop_cover_takes_the_chosen_square() {
        // Green left third, red elsewhere: a crop at the left edge is all green.
        let img = RgbImage::from_fn(300, 100, |x, _| if x < 100 { Rgb([0, 255, 0]) } else { Rgb([255, 0, 0]) });
        let img = DynamicImage::ImageRgb8(img);
        let left = crop_cover(&img, CoverCrop { left: 0.0, top: 0.0, size: 1.0 }).unwrap();
        assert_eq!(left.dimensions(), (100, 100));
        assert!(left.pixels().all(|p| p[1] == 255 && p[0] == 0));
        // Zoomed in on the right end: all red, a quarter of the short side.
        let right = crop_cover(&img, CoverCrop { left: 0.9, top: 0.5, size: 0.25 }).unwrap();
        assert_eq!(right.dimensions(), (25, 25));
        assert!(right.pixels().all(|p| p[0] == 255 && p[1] == 0));
    }

    #[test]
    fn cropped_cover_uploads_unchanged() {
        // What the crop step saves goes through the usual preparation as the same square.
        let img = RgbImage::from_fn(900, 600, |x, y| Rgb([(x % 256) as u8, (y % 256) as u8, 40]));
        let square = crop_cover(&DynamicImage::ImageRgb8(img), CoverCrop { left: 0.5, top: 0.1, size: 0.6 }).unwrap();
        assert_eq!(square.dimensions(), (360, 360));
        let file = TempFile::new("cropped.png");
        square.save_with_format(&file.0, ImageFormat::Png).unwrap();
        check_cover(&prepare_cover_jpeg(&file.0).unwrap(), 360);
    }

    #[test]
    fn huge_crop_is_shrunk_and_preview_is_small() {
        let img = DynamicImage::ImageRgb8(RgbImage::from_pixel(3000, 2000, Rgb([10, 20, 30])));
        let square = crop_cover(&img, CoverCrop { left: 0.0, top: 0.0, size: 1.0 }).unwrap();
        assert_eq!(square.dimensions(), (MAX_SIDE * 2, MAX_SIDE * 2));
        let preview = cover_preview(&img, 800);
        assert_eq!(preview.width(), 800);
        assert!((533..=534).contains(&preview.height()), "{:?}", preview.dimensions());
    }

    #[test]
    fn small_image_is_not_upscaled() {
        let img = RgbImage::from_fn(120, 200, |x, y| Rgb([x as u8, y as u8, 7]));
        let file = TempFile::new("small.png");
        img.save_with_format(&file.0, ImageFormat::Png).unwrap();
        check_cover(&prepare_cover_jpeg(&file.0).unwrap(), 120);
    }

    #[test]
    fn transparent_png_is_flattened_onto_white() {
        let img = RgbaImage::from_pixel(64, 64, Rgba([0, 0, 0, 0]));
        let file = TempFile::new("transparent.png");
        img.save_with_format(&file.0, ImageFormat::Png).unwrap();
        let jpeg = prepare_cover_jpeg(&file.0).unwrap();
        check_cover(&jpeg, 64);
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert!(decoded.pixels().all(|p| p.0.iter().all(|&c| c > 240)));
    }

    #[test]
    fn non_images_give_a_plain_error() {
        let file = TempFile::new("notes.txt");
        std::fs::write(&file.0, b"definitely not a picture").unwrap();
        let err = prepare_cover_jpeg(&file.0).unwrap_err().to_string();
        assert!(err.contains("JPEG, PNG or WebP"), "{err}");
        let missing = prepare_cover_jpeg(Path::new("Z:/no/such/file.png")).unwrap_err().to_string();
        assert!(missing.contains("couldn't open"), "{missing}");
    }
}
