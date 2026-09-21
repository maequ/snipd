//! Polygon masking for freeform (lasso) captures.
//!
//! A freeform capture is a rectangular crop of the frozen frame with everything
//! outside the drawn shape made transparent. Two details matter:
//!
//! **Speed.** The obvious implementation — test every pixel against every edge —
//! is O(pixels x edges). A lasso drawn at mouse-move resolution easily carries
//! several hundred points, and a selection can be a million pixels, so that is
//! hundreds of millions of operations while the user waits. This uses a scanline
//! fill instead, which is O(rows x edges): a couple of orders of magnitude less
//! work for the same result.
//!
//! **Edges.** A hard binary in/out test leaves visibly jagged edges on the
//! diagonal and curved strokes that freeform selection is entirely made of. Each
//! pixel row is therefore sampled at several sub-row heights and spans
//! contribute fractional coverage at their ends, so the alpha channel comes out
//! antialiased.

use image::RgbaImage;

use super::win::{Bounds, CaptureError, Frame};

/// A point in virtual-screen coordinates, as sent by the overlay.
#[derive(Debug, Clone, Copy, serde::Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Sub-row samples per pixel row. Four is enough to remove the visible
/// stair-stepping on a hand-drawn curve without meaningfully costing anything.
const SUBSAMPLES: usize = 4;

/// Fewer than three points cannot enclose an area.
const MIN_POINTS: usize = 3;

/// Crop `frame` to the polygon's bounding box and clear alpha outside the shape.
///
/// `points` are in virtual-screen coordinates. The polygon is implicitly closed:
/// the last point connects back to the first.
pub fn apply_polygon(frame: &Frame, points: &[Point]) -> Result<Frame, CaptureError> {
    if points.len() < MIN_POINTS {
        return Err(CaptureError::EmptyArea);
    }

    let bounds = polygon_bounds(points)?;
    let cropped = frame.crop(bounds)?;

    // Re-read the crop's actual origin and size: `Frame::crop` clips to the
    // frame, so a lasso drawn partly off-screen yields something smaller than
    // the polygon's own bounding box.
    let origin = cropped.origin;
    let (width, height) = cropped.image.dimensions();

    let coverage = rasterise(points, origin, width, height);
    let mut image = cropped.image;
    apply_coverage(&mut image, &coverage);

    Ok(Frame { image, origin })
}

/// Bounding box of the polygon, rounded outward so no drawn pixel is clipped.
fn polygon_bounds(points: &[Point]) -> Result<Bounds, CaptureError> {
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;

    for point in points {
        min_x = min_x.min(point.x);
        min_y = min_y.min(point.y);
        max_x = max_x.max(point.x);
        max_y = max_y.max(point.y);
    }

    let x = min_x.floor() as i32;
    let y = min_y.floor() as i32;
    let width = (max_x.ceil() - min_x.floor()).max(0.0) as u32;
    let height = (max_y.ceil() - min_y.floor()).max(0.0) as u32;

    if width == 0 || height == 0 {
        return Err(CaptureError::EmptyArea);
    }

    Ok(Bounds {
        x,
        y,
        width,
        height,
    })
}

/// Build a 0.0-1.0 coverage value per pixel, in the cropped image's own space.
fn rasterise(points: &[Point], origin: (i32, i32), width: u32, height: u32) -> Vec<f32> {
    let mut coverage = vec![0.0f32; width as usize * height as usize];

    // Translate the polygon into the cropped image's pixel space once, rather
    // than offsetting inside the inner loops.
    let local: Vec<Point> = points
        .iter()
        .map(|p| Point {
            x: p.x - origin.0 as f64,
            y: p.y - origin.1 as f64,
        })
        .collect();

    // Reused across rows so the rasteriser does not allocate per scanline.
    let mut crossings: Vec<f64> = Vec::with_capacity(local.len());
    let weight = 1.0 / SUBSAMPLES as f32;

    for row in 0..height {
        for sub in 0..SUBSAMPLES {
            let sample_y = row as f64 + (sub as f64 + 0.5) / SUBSAMPLES as f64;

            crossings.clear();
            collect_crossings(&local, sample_y, &mut crossings);
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

            // Even-odd rule: fill between alternate pairs of crossings, which
            // handles self-intersecting lassos the way a user expects.
            for pair in crossings.as_chunks::<2>().0 {
                add_span(&mut coverage, row, width, pair[0], pair[1], weight);
            }
        }
    }

    coverage
}

