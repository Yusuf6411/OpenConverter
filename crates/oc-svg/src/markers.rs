//! Line markers: arrowheads and dots at the vertices of paths.
use crate::parse::{self, Length};
use crate::render::{parse_viewbox, view_transform, Env, Renderer};
use crate::style::{LenCtx, Style};
use oc_raster::{FillRule, Path, Pt, Seg, Transform};

struct Vertex {
    p: Pt,
    /// Direction arriving at the vertex, and leaving it.
    din: Option<Pt>,
    dout: Option<Pt>,
}

fn norm(v: Pt) -> Option<Pt> {
    let l = v.len();
    (l > 1e-12).then(|| v.scale(1.0 / l))
}

/// Start and end tangent of a segment from `from`.
fn tangents(from: Pt, seg: &Seg) -> (Option<Pt>, Option<Pt>, Pt) {
    match *seg {
        Seg::Line(p) | Seg::Move(p) => {
            let d = norm(p.sub(from));
            (d, d, p)
        }
        Seg::Quad(c, p) => {
            let s = norm(c.sub(from)).or_else(|| norm(p.sub(from)));
            let e = norm(p.sub(c)).or_else(|| norm(p.sub(from)));
            (s, e, p)
        }
        Seg::Cubic(c1, c2, p) => {
            let s = norm(c1.sub(from))
                .or_else(|| norm(c2.sub(from)))
                .or_else(|| norm(p.sub(from)));
            let e = norm(p.sub(c2))
                .or_else(|| norm(p.sub(c1)))
                .or_else(|| norm(p.sub(from)));
            (s, e, p)
        }
        Seg::Close => (None, None, from),
    }
}

fn vertices(path: &Path) -> Vec<Vertex> {
    let mut out: Vec<Vertex> = Vec::new();
    let mut cur = Pt::new(0.0, 0.0);
    let mut start = cur;
    let mut sub_first: Option<usize> = None;
    for seg in &path.segs {
        match seg {
            Seg::Move(p) => {
                out.push(Vertex {
                    p: *p,
                    din: None,
                    dout: None,
                });
                sub_first = Some(out.len() - 1);
                cur = *p;
                start = *p;
            }
            Seg::Close => {
                if let Some(last) = out.last_mut() {
                    let d = norm(start.sub(cur));
                    if let Some(d) = d {
                        last.dout = Some(d);
                        out.push(Vertex {
                            p: start,
                            din: Some(d),
                            dout: None,
                        });
                    }
                }
                // closing joins the end to the start tangent
                if let Some(f) = sub_first {
                    let (fd, ld) = (
                        out.get(f).and_then(|v| v.dout),
                        out.last().and_then(|v| v.din),
                    );
                    if let Some(first) = out.get_mut(f) {
                        first.din = ld;
                    }
                    if let Some(last) = out.last_mut() {
                        last.dout = fd;
                    }
                }
                cur = start;
            }
            s => {
                let (ds, de, p) = tangents(cur, s);
                if let Some(last) = out.last_mut() {
                    if last.dout.is_none() {
                        last.dout = ds;
                    }
                }
                out.push(Vertex {
                    p,
                    din: de,
                    dout: None,
                });
                cur = p;
            }
        }
    }
    out
}

fn angle(v: &Vertex) -> f64 {
    let a = v.din.map(|d| d.y.atan2(d.x));
    let b = v.dout.map(|d| d.y.atan2(d.x));
    match (a, b) {
        (Some(a), Some(b)) => {
            let mut d = b - a;
            while d > std::f64::consts::PI {
                d -= 2.0 * std::f64::consts::PI;
            }
            while d < -std::f64::consts::PI {
                d += 2.0 * std::f64::consts::PI;
            }
            a + d / 2.0
        }
        (Some(a), None) => a,
        (None, Some(b)) => b,
        (None, None) => 0.0,
    }
}

