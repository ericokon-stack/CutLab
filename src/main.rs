#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! CutLab: vinyl-cutter design app (Rust / egui). Internal units are millimetres;
//! the UI shows inches by default. Output is HPGL for Roland CAMM-1 GS-24 class cutters.

use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Vec2};
use geo::{BooleanOps, Coord, LineString, MultiPolygon, Polygon};

type Pt = [f64; 2];
const RULER: f32 = 20.0;

// ───────────────────────── model ─────────────────────────
#[derive(Clone)]
struct Obj {
    name: String,
    polys: Vec<Vec<Pt>>, // normalised to (0,0) top-left, y-down, mm
    bw: f64,
    bh: f64,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    fx: bool,
}

impl Obj {
    fn new(name: &str, polys: Vec<Vec<Pt>>, at: Option<Pt>) -> Self {
        let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
        for p in polys.iter().flatten() {
            x0 = x0.min(p[0]);
            y0 = y0.min(p[1]);
            x1 = x1.max(p[0]);
            y1 = y1.max(p[1]);
        }
        let polys = polys.into_iter().map(|a| a.into_iter().map(|p| [p[0] - x0, p[1] - y0]).collect()).collect();
        let (bw, bh) = ((x1 - x0).max(0.01), (y1 - y0).max(0.01));
        let [x, y] = at.unwrap_or([x0, y0]);
        Obj { name: name.into(), polys, bw, bh, x, y, w: bw, h: bh, fx: false }
    }
    fn place(&self, p: Pt) -> Pt {
        let mut x = p[0] * self.w / self.bw;
        if self.fx {
            x = self.w - x;
        }
        [self.x + x, self.y + p[1] * self.h / self.bh]
    }
    fn placed(&self) -> Vec<Vec<Pt>> {
        self.polys.iter().map(|a| a.iter().map(|&p| self.place(p)).collect()).collect()
    }
    fn bake(&mut self) {
        let polys = self.polys.iter().map(|a| a.iter().map(|&p| {
            let q = self.place(p);
            [q[0] - self.x, q[1] - self.y]
        }).collect()).collect();
        self.polys = polys;
        self.bw = self.w;
        self.bh = self.h;
        self.fx = false;
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Unit { In, Mm }
impl Unit {
    fn f(self) -> f64 { if self == Unit::In { 25.4 } else { 1.0 } }
}
#[derive(Clone, Copy, PartialEq)]
enum Drag { None, Move, Pan }

// ───────────────────────── geometry helpers ─────────────────────────
fn dist(a: Pt, b: Pt) -> f64 { (a[0] - b[0]).hypot(a[1] - b[1]) }
fn closed(p: &[Pt]) -> bool { p.len() > 3 && dist(p[0], p[p.len() - 1]) < 1e-6 }
fn rect_polys(w: f64, h: f64) -> Vec<Vec<Pt>> { vec![vec![[0.0, 0.0], [w, 0.0], [w, h], [0.0, h], [0.0, 0.0]]] }
fn ellipse_polys(w: f64, h: f64) -> Vec<Vec<Pt>> {
    vec![(0..=96).map(|i| {
        let t = std::f64::consts::TAU * i as f64 / 96.0;
        [w / 2.0 + w / 2.0 * t.cos(), h / 2.0 + h / 2.0 * t.sin()]
    }).collect()]
}

fn cubic(p0: Pt, p1: Pt, p2: Pt, p3: Pt, out: &mut Vec<Pt>) {
    let len = dist(p0, p1) + dist(p1, p2) + dist(p2, p3);
    let n = ((len / 0.25).ceil() as usize).clamp(4, 200);
    for i in 1..=n {
        let t = i as f64 / n as f64;
        let u = 1.0 - t;
        out.push([
            u * u * u * p0[0] + 3.0 * u * u * t * p1[0] + 3.0 * u * t * t * p2[0] + t * t * t * p3[0],
            u * u * u * p0[1] + 3.0 * u * u * t * p1[1] + 3.0 * u * t * t * p2[1] + t * t * t * p3[1],
        ]);
    }
}

/// Drag-knife blade-offset compensation: at sharp corners, overshoot by `r` then swivel the pivot around the vertex.
fn swivel(p: &[Pt], r: f64) -> Vec<Pt> {
    use std::f64::consts::{PI, TAU};
    let mut o = vec![p[0]];
    for i in 1..p.len() {
        let v = p[i];
        if i + 1 < p.len() {
            let (a, b) = (p[i - 1], p[i + 1]);
            let (d1, d2) = ((v[0] - a[0], v[1] - a[1]), (b[0] - v[0], b[1] - v[1]));
            if d1.0.hypot(d1.1) > 1e-9 && d2.0.hypot(d2.1) > 1e-9 {
                let (a1, a2) = (d1.1.atan2(d1.0), d2.1.atan2(d2.0));
                let mut da = a2 - a1;
                while da > PI { da -= TAU; }
                while da < -PI { da += TAU; }
                if da.abs() > 0.35 {
                    let n = ((da.abs() / 0.17).ceil() as usize).max(1);
                    for k in 0..=n {
                        let t = a1 + da * k as f64 / n as f64;
                        o.push([v[0] + r * t.cos(), v[1] + r * t.sin()]);
                    }
                    continue;
                }
            }
        }
        o.push(v);
    }
    o
}

// ───────────────────────── image trace (pixel-boundary tracing + simplify + smooth) ─────────────────────────
fn load_gray(path: &std::path::Path) -> Result<image::GrayImage, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let img = if img.width().max(img.height()) > 700 { img.resize(700, 700, image::imageops::FilterType::Triangle) } else { img };
    let rgba = img.to_rgba8();
    let mut g = image::GrayImage::new(rgba.width(), rgba.height());
    for (x, y, p) in rgba.enumerate_pixels() {
        let [r, gc, b, a] = p.0;
        let l = 0.299 * r as f32 + 0.587 * gc as f32 + 0.114 * b as f32;
        let a = a as f32 / 255.0;
        g.put_pixel(x, y, image::Luma([(l * a + 255.0 * (1.0 - a)) as u8]));
    }
    Ok(g)
}
fn seg_dist(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-12 { return dist(p, a); }
    let t = (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0);
    dist(p, [a[0] + t * dx, a[1] + t * dy])
}
fn dp(p: &[Pt], eps: f64) -> Vec<Pt> {
    if p.len() < 3 { return p.to_vec(); }
    let (a, b) = (p[0], p[p.len() - 1]);
    let (mut bi, mut bd) = (0, 0.0);
    for (i, q) in p.iter().enumerate().skip(1).take(p.len() - 2) {
        let d = seg_dist(*q, a, b);
        if d > bd { bd = d; bi = i; }
    }
    if bd > eps {
        let mut l = dp(&p[..=bi], eps);
        let r = dp(&p[bi..], eps);
        l.pop();
        l.extend(r);
        l
    } else { vec![a, b] }
}
fn simplify_ring(p: &[Pt], eps: f64) -> Vec<Pt> {
    let k = p.iter().enumerate().max_by(|a, b| dist(*a.1, p[0]).total_cmp(&dist(*b.1, p[0]))).map(|(i, _)| i).unwrap_or(0);
    if k == 0 { return p.to_vec(); }
    let mut first = dp(&p[..=k], eps);
    let mut tail: Vec<Pt> = p[k..].to_vec();
    tail.push(p[0]);
    let second = dp(&tail, eps);
    first.pop();
    first.extend(second);
    first.pop();
    first
}
fn chaikin(p: &[Pt]) -> Vec<Pt> {
    let n = p.len();
    let mut o = Vec::with_capacity(n * 2);
    for i in 0..n {
        let (a, b) = (p[i], p[(i + 1) % n]);
        o.push([0.75 * a[0] + 0.25 * b[0], 0.75 * a[1] + 0.25 * b[1]]);
        o.push([0.25 * a[0] + 0.75 * b[0], 0.25 * a[1] + 0.75 * b[1]]);
    }
    o
}
fn area(p: &[Pt]) -> f64 {
    (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f64>().abs() / 2.0
}
fn trace_image(g: &image::GrayImage, thresh: u8, invert: bool, tol: f64, smooth: bool) -> Vec<Vec<Pt>> {
    let (w, h) = (g.width() as i32, g.height() as i32);
    let fg = |x: i32, y: i32| x >= 0 && y >= 0 && x < w && y < h && ((g.get_pixel(x as u32, y as u32).0[0] < thresh) != invert);
    let mut next: std::collections::HashMap<(i32, i32), Vec<(i32, i32)>> = Default::default();
    for y in 0..h {
        for x in 0..w {
            if !fg(x, y) { continue; }
            if !fg(x, y - 1) { next.entry((x, y)).or_default().push((x + 1, y)); }
            if !fg(x + 1, y) { next.entry((x + 1, y)).or_default().push((x + 1, y + 1)); }
            if !fg(x, y + 1) { next.entry((x + 1, y + 1)).or_default().push((x, y + 1)); }
            if !fg(x - 1, y) { next.entry((x, y + 1)).or_default().push((x, y)); }
        }
    }
    let mut out = vec![];
    loop {
        let start = match next.keys().next() { Some(&k) => k, None => break };
        let (mut pts, mut cur) = (vec![start], start);
        loop {
            let Some(v) = next.get_mut(&cur) else { break };
            let n = v.pop().unwrap();
            if v.is_empty() { next.remove(&cur); }
            if n == start { break; }
            pts.push(n);
            cur = n;
        }
        let ring: Vec<Pt> = pts.iter().map(|&(x, y)| [x as f64, y as f64]).collect();
        if ring.len() < 4 || area(&ring) < 6.0 { continue; } // drop speckles
        let mut p = simplify_ring(&ring, tol);
        if smooth { for _ in 0..2 { p = chaikin(&p); } }
        if p.len() < 3 { continue; }
        p.push(p[0]);
        out.push(p);
    }
    out
}

// ───────────────────────── SVG import (usvg handles transforms, arcs, <use>, units) ─────────────────────────
fn flush(cur: &mut Vec<Pt>, out: &mut Vec<Vec<Pt>>) {
    if cur.len() > 1 { out.push(std::mem::take(cur)); } else { cur.clear(); }
}

fn flatten_path(p: &usvg::Path, out: &mut Vec<Vec<Pt>>) {
    use usvg::tiny_skia_path::{PathSegment as S, Point};
    let t = p.abs_transform();
    let k = 25.4 / 96.0;
    let m = |q: Point| -> Pt {
        let (x, y) = (q.x as f64, q.y as f64);
        [(t.sx as f64 * x + t.kx as f64 * y + t.tx as f64) * k, (t.ky as f64 * x + t.sy as f64 * y + t.ty as f64) * k]
    };
    let mut cur: Vec<Pt> = vec![];
    for s in p.data().segments() {
        match s {
            S::MoveTo(a) => { flush(&mut cur, out); cur.push(m(a)); }
            S::LineTo(a) => cur.push(m(a)),
            S::QuadTo(a, b) => {
                let (p0, c, p3) = (cur.last().copied().unwrap_or(m(a)), m(a), m(b));
                let c1 = [p0[0] + 2.0 / 3.0 * (c[0] - p0[0]), p0[1] + 2.0 / 3.0 * (c[1] - p0[1])];
                let c2 = [p3[0] + 2.0 / 3.0 * (c[0] - p3[0]), p3[1] + 2.0 / 3.0 * (c[1] - p3[1])];
                cubic(p0, c1, c2, p3, &mut cur);
            }
            S::CubicTo(a, b, c) => {
                let p0 = cur.last().copied().unwrap_or(m(a));
                cubic(p0, m(a), m(b), m(c), &mut cur);
            }
            S::Close => {
                if let Some(&f) = cur.first() { cur.push(f); }
                flush(&mut cur, out);
            }
        }
    }
    flush(&mut cur, out);
}

fn walk(g: &usvg::Group, out: &mut Vec<Vec<Pt>>) {
    for n in g.children() {
        match n {
            usvg::Node::Group(g) => walk(g, out),
            usvg::Node::Path(p) => flatten_path(p, out),
            _ => {}
        }
    }
}

fn import_svg(data: &[u8]) -> Result<Vec<Vec<Pt>>, String> {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).map_err(|e| e.to_string())?;
    let mut out = vec![];
    walk(tree.root(), &mut out);
    if out.is_empty() { Err("No shapes found (convert text to paths first)".into()) } else { Ok(out) }
}

// ───────────────────────── text → outlines ─────────────────────────
struct Outl { polys: Vec<Vec<Pt>>, cur: Vec<Pt>, ox: f64, s: f64 }
impl Outl {
    fn pt(&self, x: f32, y: f32) -> Pt { [self.ox + x as f64 * self.s, -(y as f64) * self.s] }
}
impl ttf_parser::OutlineBuilder for Outl {
    fn move_to(&mut self, x: f32, y: f32) { self.cur = vec![self.pt(x, y)]; }
    fn line_to(&mut self, x: f32, y: f32) { let p = self.pt(x, y); self.cur.push(p); }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let p0 = *self.cur.last().unwrap_or(&[0.0, 0.0]);
        let (c, p3) = (self.pt(x1, y1), self.pt(x, y));
        for i in 1..=10 {
            let t = i as f64 / 10.0;
            let u = 1.0 - t;
            self.cur.push([u * u * p0[0] + 2.0 * u * t * c[0] + t * t * p3[0], u * u * p0[1] + 2.0 * u * t * c[1] + t * t * p3[1]]);
        }
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let p0 = *self.cur.last().unwrap_or(&[0.0, 0.0]);
        let (a, b, c) = (self.pt(x1, y1), self.pt(x2, y2), self.pt(x, y));
        cubic(p0, a, b, c, &mut self.cur);
    }
    fn close(&mut self) {
        if let Some(&f) = self.cur.first() { self.cur.push(f); }
        let c = std::mem::take(&mut self.cur);
        if c.len() > 2 { self.polys.push(c); }
    }
}

