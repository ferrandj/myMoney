//! The page interpreter (pdfminer's `PDFPageInterpreter` + `PDFTextDevice` + `LTChar`): runs the content
//! streams and records every character with its box, in pdfplumber's coordinates.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::document::Document;
use super::font::{Font, FontError};
use super::objects::{literal_name, Dict, Obj, Resolver};
use super::parser::ContentParser;

pub type Matrix = (f64, f64, f64, f64, f64, f64);
const IDENTITY: Matrix = (1.0, 0.0, 0.0, 1.0, 0.0, 0.0);

pub fn mult(m1: Matrix, m0: Matrix) -> Matrix {
    let (a1, b1, c1, d1, e1, f1) = m1;
    let (a0, b0, c0, d0, e0, f0) = m0;
    (a0 * a1 + c0 * b1, b0 * a1 + d0 * b1, a0 * c1 + c0 * d1, b0 * c1 + d0 * d1, a0 * e1 + c0 * f1 + e0, b0 * e1 + d0 * f1 + f0)
}

fn translate(m: Matrix, x: f64, y: f64) -> Matrix {
    let (a, b, c, d, e, f) = m;
    (a, b, c, d, x * a + y * c + e, x * b + y * d + f)
}

fn apply_pt(m: Matrix, x: f64, y: f64) -> (f64, f64) {
    let (a, b, c, d, e, f) = m;
    (a * x + c * y + e, b * x + d * y + f)
}

fn apply_rect(m: Matrix, r: (f64, f64, f64, f64)) -> (f64, f64, f64, f64) {
    let (x0, y0, x1, y1) = r;
    let (l1, b1) = apply_pt(m, x0, y0);
    let (r1, b2) = apply_pt(m, x1, y0);
    let (r2, t1) = apply_pt(m, x1, y1);
    let (l2, t2) = apply_pt(m, x0, y1);
    // Python's min/max keep the first of equal values (it matters for -0.0 and 0.0).
    (py_min(&[l1, l2, r1, r2]), py_min(&[b1, b2, t1, t2]), py_max(&[l1, l2, r1, r2]), py_max(&[b1, b2, t1, t2]))
}

fn py_min(v: &[f64]) -> f64 {
    let mut m = v[0];
    for &x in &v[1..] {
        if x < m {
            m = x;
        }
    }
    m
}

fn py_max(v: &[f64]) -> f64 {
    let mut m = v[0];
    for &x in &v[1..] {
        if x > m {
            m = x;
        }
    }
    m
}

/// A character as pdfplumber exposes it.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Char {
    pub text: String,
    pub x0: f64,
    pub x1: f64,
    pub top: f64,
    pub bottom: f64,
    pub upright: bool,
    pub size: f64,
    pub fontname: String,
}

#[derive(Clone)]
struct TextState {
    font: Option<Rc<Font>>,
    fontsize: f64,
    charspace: f64,
    wordspace: f64,
    scaling: f64,
    leading: f64,
    rise: f64,
    matrix: Matrix,
    linematrix: (f64, f64),
}

impl TextState {
    fn new() -> Self {
        TextState {
            font: None,
            fontsize: 0.0,
            charspace: 0.0,
            wordspace: 0.0,
            scaling: 100.0,
            leading: 0.0,
            rise: 0.0,
            matrix: IDENTITY,
            linematrix: (0.0, 0.0),
        }
    }
    fn reset(&mut self) {
        self.matrix = IDENTITY;
        self.linematrix = (0.0, 0.0);
    }
}

#[derive(Clone)]
struct ColorSpace {
    name: String,
    n: usize,
}

fn predefined(name: &str) -> Option<ColorSpace> {
    let n = match name {
        "DeviceGray" | "CalGray" | "Separation" | "Indexed" | "Pattern" => 1,
        "CalRGB" | "Lab" | "DeviceRGB" => 3,
        "DeviceCMYK" => 4,
        _ => return None,
    };
    Some(ColorSpace { name: name.to_string(), n })
}

#[derive(Clone)]
struct GraphicState {
    scs: ColorSpace,
    ncs: ColorSpace,
}

/// What survives across nested interpreters: the device (its CTM and the characters drawn) and the fonts.
pub struct Device {
    pub ctm: Matrix,
    pub chars: Vec<RawChar>,
    fonts: HashMap<u32, Rc<Font>>,
}