impl Renderer<'_> {
    pub fn draw_markers(&mut self, _i: usize, style: &Style, env: &Env, path: &Path) {
        if style.marker_start.is_none() && style.marker_mid.is_none() && style.marker_end.is_none()
        {
            return;
        }
        let vs = vertices(path);
        let n = vs.len();
        for (k, v) in vs.iter().enumerate() {
            let id = if k == 0 {
                &style.marker_start
            } else if k + 1 == n {
                &style.marker_end
            } else {
                &style.marker_mid
            };
            let Some(id) = id else { continue };
            self.draw_marker(id, v, k == 0, style, env);
        }
    }

    fn draw_marker(&mut self, id: &str, v: &Vertex, is_start: bool, style: &Style, env: &Env) {
        let Some(m) = self.doc.by_id(id) else { return };
        if self.doc.node(m).is_none_or(|n| n.tag != "marker")
            || self.active.contains(&m)
            || self.active.len() > 4
        {
            return;
        }
        let mstyle = self.style_chain(m);
        let fs = mstyle.font_size;
        let lc = env.lc;
        let mw = self.len_attr(m, "markerWidth", lc.vw, fs).unwrap_or(3.0);
        let mh = self.len_attr(m, "markerHeight", lc.vh, fs).unwrap_or(3.0);
        if mw <= 0.0 || mh <= 0.0 {
            return;
        }
        let units_stroke = self
            .doc
            .attr(m, "markerUnits")
            .is_none_or(|u| u.trim() != "userSpaceOnUse");
        let k = if units_stroke {
            style.stroke_width
        } else {
            1.0
        };
        let rotate = match self.doc.attr(m, "orient").map(str::trim) {
            Some("auto") => angle(v),
            Some("auto-start-reverse") => {
                angle(v) + if is_start { std::f64::consts::PI } else { 0.0 }
            }
            Some(a) => Length::parse(a).map_or(0.0, |l| l.v.to_radians()),
            None => 0.0,
        };
        let vb = self.doc.attr(m, "viewBox").and_then(parse_viewbox);
        let par = self
            .doc
            .attr(m, "preserveAspectRatio")
            .unwrap_or("xMidYMid meet")
            .to_string();
        let ref_x = self.len_attr(m, "refX", lc.vw, fs).unwrap_or(0.0);
        let ref_y = self.len_attr(m, "refY", lc.vh, fs).unwrap_or(0.0);
        // marker content space → marker viewport
        let (content_t, inner_lc) = match vb {
            Some(b) => (
                view_transform(b, (0.0, 0.0, mw, mh), &par),
                LenCtx { vw: b.2, vh: b.3 },
            ),
            None => (Transform::IDENTITY, LenCtx { vw: mw, vh: mh }),
        };
        let r = content_t.apply(Pt::new(ref_x, ref_y));
        let place = Transform::translate(-r.x, -r.y)
            .then(&Transform::scale(k, k))
            .then(&Transform::rotate(rotate))
            .then(&Transform::translate(v.p.x, v.p.y));
        let mut e = env.clone();
        e.ctm = content_t.then(&place).then(&env.ctm);
        e.lc = inner_lc;
        let overflow = self
            .doc
            .node(m)
            .and_then(|n| n.style.get("overflow"))
            .map_or("hidden", |s| s.trim());
        if !matches!(overflow, "visible" | "auto") {
            // The clip is the marker viewport (or the viewBox for the clipping region).
            let polys =
                oc_raster::device_polys(&Path::rect(0.0, 0.0, mw, mh), &place.then(&env.ctm));
            e.clip = e
                .clip
                .intersect(&polys, FillRule::NonZero, self.canvas.height);
        }
        let _ = parse::numbers;
        self.active.push(m);
        let kids = self
            .doc
            .node(m)
            .map(|n| n.children.clone())
            .unwrap_or_default();
        self.draw_children(&kids, &mstyle, &e);
        self.active.pop();
    }
}
