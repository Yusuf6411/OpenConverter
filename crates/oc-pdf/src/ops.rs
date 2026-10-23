//! The display list the content-stream interpreter produces and the renderer, the text
//! extractor and the SVG exporter consume. All geometry is in page space: points, origin at the
//! top left, y down, rotation already applied.
use crate::color::ColorSpace;
use crate::font::PdfFont;
use crate::function::Function;
use oc_raster::{Blend, FillRule, Path, Pt, Transform};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub enum PaintSrc {
    Rgb([f32; 3]),
    Pattern(Arc<PatternPaint>, [f32; 3]),
}

pub enum PatternKind {
    Tiling { ops: Vec<Op>, bbox: [f64; 4], xstep: f64, ystep: f64, uncolored: bool },
    Shading(Arc<Shading>),
}

pub struct PatternPaint {
    /// Pattern space to page space.
    pub matrix: Transform,
    pub kind: PatternKind,
}

impl std::fmt::Debug for PatternPaint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PatternPaint")
    }
}

#[derive(Debug, Clone)]
pub struct StrokeStyle {
    pub width: f64,
    pub cap: u8,
    pub join: u8,
    pub miter: f64,
    pub dash: Vec<f64>,
    pub phase: f64,
}

pub struct SoftMask {
    pub ops: Vec<Op>,
    pub luminosity: bool,
    pub backdrop: [f32; 3],
    pub transfer: Option<Function>,
}

#[derive(Clone)]
pub struct Fx {
    pub alpha: f32,
    pub blend: Blend,
    pub smask: Option<Arc<SoftMask>>,
}

impl Default for Fx {
    fn default() -> Self {
        Fx { alpha: 1.0, blend: Blend::Normal, smask: None }
    }
}

pub struct ImageData {
    pub width: usize,
    pub height: usize,
    /// Straight RGBA, or for stencil masks only the alpha channel is meaningful.
    pub rgba: Vec<u8>,
    pub stencil: bool,
    pub interpolate: bool,
}

#[derive(Clone)]
pub struct PlacedGlyph {
    pub gid: Option<u16>,
    /// Font units (scaled by the font's unit scale) to page space.
    pub to_page: Transform,
    pub unicode: String,
    /// Baseline origin in page space.
    pub origin: Pt,
    /// Advance along the baseline direction, in page space units.
    pub advance: f64,
    pub code: u32,
    pub is_space: bool,
}

pub struct TextRun {
    pub font: Arc<PdfFont>,
    pub glyphs: Vec<PlacedGlyph>,
    /// Text render mode 0..7.
    pub mode: u8,
    pub fill: PaintSrc,
    pub stroke: PaintSrc,
    pub stroke_style: StrokeStyle,
    /// Stroke pen transform (the CTM) for line width.
    pub ctm: Transform,
    pub fill_fx: Fx,
    pub stroke_fx: Fx,
    /// Font size in page units (for extraction).
    pub size: f64,
}

pub enum Op {
    Save,
    Restore,
    Clip { path: Arc<Path>, rule: FillRule },
    Fill { path: Arc<Path>, rule: FillRule, paint: PaintSrc, fx: Fx },
    Stroke { path: Arc<Path>, ctm: Transform, style: StrokeStyle, paint: PaintSrc, fx: Fx },
    Text(Box<TextRun>),
    Image { image: Arc<ImageData>, ctm: Transform, fx: Fx, stencil_paint: Option<PaintSrc> },
    Shade { shading: Arc<Shading>, matrix: Transform, fx: Fx },
    Group { ops: Vec<Op>, fx: Fx, isolated: bool },
    /// Marked content boundaries matter for extraction only (ActualText, artifacts).
    Marked { tag: String, actual_text: Option<String>, begin: bool },
}

#[derive(Clone)]
pub struct MeshVertex {
    pub p: [f64; 2],
    /// RGB, or the parametric value in `c[0]` when the shading has a function.
    pub c: [f32; 3],
}

pub enum Shading {
    Axial { coords: [f64; 4], domain: [f64; 2], func: Option<Function>, extend: [bool; 2], cs: ColorSpace, background: Option<[f32; 3]>, bbox: Option<[f64; 4]> },
    Radial { coords: [f64; 6], domain: [f64; 2], func: Option<Function>, extend: [bool; 2], cs: ColorSpace, background: Option<[f32; 3]>, bbox: Option<[f64; 4]> },
    FunctionBased { domain: [f64; 4], matrix: Transform, func: Function, cs: ColorSpace, background: Option<[f32; 3]> },
    Mesh { triangles: Vec<[MeshVertex; 3]>, func: Option<Function>, cs: ColorSpace, background: Option<[f32; 3]> },
}

pub struct Page {
    pub width: f64,
    pub height: f64,
    pub ops: Vec<Op>,
    pub notes: Vec<String>,
}