/// X positions where the polygon's edges cross a given horizontal line.
fn collect_crossings(points: &[Point], y: f64, out: &mut Vec<f64>) {
    for index in 0..points.len() {
        let a = points[index];
        let b = points[(index + 1) % points.len()];

        // A horizontal edge never "crosses" a scanline; including it would
        // corrupt the even-odd pairing.
        if (a.y - b.y).abs() < f64::EPSILON {
            continue;
        }

        // Half-open test: a vertex exactly on the scanline counts for one of its
        // two edges, never both, which is what stops spurious double crossings.
        let (lower, upper) = if a.y < b.y { (a, b) } else { (b, a) };
        if y >= lower.y && y < upper.y {
            let t = (y - lower.y) / (upper.y - lower.y);
            out.push(lower.x + t * (upper.x - lower.x));
        }
    }
}

/// Add fractional coverage for a horizontal span, with partial pixels at each end.
fn add_span(coverage: &mut [f32], row: u32, width: u32, x0: f64, x1: f64, weight: f32) {
    let left = x0.max(0.0);
    let right = x1.min(width as f64);
    if right <= left {
        return;
    }

    let row_offset = row as usize * width as usize;
    let first = left.floor() as usize;
    let last = (right.ceil() as usize).min(width as usize);

    for pixel in first..last {
        // How much of this pixel's 1.0-wide cell the span actually covers.
        let cell_start = pixel as f64;
        let cell_end = cell_start + 1.0;
        let covered = (right.min(cell_end) - left.max(cell_start)).max(0.0);
        if covered > 0.0 {
            coverage[row_offset + pixel] += weight * covered as f32;
        }
    }
}

/// Write coverage into the image's alpha channel.
fn apply_coverage(image: &mut RgbaImage, coverage: &[f32]) {
    for (pixel, &cover) in image.pixels_mut().zip(coverage.iter()) {
        pixel.0[3] = (cover.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(width: u32, height: u32) -> Frame {
        // Opaque white, so any transparency in the result came from the mask.
        let mut image = RgbaImage::new(width, height);
        for pixel in image.pixels_mut() {
            pixel.0 = [255, 255, 255, 255];
        }
        Frame {
            image,
            origin: (0, 0),
        }
    }

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point> {
        vec![
            Point { x: x0, y: y0 },
            Point { x: x1, y: y0 },
            Point { x: x1, y: y1 },
            Point { x: x0, y: y1 },
        ]
    }

    #[test]
    fn rejects_a_degenerate_polygon() {
        let f = frame(50, 50);
        assert!(apply_polygon(&f, &[Point { x: 1.0, y: 1.0 }]).is_err());
    }

    #[test]
    fn a_rectangle_polygon_crops_to_its_bounds() {
        let f = frame(100, 100);
        let result = apply_polygon(&f, &square(10.0, 20.0, 40.0, 60.0)).unwrap();
        assert_eq!(result.image.dimensions(), (30, 40));
        assert_eq!(result.origin, (10, 20));
    }

    #[test]
    fn interior_is_opaque_and_exterior_is_transparent() {
        let f = frame(100, 100);
        // A triangle, so the bounding box has substantial area outside the shape.
        let triangle = vec![
            Point { x: 10.0, y: 10.0 },
            Point { x: 90.0, y: 10.0 },
            Point { x: 10.0, y: 90.0 },
        ];
        let result = apply_polygon(&f, &triangle).unwrap();

        // Just inside the right-angle corner.
        assert_eq!(result.image.get_pixel(3, 3).0[3], 255);
        // The far corner of the bounding box lies outside the triangle.
        let (w, h) = result.image.dimensions();
        assert_eq!(result.image.get_pixel(w - 2, h - 2).0[3], 0);
    }

    #[test]
    fn colour_channels_are_left_untouched() {
        let f = frame(60, 60);
        let result = apply_polygon(&f, &square(5.0, 5.0, 50.0, 50.0)).unwrap();
        let pixel = result.image.get_pixel(10, 10);
        assert_eq!([pixel.0[0], pixel.0[1], pixel.0[2]], [255, 255, 255]);
    }

    #[test]
    fn a_lasso_running_off_screen_is_clipped_not_rejected() {
        let f = frame(50, 50);
        // Extends well past the frame on the right and bottom.
        let result = apply_polygon(&f, &square(30.0, 30.0, 200.0, 200.0)).unwrap();
        let (w, h) = result.image.dimensions();
        assert!(w <= 20 && h <= 20, "got {w}x{h}");
    }

    #[test]
    fn diagonal_edges_are_antialiased() {
        let f = frame(100, 100);
        let triangle = vec![
            Point { x: 0.0, y: 0.0 },
            Point { x: 80.0, y: 0.0 },
            Point { x: 0.0, y: 80.0 },
        ];
        let result = apply_polygon(&f, &triangle).unwrap();

        // The hypotenuse should produce at least one partially transparent
        // pixel; a binary in/out test would only ever yield 0 or 255.
        let partial = result.image.pixels().any(|p| p.0[3] > 0 && p.0[3] < 255);
        assert!(partial, "expected antialiased edge pixels");
    }
}