fn text_polys(font: &[u8], text: &str, size_mm: f64) -> Result<Vec<Vec<Pt>>, String> {
    let face = ttf_parser::Face::parse(font, 0).map_err(|e| e.to_string())?;
    let s = size_mm / face.units_per_em() as f64;
    let mut b = Outl { polys: vec![], cur: vec![], ox: 0.0, s };
    for c in text.chars() {
        match face.glyph_index(c) {
            Some(g) => {
                let _ = face.outline_glyph(g, &mut b);
                b.ox += face.glyph_hor_advance(g).unwrap_or(0) as f64 * s;
            }
            None => b.ox += size_mm * 0.3,
        }
    }
    Ok(b.polys)
}

// ───────────────────────── weld / offset (geo boolean ops) ─────────────────────────
fn ring_poly(r: &[Pt]) -> Polygon<f64> {
    Polygon::new(LineString::from(r.iter().map(|p| (p[0], p[1])).collect::<Vec<_>>()), vec![])
}
/// Even-odd fill of an object's closed rings (XOR of rings), in placed mm coordinates.
fn fill(o: &Obj) -> MultiPolygon<f64> {
    let mut acc = MultiPolygon::<f64>::new(vec![]);
    for r in o.placed() {
        if closed(&r) { acc = acc.xor(&ring_poly(&r)); }
    }
    acc
}
fn union_all(mut v: Vec<MultiPolygon<f64>>) -> MultiPolygon<f64> {
    if v.is_empty() { return MultiPolygon::new(vec![]); }
    while v.len() > 1 {
        v = v.chunks(2).map(|c| if c.len() == 2 { c[0].union(&c[1]) } else { c[0].clone() }).collect();
    }
    v.pop().unwrap()
}
fn mp_to_polys(mp: &MultiPolygon<f64>) -> Vec<Vec<Pt>> {
    let mut v = vec![];
    for p in &mp.0 {
        for r in std::iter::once(p.exterior()).chain(p.interiors().iter()) {
            v.push(r.coords().map(|c| [c.x, c.y]).collect());
        }
    }
    v
}
/// Offset by |d| mm: outward when d>=0, inward when d<0 (Minkowski band of boundary segments + round joins).
fn grow(mp: &MultiPolygon<f64>, d: f64) -> MultiPolygon<f64> {
    let r = d.abs();
    let mut pieces = vec![];
    for poly in &mp.0 {
        for ring in std::iter::once(poly.exterior()).chain(poly.interiors().iter()) {
            let c: Vec<Coord<f64>> = ring.coords().cloned().collect();
            for w in c.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let l = dx.hypot(dy);
                if l < 1e-9 { continue; }
                let (nx, ny) = (-dy / l * r, dx / l * r);
                pieces.push(MultiPolygon::new(vec![ring_poly(&[[a.x + nx, a.y + ny], [b.x + nx, b.y + ny], [b.x - nx, b.y - ny], [a.x - nx, a.y - ny]])]));
            }
            for v in &c {
                let pts: Vec<Pt> = (0..12).map(|i| {
                    let t = i as f64 * std::f64::consts::TAU / 12.0;
                    [v.x + r * 1.035 * t.cos(), v.y + r * 1.035 * t.sin()]
                }).collect();
                pieces.push(MultiPolygon::new(vec![ring_poly(&pts)]));
            }
        }
    }
    let band = union_all(pieces);
    if d >= 0.0 { mp.union(&band) } else { mp.difference(&band) }
}

