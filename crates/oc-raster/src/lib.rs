//! Vector rasteriser: anti-aliased fills and strokes, clips, gradients, patterns and
//! blend modes, used by the SVG and PDF renderers and by font previews.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::chunks_exact_to_as_chunks,
    clippy::should_implement_trait,
    clippy::manual_is_multiple_of
)]

pub mod blend;
pub mod canvas;
pub mod geom;
pub mod paint;
pub mod scan;
pub mod stroke;

pub use blend::Blend;
pub use canvas::{device_polys, stroke_polys, Canvas, Clip};
pub use geom::{Path, Poly, Pt, Seg, Transform};
pub use paint::{Filter, Paint, Pixmap, Rgba, Spread, Stop};
pub use scan::{FillRule, IRect};
pub use stroke::{Cap, Join, Stroke};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn solid_fill_and_clip() {
        let mut c = Canvas::new(20, 20).unwrap();
        let clip = c.clip().intersect(
            &device_polys(&Path::rect(0.0, 0.0, 10.0, 20.0), &Transform::IDENTITY),
            FillRule::NonZero,
            20,
        );
        c.fill_path(
            &Path::rect(0.0, 0.0, 20.0, 20.0),
            &Transform::IDENTITY,
            FillRule::NonZero,
            &Paint::Solid([1.0, 0.0, 0.0, 1.0]),
            Blend::Normal,
            1.0,
            &clip,
        );
        assert_eq!(c.pixel(5, 5), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(c.pixel(15, 5), [0.0; 4]);
    }

    #[test]
    fn linear_gradient_runs_left_to_right() {
        let mut c = Canvas::new(100, 4).unwrap();
        let paint = Paint::Linear {
            p0: Pt::new(0.0, 0.0),
            p1: Pt::new(100.0, 0.0),
            stops: vec![
                Stop {
                    offset: 0.0,
                    color: [0.0, 0.0, 0.0, 1.0],
                },
                Stop {
                    offset: 1.0,
                    color: [1.0, 1.0, 1.0, 1.0],
                },
            ],
            spread: Spread::Pad,
            extend: (true, true),
            to_gradient: Transform::IDENTITY,
        };
        let clip = c.clip();
        c.fill_path(
            &Path::rect(0.0, 0.0, 100.0, 4.0),
            &Transform::IDENTITY,
            FillRule::NonZero,
            &paint,
            Blend::Normal,
            1.0,
            &clip,
        );
        let (a, b) = (c.pixel(10, 1)[0], c.pixel(90, 1)[0]);
        assert!(a < 0.15 && b > 0.85, "{a} {b}");
    }

    #[test]
    fn multiply_darkens_and_alpha_composites() {
        let d = blend::composite(Blend::Multiply, [0.5, 0.5, 0.5, 1.0], [1.0, 0.0, 0.0, 1.0]);
        assert!((d[0] - 0.5).abs() < 1e-6 && d[1].abs() < 1e-6);
        let half = blend::composite(Blend::Normal, [0.0, 0.0, 0.0, 0.0], [0.5, 0.0, 0.0, 0.5]);
        assert_eq!(half, [0.5, 0.0, 0.0, 0.5]);
    }

    #[test]
    fn radial_gradient_centre_and_edge() {
        let mut c = Canvas::new(40, 40).unwrap();
        let paint = Paint::Radial {
            c0: Pt::new(20.0, 20.0),
            r0: 0.0,
            c1: Pt::new(20.0, 20.0),
            r1: 20.0,
            stops: vec![
                Stop {
                    offset: 0.0,
                    color: [1.0, 1.0, 1.0, 1.0],
                },
                Stop {
                    offset: 1.0,
                    color: [0.0, 0.0, 0.0, 1.0],
                },
            ],
            spread: Spread::Pad,
            extend: (true, true),
            to_gradient: Transform::IDENTITY,
        };
        let clip = c.clip();
        c.fill_path(
            &Path::rect(0.0, 0.0, 40.0, 40.0),
            &Transform::IDENTITY,
            FillRule::NonZero,
            &paint,
            Blend::Normal,
            1.0,
            &clip,
        );
        assert!(c.pixel(20, 20)[0] > 0.9);
        assert!(c.pixel(1, 20)[0] < 0.15);
    }

    #[test]
    fn blur_preserves_total_energy_away_from_edges() {
        let mut c = Canvas::new(41, 41).unwrap();
        let i = (20 * 41 + 20) * 4;
        c.data[i + 3] = 1.0;
        c.blur(2.0, 2.0);
        let sum: f32 = c.data.chunks_exact(4).map(|p| p[3]).sum();
        assert!((sum - 1.0).abs() < 1e-3, "{sum}");
        assert!(c.pixel(20, 20)[3] < 0.2);
    }
}