pub struct RawChar {
    pub text: String,
    pub bbox: (f64, f64, f64, f64),
    pub upright: bool,
    pub size: f64,
    pub fontname: String,
}

struct Interp<'d> {
    doc: &'d Document,
    fontmap: HashMap<String, Rc<Font>>,
    xobjmap: HashMap<String, Obj>,
    csmap: HashMap<String, ColorSpace>,
    resources: Dict,
    ctm: Matrix,
    ts: TextState,
    gs: GraphicState,
    gstack: Vec<(Matrix, TextState, GraphicState)>,
    argstack: Vec<Obj>,
    stream_ids: HashSet<u32>,
    parent_stream_ids: HashSet<u32>,
}

fn f(o: &Obj) -> Option<f64> {
    match o {
        Obj::Int(i) => Some(*i as f64),
        Obj::Real(x) => Some(*x),
        Obj::Bool(b) => Some(*b as i64 as f64),
        Obj::Str(s) => std::str::from_utf8(s).ok().and_then(|t| t.trim().parse().ok()),
        _ => None,
    }
}

impl<'d> Interp<'d> {
    fn new(doc: &'d Document, parent: HashSet<u32>) -> Self {
        let gray = predefined("DeviceGray").unwrap();
        Interp {
            doc,
            fontmap: HashMap::new(),
            xobjmap: HashMap::new(),
            csmap: HashMap::new(),
            resources: Dict::new(),
            ctm: IDENTITY,
            ts: TextState::new(),
            gs: GraphicState { scs: gray.clone(), ncs: gray },
            gstack: vec![],
            argstack: vec![],
            stream_ids: HashSet::new(),
            parent_stream_ids: parent,
        }
    }