// ───────────────────────── Windows raw printer send ─────────────────────────
#[cfg(windows)]
fn send_raw(printer: &str, data: &[u8]) -> Result<(), String> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Graphics::Printing::*;
    let w = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    unsafe {
        let mut h: HANDLE = std::ptr::null_mut();
        let name = w(printer);
        if OpenPrinterW(name.as_ptr(), &mut h, std::ptr::null()) == 0 {
            return Err(format!("Cannot open printer '{printer}'"));
        }
        let (mut doc, mut ty) = (w("HPGL cut job"), w("RAW"));
        let di = DOC_INFO_1W { pDocName: doc.as_mut_ptr(), pOutputFile: std::ptr::null_mut(), pDatatype: ty.as_mut_ptr() };
        let mut ok = false;
        if StartDocPrinterW(h, 1, &di) != 0 {
            if StartPagePrinter(h) != 0 {
                let mut written = 0u32;
                ok = WritePrinter(h, data.as_ptr() as *const _, data.len() as u32, &mut written) != 0 && written as usize == data.len();
                EndPagePrinter(h);
            }
            EndDocPrinter(h);
        }
        ClosePrinter(h);
        if ok { Ok(()) } else { Err("Write to printer failed".into()) }
    }
}
#[cfg(not(windows))]
fn send_raw(_: &str, _: &[u8]) -> Result<(), String> { Err("Direct printer send is Windows-only; export a .plt instead".into()) }

