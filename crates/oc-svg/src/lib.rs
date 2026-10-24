//! SVG rendering: a small DOM with the CSS cascade, drawn with `oc-raster`.
#![forbid(unsafe_code)]
#![allow(
    clippy::indexing_slicing,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::chunks_exact_to_as_chunks
)]

pub mod clipmask;
pub mod color;
pub mod css;
pub mod dom;
pub mod filter;
pub mod markers;
pub mod paintserver;
pub mod parse;
pub mod render;
pub mod style;
pub mod text;

pub use render::{render, ImageDecoder, Options, Rendered};

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use oc_core::Limits;

    fn draw(svg: &str) -> oc_raster::Canvas {
        render(svg, &Options::default(), &Limits::default())
            .unwrap()
            .canvas
    }

    #[test]
    fn rect_fills_and_viewbox_scales() {
        let c = draw(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="40" viewBox="0 0 4 4"><rect width="2" height="4" fill="#f00"/></svg>"##,
        );
        assert_eq!(c.pixel(5, 20), [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(c.pixel(30, 20)[3], 0.0);
    }

    #[test]
    fn style_sheet_beats_attribute_and_inline_beats_sheet() {
        let c = draw(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="30" height="10"><style>rect{fill:#00f}</style><rect width="10" height="10" fill="#f00"/><rect x="10" width="10" height="10" style="fill:#0f0"/></svg>"##,
        );
        assert_eq!(c.pixel(5, 5), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(c.pixel(15, 5), [0.0, 1.0, 0.0, 1.0]);
    }

    #[test]
    fn gradient_and_clip_path() {
        let c = draw(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="10"><defs><linearGradient id="g"><stop offset="0" stop-color="#000"/><stop offset="1" stop-color="#fff"/></linearGradient><clipPath id="c"><rect width="50" height="10"/></clipPath></defs><rect width="100" height="10" fill="url(#g)" clip-path="url(#c)"/></svg>"##,
        );
        assert!(c.pixel(10, 5)[0] < 0.25);
        assert!(c.pixel(45, 5)[0] > 0.35);
        assert_eq!(c.pixel(75, 5)[3], 0.0);
    }

    #[test]
    fn use_cycles_and_missing_references_do_not_hang() {
        let c = draw(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"><g id="a"><use href="#a"/></g><rect width="5" height="5" fill="url(#nope) #f00"/><use href="#missing"/></svg>"##,
        );
        assert_eq!(c.pixel(2, 2), [1.0, 0.0, 0.0, 1.0]);
    }

    #[test]
    fn rejects_documents_without_a_size_and_huge_ones() {
        let l = Limits::default();
        assert!(render(
            "<svg xmlns='http://www.w3.org/2000/svg' width='0' height='5'/>",
            &Options::default(),
            &l
        )
        .is_err());
        assert!(render(
            "<svg xmlns='http://www.w3.org/2000/svg' width='9e9' height='9e9'/>",
            &Options::default(),
            &l
        )
        .is_err());
        assert!(render("not xml", &Options::default(), &l).is_err());
    }

    #[test]
    fn requested_width_keeps_the_aspect_ratio() {
        let r = render(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="20"/>"##,
            &Options {
                width: Some(50),
                ..Options::default()
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!((r.canvas.width, r.canvas.height), (50, 100));
    }
}