    fn init_resources(&mut self, dev: &mut Device, resources: &Dict) -> Result<(), FontError> {
        self.resources = resources.clone();
        self.fontmap.clear();
        self.xobjmap.clear();
        self.csmap.clear();
        for n in ["DeviceGray", "CalRGB", "CalGray", "Lab", "DeviceRGB", "DeviceCMYK", "Separation", "Indexed", "Pattern"] {
            self.csmap.insert(n.to_string(), predefined(n).unwrap());
        }
        for (k, v) in resources {
            match k.as_str() {
                "Font" => {
                    if let Obj::Dict(fonts) = self.doc.resolve(v) {
                        for (fid, spec) in fonts.iter() {
                            let objid = if let Obj::Ref(id) = spec { Some(*id) } else { None };
                            let font = match objid.and_then(|id| dev.fonts.get(&id).cloned()) {
                                Some(f) => f,
                                None => {
                                    let spec_d = match self.doc.resolve(spec) {
                                        Obj::Dict(d) => (*d).clone(),
                                        _ => Dict::new(),
                                    };
                                    let font = Font::from_spec(self.doc, &spec_d)?;
                                    if let Some(id) = objid {
                                        dev.fonts.insert(id, font.clone());
                                    }
                                    font
                                }
                            };
                            self.fontmap.insert(fid.clone(), font);
                        }
                    }
                }
                "ColorSpace" => {
                    if let Obj::Dict(cs) = self.doc.resolve(v) {
                        for (csid, spec) in cs.iter() {
                            let spec = self.doc.resolve(spec);
                            let c = match &spec {
                                Obj::Array(a) if !a.is_empty() => {
                                    let name = literal_name(&a[0]);
                                    if name == "ICCBased" && a.len() >= 2 {
                                        let n = match self.doc.resolve(&a[1]) {
                                            Obj::Stream(s) => s.get("N").and_then(f).unwrap_or(0.0) as usize,
                                            _ => 0,
                                        };
                                        Some(ColorSpace { name, n })
                                    } else if name == "DeviceN" && a.len() >= 2 {
                                        let n = match self.doc.resolve(&a[1]) {
                                            Obj::Array(x) => x.len(),
                                            _ => 0,
                                        };
                                        Some(ColorSpace { name, n })
                                    } else {
                                        predefined(&name)
                                    }
                                }
                                other => predefined(&literal_name(other)),
                            };
                            if let Some(c) = c {
                                self.csmap.insert(csid.clone(), c);
                            }
                        }
                    }
                }
                "XObject" => {
                    if let Obj::Dict(xo) = self.doc.resolve(v) {
                        for (id, x) in xo.iter() {
                            self.xobjmap.insert(id.clone(), x.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn render_contents(&mut self, dev: &mut Device, resources: &Dict, streams: Vec<Obj>, ctm: Matrix) -> Result<(), FontError> {
        self.init_resources(dev, resources)?;
        // init_state
        self.gstack.clear();
        self.ctm = ctm;
        dev.ctm = ctm;
        self.ts = TextState::new();
        let gray = predefined("DeviceGray").unwrap();
        self.gs = GraphicState { scs: gray.clone(), ncs: gray };
        self.argstack.clear();
        self.execute(dev, streams)
    }

    fn execute(&mut self, dev: &mut Device, streams: Vec<Obj>) -> Result<(), FontError> {
        let mut valid = vec![];
        self.stream_ids.clear();
        for o in streams {
            let s = match self.doc.resolve(&o) {
                Obj::Stream(s) => s,
                _ => continue, // stream_value gives an empty stream without id: skipped
            };
            let Some(id) = s.objid else { continue };
            if self.parent_stream_ids.contains(&id) {
                continue;
            }
            self.stream_ids.insert(id);
            valid.push(s);
        }
        let datas: Vec<&[u8]> = valid.iter().map(|s| s.data(self.doc)).collect();
        // `data()` caches inside each stream; keep them alive while parsing.
        let datas: Vec<Vec<u8>> = datas.into_iter().map(|d| d.to_vec()).collect();
        let refs: Vec<&[u8]> = datas.iter().map(|d| d.as_slice()).collect();
        let mut parser = ContentParser::new(refs);
        while let Some(obj) = parser.next_object() {
            match obj {
                Obj::Keyword(k) => self.dispatch(dev, &k)?,
                other => self.argstack.push(other),
            }
        }
        Ok(())
    }

    fn pop(&mut self, n: usize) -> Vec<Obj> {
        if n == 0 {
            return vec![];
        }
        let at = self.argstack.len().saturating_sub(n);
        self.argstack.split_off(at)
    }

    fn dispatch(&mut self, dev: &mut Device, k: &[u8]) -> Result<(), FontError> {
        let name = String::from_utf8_lossy(k).into_owned();
        let nargs = match name.as_str() {
            "q" | "Q" | "h" | "S" | "s" | "f" | "F" | "f*" | "B" | "B*" | "b" | "b*" | "n" | "W" | "W*" | "SCN" | "scn" | "SC"
            | "sc" | "BT" | "ET" | "BX" | "EX" | "EMC" | "T*" | "BI" | "ID" => 0,
            "w" | "J" | "j" | "M" | "ri" | "i" | "gs" | "CS" | "cs" | "G" | "g" | "sh" | "MP" | "BMC" | "Tc" | "Tw" | "Tz"
            | "TL" | "Tr" | "Ts" | "TJ" | "Tj" | "'" | "EI" | "Do" => 1,
            "d" | "m" | "l" | "DP" | "BDC" | "Tf" | "Td" | "TD" => 2,
            "RG" | "rg" | "\"" => 3,
            "v" | "y" | "re" | "K" | "k" => 4,
            "cm" | "c" | "Tm" => 6,
            _ => return Ok(()), // unknown operator: its operands stay on the stack
        };
        let args = self.pop(nargs);
        if args.len() != nargs {
            return Ok(());
        }
        match name.as_str() {
            "q" => self.gstack.push((self.ctm, self.ts.clone(), self.gs.clone())),
            "Q" => {
                if let Some((ctm, ts, gs)) = self.gstack.pop() {
                    self.ctm = ctm;
                    self.ts = ts;
                    self.gs = gs;
                    dev.ctm = ctm;
                }
            }
            "cm" => {
                let v: Vec<Option<f64>> = args.iter().map(f).collect();
                if v.iter().all(|x| x.is_some()) {
                    let m = (v[0].unwrap(), v[1].unwrap(), v[2].unwrap(), v[3].unwrap(), v[4].unwrap(), v[5].unwrap());
                    self.ctm = mult(m, self.ctm);
                    dev.ctm = self.ctm;
                }
            }
            "CS" => {
                if let Some(c) = self.csmap.get(&literal_name(&args[0])) {
                    self.gs.scs = c.clone();
                }
            }
            "cs" => {
                if let Some(c) = self.csmap.get(&literal_name(&args[0])) {
                    self.gs.ncs = c.clone();
                }
            }
            "G" => {
                if f(&args[0]).is_some() {
                    self.gs.scs = self.csmap["DeviceGray"].clone();
                }
            }
            "g" => {
                if f(&args[0]).is_some() {
                    self.gs.ncs = self.csmap["DeviceGray"].clone();
                }
            }
            "RG" => {
                if args.iter().all(|a| f(a).is_some()) {
                    self.gs.scs = self.csmap["DeviceRGB"].clone();
                }
            }
            "rg" => {
                if args.iter().all(|a| f(a).is_some()) {
                    self.gs.ncs = self.csmap["DeviceRGB"].clone();
                }
            }
            "K" => {
                if args.iter().all(|a| f(a).is_some()) {
                    self.gs.scs = self.csmap["DeviceCMYK"].clone();
                }
            }
            "k" => {
                if args.iter().all(|a| f(a).is_some()) {
                    self.gs.ncs = self.csmap["DeviceCMYK"].clone();
                }
            }
            "SCN" | "SC" => {
                let n = self.gs.scs.n;
                self.pop(n);
            }
            "scn" | "sc" => {
                let n = self.gs.ncs.n;
                self.pop(n);
            }
            "BT" => self.ts.reset(),
            "Tc" => {
                if let Some(x) = f(&args[0]) {
                    self.ts.charspace = x;
                }
            }
            "Tw" => {
                if let Some(x) = f(&args[0]) {
                    self.ts.wordspace = x;
                }
            }
            "Tz" => {
                if let Some(x) = f(&args[0]) {
                    self.ts.scaling = x;
                }
            }
            "TL" => {
                if let Some(x) = f(&args[0]) {
                    self.ts.leading = -x;
                }
            }
            "Tf" => {
                let key = literal_name(&args[0]);
                match self.fontmap.get(&key) {
                    Some(font) => self.ts.font = Some(font.clone()),
                    None => self.ts.font = Some(Font::from_spec(self.doc, &Dict::new())?),
                }
                if let Some(x) = f(&args[1]) {
                    self.ts.fontsize = x;
                }
            }
            "Ts" => {
                if let Some(x) = f(&args[0]) {
                    self.ts.rise = x;
                }
            }
            "Td" | "TD" => {
                let (tx, ty) = (f(&args[0]), f(&args[1]));
                if let (Some(tx), Some(ty)) = (tx, ty) {
                    let (a, b, c, d, e, ff) = self.ts.matrix;
                    self.ts.matrix = (a, b, c, d, tx * a + ty * c + e, tx * b + ty * d + ff);
                }
                if name == "TD" {
                    if let Some(ty) = ty {
                        self.ts.leading = ty;
                    }
                }
                self.ts.linematrix = (0.0, 0.0);
            }
            "Tm" => {
                let v: Vec<Option<f64>> = args.iter().map(f).collect();
                if v.iter().all(|x| x.is_some()) {
                    self.ts.matrix = (v[0].unwrap(), v[1].unwrap(), v[2].unwrap(), v[3].unwrap(), v[4].unwrap(), v[5].unwrap());
                    self.ts.linematrix = (0.0, 0.0);
                }
            }
            "T*" => self.t_star(),
            "TJ" => {
                let seq = match &args[0] {
                    Obj::Array(a) => a.clone(),
                    other => vec![other.clone()],
                };
                self.show(dev, &seq);
            }
            "Tj" => self.show(dev, &[args[0].clone()]),
            "'" => {
                self.t_star();
                self.show(dev, &[args[0].clone()]);
            }
            "\"" => {
                if let Some(x) = f(&args[0]) {
                    self.ts.wordspace = x;
                }
                if let Some(x) = f(&args[1]) {
                    self.ts.charspace = x;
                }
                self.show(dev, &[args[2].clone()]);
            }
            "Do" => self.do_xobject(dev, &literal_name(&args[0]))?,
            _ => {}
        }
        Ok(())
    }

    fn t_star(&mut self) {
        let (a, b, c, d, e, ff) = self.ts.matrix;
        let l = self.ts.leading;
        self.ts.matrix = (a, b, c, d, l * c + e, l * d + ff);
        self.ts.linematrix = (0.0, 0.0);
    }

    fn do_xobject(&mut self, dev: &mut Device, id: &str) -> Result<(), FontError> {
        let Some(x) = self.xobjmap.get(id).cloned() else { return Ok(()) };
        let s = match self.doc.resolve(&x) {
            Obj::Stream(s) => s,
            _ => return Ok(()),
        };
        let is_form = s.get("Subtype").map(|t| t.is_name("Form")).unwrap_or(false);
        if !is_form || !s.dict.contains_key("BBox") {
            return Ok(());
        }
        let matrix = match s.get("Matrix").map(|m| self.doc.resolve(m)) {
            Some(Obj::Array(a)) if a.len() == 6 => {
                let v: Vec<f64> = a.iter().map(|x| f(&self.doc.resolve(x)).unwrap_or(0.0)).collect();
                (v[0], v[1], v[2], v[3], v[4], v[5])
            }
            Some(Obj::Array(_)) => return Ok(()),
            _ => IDENTITY,
        };
        let resources = match s.get("Resources").map(|r| self.doc.resolve(r)) {
            Some(Obj::Dict(d)) => (*d).clone(),
            Some(_) => Dict::new(),
            None => self.resources.clone(),
        };
        let mut parent = self.parent_stream_ids.clone();
        parent.extend(self.stream_ids.iter().copied());
        let mut sub = Interp::new(self.doc, parent);
        let ctm = mult(matrix, self.ctm);
        sub.render_contents(dev, &resources, vec![Obj::Stream(s)], ctm)
        // The device keeps the form's CTM afterwards, as in pdfminer (until the next q/Q/cm).
    }

    fn show(&mut self, dev: &mut Device, seq: &[Obj]) {
        let Some(font) = self.ts.font.clone() else { return };
        let matrix = mult(self.ts.matrix, dev.ctm);
        let fontsize = self.ts.fontsize;
        let scaling = self.ts.scaling * 0.01;
        let charspace = self.ts.charspace * scaling;
        let mut wordspace = self.ts.wordspace * scaling;
        let rise = self.ts.rise;
        if font.is_multibyte() {
            wordspace = 0.0;
        }
        let dxscale = 0.001 * fontsize * scaling;
        let (mut x, mut y) = self.ts.linematrix;
        let mut needcharspace = false;
        for obj in seq {
            match obj {
                Obj::Int(_) | Obj::Real(_) | Obj::Bool(_) => {
                    let v = f(obj).unwrap();
                    if font.vertical {
                        y -= v * dxscale;
                    } else {
                        x -= v * dxscale;
                    }
                    needcharspace = true;
                }
                Obj::Str(bytes) => {
                    for cid in font.decode(bytes) {
                        if needcharspace {
                            if font.vertical {
                                y += charspace;
                            } else {
                                x += charspace;
                            }
                        }
                        let adv = render_char(dev, translate(matrix, x, y), &font, fontsize, scaling, rise, cid);
                        if font.vertical {
                            y += adv;
                        } else {
                            x += adv;
                        }
                        if cid == 32 && wordspace != 0.0 {
                            if font.vertical {
                                y += wordspace;
                            } else {
                                x += wordspace;
                            }
                        }
                        needcharspace = true;
                    }
                }
                _ => {}
            }
        }
        self.ts.linematrix = (x, y);
    }
}

fn render_char(dev: &mut Device, matrix: Matrix, font: &Font, fontsize: f64, scaling: f64, rise: f64, cid: u32) -> f64 {
    let text = font.to_unichr(cid).unwrap_or_else(|| format!("(cid:{cid})"));
    let textwidth = font.char_width(cid);
    let adv = textwidth * fontsize * scaling;
    let bbox = if font.vertical {
        let (vx, vy) = font.char_disp(cid);
        let vx = match vx {
            None => fontsize * 0.5,
            Some(v) => v * fontsize * 0.001,
        };
        let vy = (1000.0 - vy) * fontsize * 0.001;
        (-vx, vy + rise + adv, -vx + fontsize, vy + rise)
    } else {
        let descent = font.get_descent() * fontsize;
        (0.0, descent + rise, adv, descent + rise + fontsize)
    };
    let (a, b, c, d, _, _) = matrix;
    let upright = a * d * scaling > 0.0 && b * c <= 0.0;
    let (mut x0, mut y0, mut x1, mut y1) = apply_rect(matrix, bbox);
    if x1 < x0 {
        std::mem::swap(&mut x0, &mut x1);
    }
    if y1 < y0 {
        std::mem::swap(&mut y0, &mut y1);
    }
    let size = if font.vertical { x1 - x0 } else { y1 - y0 };
    dev.chars.push(RawChar { text, bbox: (x0, y0, x1, y1), upright, size, fontname: font.fontname.clone() });
    adv
}

fn rect(r: &dyn Resolver, o: Option<&Obj>) -> Option<(f64, f64, f64, f64)> {
    let a = match r.resolve(o?) {
        Obj::Array(a) => a,
        _ => return None,
    };
    if a.len() != 4 {
        return None;
    }
    let v: Vec<Option<f64>> = a.iter().map(|x| f(&r.resolve(x))).collect();
    if v.iter().any(|x| x.is_none()) {
        return None;
    }
    Some((v[0].unwrap(), v[1].unwrap(), v[2].unwrap(), v[3].unwrap()))
}

/// The characters of every page, in pdfplumber's coordinates (top-left origin).
pub fn page_chars(doc: &Document) -> Result<Vec<Vec<Char>>, FontError> {
    let mut dev = Device { ctm: IDENTITY, chars: vec![], fonts: HashMap::new() };
    let mut out = vec![];
    for (_id, attrs) in doc.pages() {
        let resources = match attrs.get("Resources").map(|r| doc.resolve(r)) {
            Some(Obj::Dict(d)) => (*d).clone(),
            _ => Dict::new(),
        };
        let mediabox = rect(doc, attrs.get("MediaBox")).unwrap_or((0.0, 0.0, 612.0, 792.0));
        let rotate_raw = match attrs.get("Rotate").map(|r| doc.resolve(r)) {
            Some(Obj::Int(i)) => i,
            _ => 0,
        };
        let rotate = (rotate_raw + 360).rem_euclid(360);
        let contents = match attrs.get("Contents").map(|c| doc.resolve(c)) {
            None | Some(Obj::Null) => vec![],
            Some(Obj::Array(a)) => a,
            Some(other) => vec![other],
        };
        let (x0, y0, x1, y1) = mediabox;
        let ctm = match rotate {
            90 => (0.0, -1.0, 1.0, 0.0, -y0, x1),
            180 => (-1.0, 0.0, 0.0, -1.0, x1, y1),
            270 => (0.0, 1.0, -1.0, 0.0, y1, -x0),
            _ => (1.0, 0.0, 0.0, 1.0, -x0, -y0),
        };
        dev.chars.clear();
        let mut interp = Interp::new(doc, HashSet::new());
        interp.render_contents(&mut dev, &resources, contents, ctm)?;

        // pdfplumber's page box: MediaBox sorted, swapped for 90/270, then flipped.
        let mb = rect(doc, attrs.get("MediaBox")).unwrap_or((0.0, 0.0, 612.0, 792.0));
        let (bx0, bx1) = if mb.0 <= mb.2 { (mb.0, mb.2) } else { (mb.2, mb.0) };
        let (by0, by1) = if mb.1 <= mb.3 { (mb.1, mb.3) } else { (mb.3, mb.1) };
        let pl_rot = rotate_raw.rem_euclid(360);
        let raw = if pl_rot == 90 || pl_rot == 270 { (by0, bx0, by1, bx1) } else { (bx0, by0, bx1, by1) };
        let mb_height = raw.3 - raw.1;
        let inv = (raw.0, mb_height - raw.3, raw.2, mb_height - raw.1);
        let height = inv.3 - inv.1;
        let (mb_x0, mb_top) = (inv.0, inv.1);
        let chars = dev
            .chars
            .drain(..)
            .map(|c| {
                let (cx0, cy0, cx1, cy1) = c.bbox;
                let (mut x0, mut x1) = (cx0, cx1);
                if mb_x0 != 0.0 {
                    x0 += mb_x0;
                    x1 += mb_x0;
                }
                Char {
                    text: c.text,
                    x0,
                    x1,
                    top: (height - cy1) + mb_top,
                    bottom: (height - cy0) + mb_top,
                    upright: c.upright,
                    size: c.size,
                    fontname: c.fontname,
                }
            })
            .collect();
        out.push(chars);
    }
    Ok(out)
}