fn list_printers() -> Vec<String> {
    let mut c = std::process::Command::new("powershell");
    c.args(["-NoProfile", "-Command", "(Get-Printer).Name"]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c.output().ok().map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

// ───────────────────────── app ─────────────────────────
struct App {
    objs: Vec<Obj>,
    sel: Option<usize>,
    hist: Vec<(Vec<Obj>, Option<usize>)>,
    unit: Unit,
    prev_unit: Unit,
    mat_w: f64, // in current unit
    mat_h: f64,
    vs: f64,   // px per mm
    vo: Vec2,  // screen offset of world origin from canvas top-left
    need_fit: bool,
    drag: Drag,
    lock: bool,
    text: String,
    text_size: f64,
    font: Option<Vec<u8>>,
    font_name: String,
    upmm: f64,
    overcut: f64,
    offset: f64,
    printer: String,
    printers: Vec<String>,
    msg: String,
    trace_img: Option<image::GrayImage>,
    trace_name: String,
    thresh: f32,
    invert: bool,
    smooth: bool,
    trace_w: f64,
    tol: f64,
    blade: f64,
}

impl App {
    fn new() -> Self {
        let font = std::fs::read(r"C:\Windows\Fonts\arial.ttf").ok();
        let font_name = if font.is_some() { "arial.ttf".into() } else { "none".into() };
        App {
            objs: vec![], sel: None, hist: vec![], unit: Unit::In, prev_unit: Unit::In, mat_w: 22.0, mat_h: 16.0,
            vs: 3.0, vo: Vec2::new(30.0, 30.0), need_fit: true, drag: Drag::None, lock: true, text: "Hello".into(),
            text_size: 1.0, font, font_name, upmm: 40.0, overcut: 0.02, offset: 0.05, printer: "Roland GS-24".into(),
            trace_img: None, trace_name: String::new(), thresh: 128.0, invert: false, smooth: true, trace_w: 4.0, tol: 1.0, blade: 0.0, printers: vec![], msg: "Drag to move, drag empty space to pan, wheel to zoom. Origin of the cut job = bottom-left of all objects.".into(),
        }
    }
    fn mat(&self) -> (f64, f64) { (self.mat_w * self.unit.f(), self.mat_h * self.unit.f()) }
    fn snap(&mut self) {
        self.hist.push((self.objs.clone(), self.sel));
        if self.hist.len() > 30 { self.hist.remove(0); }
    }
    fn undo(&mut self) {
        if let Some((o, s)) = self.hist.pop() { self.objs = o; self.sel = s; }
    }
    fn add(&mut self, name: &str, polys: Vec<Vec<Pt>>) {
        self.snap();
        self.objs.push(Obj::new(name, polys, Some([12.7, 12.7])));
        self.sel = Some(self.objs.len() - 1);
    }
    fn fit(&mut self, size: Vec2) {
        let (mw, mh) = self.mat();
        let s = ((size.x as f64 - 70.0) / mw).min((size.y as f64 - 70.0) / mh).max(0.05);
        self.vs = s;
        self.vo = Vec2::new(((size.x as f64 - mw * s) / 2.0).max(RULER as f64 + 8.0) as f32, ((size.y as f64 - mh * s) / 2.0).max(RULER as f64 + 8.0) as f32);
    }
    fn to_world(&self, p: Pos2, r: Rect) -> Pt { [(p.x - r.min.x - self.vo.x) as f64 / self.vs, (p.y - r.min.y - self.vo.y) as f64 / self.vs] }
    fn hit(&self, w: Pt) -> Option<usize> {
        self.objs.iter().rposition(|o| w[0] >= o.x && w[0] <= o.x + o.w && w[1] >= o.y && w[1] <= o.y + o.h)
    }

    fn import(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("SVG", &["svg"]).pick_file() {
            let n = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|d| import_svg(&d)) {
                Ok(polys) => { self.add(&n, polys); self.msg = format!("Imported {n}"); }
                Err(e) => self.msg = format!("Error: {e}"),
            }
        }
    }
    fn load_font(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Font", &["ttf", "otf"]).set_directory(r"C:\Windows\Fonts").pick_file() {
            match std::fs::read(&p) {
                Ok(d) => { self.font = Some(d); self.font_name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(); }
                Err(e) => self.msg = e.to_string(),
            }
        }
    }
    fn add_text(&mut self) {
        let size = self.text_size * self.unit.f();
        let r = self.font.as_ref().map(|d| text_polys(d, &self.text, size));
        match r {
            None => self.msg = "Load a .ttf/.otf font first".into(),
            Some(Err(e)) => self.msg = format!("Font error: {e}"),
            Some(Ok(p)) if p.is_empty() => self.msg = "No outlines for that text".into(),
            Some(Ok(p)) => { let n = format!("\"{}\"", self.text); self.add(&n, p); }
        }
    }
    fn load_image(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Image", &["png", "jpg", "jpeg", "bmp", "gif"]).pick_file() {
            match load_gray(&p) {
                Ok(g) => {
                    self.trace_name = p.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    self.msg = format!("Loaded {} ({}x{}). Adjust threshold, then Trace.", self.trace_name, g.width(), g.height());
                    self.trace_img = Some(g);
                }
                Err(e) => self.msg = format!("Image error: {e}"),
            }
        }
    }
    fn run_trace(&mut self) {
        let Some(g) = &self.trace_img else { self.msg = "Load an image first".into(); return };
        let polys = trace_image(g, self.thresh as u8, self.invert, self.tol, self.smooth);
        let k = self.trace_w * self.unit.f() / g.width() as f64;
        if polys.is_empty() { self.msg = "Nothing found: adjust threshold or invert".into(); return; }
        let polys: Vec<Vec<Pt>> = polys.into_iter().map(|p| p.into_iter().map(|q| [q[0] * k, q[1] * k]).collect()).collect();
        let n = format!("Trace {}", self.trace_name);
        self.add(&n, polys);
    }
    fn weld(&mut self) {
        let (mut fills, mut opens) = (vec![], vec![]);
        for o in &self.objs {
            fills.push(fill(o));
            opens.extend(o.placed().into_iter().filter(|p| !closed(p)));
        }
        let mut polys = mp_to_polys(&union_all(fills));
        polys.extend(opens);
        if polys.is_empty() { self.msg = "Nothing to weld".into(); return; }
        self.snap();
        self.objs = vec![Obj::new("Welded", polys, None)];
        self.sel = Some(0);
        self.msg = "Welded into one object".into();
    }
    fn offset_sel(&mut self, sign: f64) {
        let Some(i) = self.sel else { self.msg = "Select an object first".into(); return };
        let res = grow(&fill(&self.objs[i]), self.offset * self.unit.f() * sign);
        if res.0.is_empty() { self.msg = "Offset removed the whole shape (closed shapes only)".into(); return; }
        self.snap();
        let name = format!("{} {}", self.objs[i].name, if sign > 0.0 { "+" } else { "-" });
        self.objs[i] = Obj::new(&name, mp_to_polys(&res), None);
    }
    fn rotate(&mut self) {
        let Some(i) = self.sel else { return };
        self.snap();
        let o = &mut self.objs[i];
        o.bake();
        let h = o.bh;
        for p in o.polys.iter_mut().flatten() { *p = [h - p[1], p[0]]; }
        std::mem::swap(&mut o.bw, &mut o.bh);
        std::mem::swap(&mut o.w, &mut o.h);
    }

    fn hpgl(&self) -> Result<String, String> {
        let mut ps: Vec<Vec<Pt>> = self.objs.iter().flat_map(|o| o.placed()).filter(|p| p.len() > 1).collect();
        if ps.is_empty() { return Err("Nothing to cut yet".into()); }
        let x0 = ps.iter().flatten().map(|p| p[0]).fold(f64::MAX, f64::min);
        let y1 = ps.iter().flatten().map(|p| p[1]).fold(f64::MIN, f64::max);
        for q in ps.iter_mut().flatten() { *q = [q[0] - x0, y1 - q[1]]; }
        let mut ord: Vec<Vec<Pt>> = vec![];
        let mut pos = [0.0, 0.0];
        while !ps.is_empty() {
            let (mut bi, mut bd, mut rv) = (0, f64::MAX, false);
            for (i, p) in ps.iter().enumerate() {
                let last = *p.last().unwrap();
                let a = dist(pos, p[0]);
                let b = if dist(p[0], last) < 1e-6 { f64::MAX } else { dist(pos, last) };
                if a < bd { bd = a; bi = i; rv = false; }
                if b < bd { bd = b; bi = i; rv = true; }
            }
            let mut p = ps.swap_remove(bi);
            if rv { p.reverse(); }
            pos = *p.last().unwrap();
            ord.push(p);
        }
        let oc = self.overcut * self.unit.f();
        let bl = self.blade * self.unit.f();
        let u = self.upmm;
        let r = |v: f64| (v * u).round() as i64;
        let mut out = String::from("IN;\nSP1;\n");
        for mut p in ord {
            if oc > 0.0 && dist(p[0], *p.last().unwrap()) < 1e-6 {
                let (mut a, mut extra) = (0.0, vec![]);
                for i in 1..p.len() {
                    if a >= oc { break; }
                    let s = dist(p[i - 1], p[i]);
                    if a + s > oc {
                        let t = (oc - a) / s;
                        extra.push([p[i - 1][0] + (p[i][0] - p[i - 1][0]) * t, p[i - 1][1] + (p[i][1] - p[i - 1][1]) * t]);
                        a = oc;
                    } else { extra.push(p[i]); a += s; }
                }
                p.extend(extra);
            }
            if bl > 0.0 { p = swivel(&p, bl); }
            out.push_str(&format!("PU{},{};\n", r(p[0][0]), r(p[0][1])));
            let mut last = (r(p[0][0]), r(p[0][1]));
            let mut c: Vec<String> = vec![];
            for q in &p[1..] {
                let k = (r(q[0]), r(q[1]));
                if k != last { c.push(format!("{},{}", k.0, k.1)); last = k; }
            }
            if !c.is_empty() { out.push_str(&format!("PD{};\n", c.join(","))); }
            out.push_str("PU;\n");
        }
        out.push_str("PU0,0;\nSP0;\n");
        Ok(out)
    }
    fn export(&mut self) {
        match self.hpgl() {
            Ok(s) => if let Some(p) = rfd::FileDialog::new().set_file_name("job.plt").save_file() {
                self.msg = match std::fs::write(&p, s) { Ok(_) => format!("Saved {}", p.display()), Err(e) => e.to_string() };
            },
            Err(e) => self.msg = e,
        }
    }
    fn send(&mut self) {
        let r = self.hpgl().and_then(|s| send_raw(&self.printer, s.as_bytes()));
        self.msg = match r { Ok(()) => format!("Sent to '{}'", self.printer), Err(e) => e };
    }
}

fn dv(ui: &mut egui::Ui, v: &mut f64, f: f64, speed: f64) -> bool {
    let mut x = *v / f;
    let ch = ui.add(egui::DragValue::new(&mut x).speed(speed).max_decimals(3)).changed();
    if ch { *v = x * f; }
    ch
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if !ctx.wants_keyboard_input() {
            let (del, undo) = ctx.input(|i| (i.key_pressed(egui::Key::Delete), i.modifiers.command && i.key_pressed(egui::Key::Z)));
            if undo { self.undo(); }
            if del { if let Some(i) = self.sel { self.snap(); self.objs.remove(i); self.sel = None; } }
        }

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui.button("Import SVG").clicked() { self.import(); }
                if ui.button("Rect").clicked() { self.add("Rect", rect_polys(50.8, 38.1)); }
                if ui.button("Ellipse").clicked() { self.add("Ellipse", ellipse_polys(50.8, 50.8)); }
                ui.separator();
                if ui.button(format!("Font: {}", self.font_name)).clicked() { self.load_font(); }
                ui.add(egui::TextEdit::singleline(&mut self.text).desired_width(90.0));
                ui.add(egui::DragValue::new(&mut self.text_size).speed(0.05).range(0.05..=50.0));
                if ui.button("Add text").clicked() { self.add_text(); }
                ui.separator();
                if ui.button("Fit").clicked() { self.need_fit = true; }
                if ui.button("Undo").clicked() { self.undo(); }
                egui::ComboBox::from_label("Units").selected_text(if self.unit == Unit::In { "inches" } else { "mm" }).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.unit, Unit::In, "inches");
                    ui.selectable_value(&mut self.unit, Unit::Mm, "mm");
                });
                ui.separator();
                if ui.button("Export .plt").clicked() { self.export(); }
                if ui.button("Send to cutter").clicked() { self.send(); }
            });
        });
        if self.unit != self.prev_unit {
            let k = self.prev_unit.f() / self.unit.f();
            for v in [&mut self.mat_w, &mut self.mat_h, &mut self.text_size, &mut self.offset, &mut self.overcut, &mut self.trace_w, &mut self.blade] { *v *= k; }
            self.prev_unit = self.unit;
            self.need_fit = true;
        }
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| { ui.label(&self.msg); });

        egui::SidePanel::right("props").default_width(270.0).show(ctx, |ui| {
            let f = self.unit.f();
            let suf = if self.unit == Unit::In { "in" } else { "mm" };
            ui.heading("Selected");
            if let Some(i) = self.sel {
                let lock = self.lock;
                let o = &mut self.objs[i];
                ui.horizontal(|ui| { ui.label("X"); dv(ui, &mut o.x, f, 0.01); ui.label("Y"); dv(ui, &mut o.y, f, 0.01); ui.label(suf); });
                ui.horizontal(|ui| {
                    ui.label("W");
                    if dv(ui, &mut o.w, f, 0.01) && lock { o.h = o.w * o.bh / o.bw; }
                    ui.label("H");
                    if dv(ui, &mut o.h, f, 0.01) && lock { o.w = o.h * o.bw / o.bh; }
                    ui.label(suf);
                });
                ui.checkbox(&mut self.lock, "keep proportions");
                ui.horizontal_wrapped(|ui| {
                    if ui.button("Mirror").clicked() { self.snap(); let o = &mut self.objs[i]; o.fx = !o.fx; }
                    if ui.button("Rotate 90").clicked() { self.rotate(); }
                    if ui.button("Duplicate").clicked() {
                        self.snap();
                        let mut c = self.objs[i].clone();
                        c.x += 6.35; c.y += 6.35;
                        self.objs.push(c);
                        self.sel = Some(self.objs.len() - 1);
                    }
                    if ui.button("Delete").clicked() { self.snap(); self.objs.remove(i); self.sel = None; }
                });
            } else { ui.label("Nothing selected"); }
            ui.separator();
            ui.heading("Edit");
            ui.horizontal(|ui| {
                if ui.button("Weld all").clicked() { self.weld(); }
                if ui.button("Offset +").clicked() { self.offset_sel(1.0); }
                if ui.button("Offset -").clicked() { self.offset_sel(-1.0); }
                ui.add(egui::DragValue::new(&mut self.offset).speed(0.005).suffix(format!(" {suf}")));
            });
            ui.separator();
            ui.heading("Objects");
            let names: Vec<String> = self.objs.iter().map(|o| format!("{} ({:.1} x {:.1})", o.name, o.w / f, o.h / f)).collect();
            for (i, n) in names.iter().enumerate() {
                if ui.selectable_label(self.sel == Some(i), n).clicked() { self.sel = Some(i); }
            }
            ui.separator();
            ui.heading("Trace image");
            ui.horizontal(|ui| { if ui.button("Load image...").clicked() { self.load_image(); } ui.label(&self.trace_name); });
            ui.add(egui::Slider::new(&mut self.thresh, 1.0..=254.0).text("threshold"));
            ui.horizontal(|ui| { ui.checkbox(&mut self.invert, "invert"); ui.checkbox(&mut self.smooth, "smooth"); });
            ui.horizontal(|ui| {
                ui.label("Width");
                ui.add(egui::DragValue::new(&mut self.trace_w).speed(0.1).range(0.1..=100.0).suffix(format!(" {suf}")));
                ui.label("Detail");
                ui.add(egui::DragValue::new(&mut self.tol).speed(0.05).range(0.2..=5.0));
            });
            if ui.button("Trace").clicked() { self.run_trace(); }
            ui.separator();
            ui.heading("Material");
            ui.horizontal(|ui| {
                ui.label("W"); ui.add(egui::DragValue::new(&mut self.mat_w).speed(0.1).range(1.0..=500.0));
                ui.label("H"); ui.add(egui::DragValue::new(&mut self.mat_h).speed(0.1).range(1.0..=500.0));
                ui.label(suf);
            });
            ui.heading("Output");
            ui.horizontal(|ui| { ui.label("Units/mm"); ui.add(egui::DragValue::new(&mut self.upmm).speed(1.0).range(1.0..=1000.0)); });
            ui.horizontal(|ui| { ui.label("Overcut"); ui.add(egui::DragValue::new(&mut self.overcut).speed(0.005).range(0.0..=5.0).suffix(format!(" {suf}"))); });
            ui.horizontal(|ui| {
                ui.label("Blade offset");
                ui.add(egui::DragValue::new(&mut self.blade).speed(0.001).range(0.0..=1.0).max_decimals(4).suffix(format!(" {suf}")));
            });
            ui.horizontal(|ui| {
                ui.label("Printer");
                ui.text_edit_singleline(&mut self.printer);
            });
            if ui.button("List installed printers").clicked() { self.printers = list_printers(); }
            let mut pick = None;
            for p in &self.printers { if ui.selectable_label(*p == self.printer, p).clicked() { pick = Some(p.clone()); } }
            if let Some(p) = pick { self.printer = p; }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
            let rect = resp.rect;
            if self.need_fit { self.fit(rect.size()); self.need_fit = false; }
            // interaction
            if resp.drag_started() || resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    if pos.x > rect.min.x + RULER && pos.y > rect.min.y + RULER {
                        let h = self.hit(self.to_world(pos, rect));
                        self.sel = h;
                        if resp.drag_started() {
                            self.drag = if h.is_some() { self.snap(); Drag::Move } else { Drag::Pan };
                        }
                    } else { self.drag = Drag::None; }
                }
            }
            if resp.dragged() {
                let d = resp.drag_delta();
                match (self.drag, self.sel) {
                    (Drag::Move, Some(i)) => { self.objs[i].x += d.x as f64 / self.vs; self.objs[i].y += d.y as f64 / self.vs; }
                    (Drag::Pan, _) => self.vo += d,
                    _ => {}
                }
            }
            if resp.drag_stopped() { self.drag = Drag::None; }
            if resp.hovered() {
                let sc = ui.input(|i| i.raw_scroll_delta.y);
                if let (true, Some(h)) = (sc != 0.0, resp.hover_pos()) {
                    let w = self.to_world(h, rect);
                    self.vs = (self.vs * (sc as f64 * 0.0015).exp()).clamp(0.05, 400.0);
                    self.vo = Vec2::new(h.x - rect.min.x - (w[0] * self.vs) as f32, h.y - rect.min.y - (w[1] * self.vs) as f32);
                }
            }

            // painting
            let dark = ui.visuals().dark_mode;
            let (c_mat, c_grid, c_cut, c_ac, c_fg, c_bg) = if dark {
                (Color32::from_gray(30), Color32::from_gray(48), Color32::from_rgb(255, 107, 107), Color32::from_rgb(106, 166, 255), Color32::from_gray(225), Color32::from_gray(36))
            } else {
                (Color32::from_rgb(250, 250, 247), Color32::from_gray(228), Color32::from_rgb(196, 22, 28), Color32::from_rgb(10, 108, 255), Color32::from_gray(30), Color32::from_gray(245))
            };
            let (vs, vo) = (self.vs, self.vo);
            let sp = |p: Pt| Pos2::new(rect.min.x + vo.x + (p[0] * vs) as f32, rect.min.y + vo.y + (p[1] * vs) as f32);
            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let (mw, mh) = self.mat();
            painter.rect_filled(Rect::from_two_pos(sp([0.0, 0.0]), sp([mw, mh])), 0.0, c_mat);
            let gstep = if self.unit == Unit::In { 25.4 } else { 10.0 };
            if gstep * vs >= 6.0 {
                let mut g = 0.0;
                while g <= mw + 1e-6 { painter.line_segment([sp([g, 0.0]), sp([g, mh])], Stroke::new(1.0, c_grid)); g += gstep; }
                g = 0.0;
                while g <= mh + 1e-6 { painter.line_segment([sp([0.0, g]), sp([mw, g])], Stroke::new(1.0, c_grid)); g += gstep; }
            }
            for o in &self.objs {
                for p in o.placed() { painter.add(Shape::line(p.iter().map(|&q| sp(q)).collect(), Stroke::new(1.4, c_cut))); }
            }
            if let Some(i) = self.sel {
                let o = &self.objs[i];
                painter.rect_stroke(Rect::from_two_pos(sp([o.x, o.y]), sp([o.x + o.w, o.y + o.h])), 0.0, Stroke::new(1.0, c_ac));
            }

            // rulers
            let f = self.unit.f();
            let su = vs * f; // px per unit
            painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(rect.width(), RULER)), 0.0, c_bg);
            painter.rect_filled(Rect::from_min_size(rect.min, Vec2::new(RULER, rect.height())), 0.0, c_bg);
            let ms: &[f64] = if self.unit == Unit::In { &[0.0625, 0.125, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0] } else { &[1.0, 2.0, 5.0, 10.0, 50.0, 100.0] };
            let ls = [1.0, 2.0, 5.0, 10.0, 20.0, 50.0, 100.0, 500.0];
            let minor = ms.iter().copied().find(|s| s * su >= 7.0).unwrap_or(10.0);
            let label = ls.iter().copied().find(|&s| s * su >= 56.0 && s >= minor).unwrap_or(500.0);
            let is_m = |v: f64, q: f64| (v / q - (v / q).round()).abs() < 1e-6;
            let tick = Stroke::new(1.0, c_fg);
            for axis in 0..2 {
                let (o, len) = if axis == 0 { (vo.x as f64, rect.width() as f64) } else { (vo.y as f64, rect.height() as f64) };
                let (a, b) = (((RULER as f64 + 2.0 - o) / su / minor).ceil() as i64, ((len - o) / su / minor).floor() as i64);
                for i in a..=b {
                    let v = i as f64 * minor;
                    let p = (o + v * su) as f32;
                    let big = is_m(v, label);
                    let l = if big { 14.0 } else if is_m(v, label / 2.0) { 9.0 } else { 5.0 };
                    let txt = format!("{}", (v * 1000.0).round() / 1000.0);
                    if axis == 0 {
                        let x = rect.min.x + p;
                        painter.line_segment([Pos2::new(x, rect.min.y + RULER - l), Pos2::new(x, rect.min.y + RULER)], tick);
                        if big { painter.text(Pos2::new(x + 3.0, rect.min.y + 1.0), Align2::LEFT_TOP, txt, FontId::proportional(10.0), c_fg); }
                    } else {
                        let y = rect.min.y + p;
                        painter.line_segment([Pos2::new(rect.min.x + RULER - l, y), Pos2::new(rect.min.x + RULER, y)], tick);
                        if big { painter.text(Pos2::new(rect.min.x + 1.0, y + 1.0), Align2::LEFT_TOP, txt, FontId::proportional(9.0), c_fg); }
                    }
                }
            }
            painter.line_segment([Pos2::new(rect.min.x, rect.min.y + RULER), Pos2::new(rect.max.x, rect.min.y + RULER)], Stroke::new(1.0, c_grid));
            painter.line_segment([Pos2::new(rect.min.x + RULER, rect.min.y), Pos2::new(rect.min.x + RULER, rect.max.y)], Stroke::new(1.0, c_grid));
            if let Some(h) = resp.hover_pos() {
                painter.line_segment([Pos2::new(h.x, rect.min.y), Pos2::new(h.x, rect.min.y + RULER)], Stroke::new(1.0, c_ac));
                painter.line_segment([Pos2::new(rect.min.x, h.y), Pos2::new(rect.min.x + RULER, h.y)], Stroke::new(1.0, c_ac));
            }
            painter.text(rect.min + Vec2::new(3.0, 4.0), Align2::LEFT_TOP, if self.unit == Unit::In { "in" } else { "mm" }, FontId::proportional(9.0), c_fg);
        });
    }
}

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1240.0, 800.0]).with_title("CutLab"),
        ..Default::default()
    };
    eframe::run_native("CutLab", opts, Box::new(|_cc| Ok(Box::new(App::new()))))
}
