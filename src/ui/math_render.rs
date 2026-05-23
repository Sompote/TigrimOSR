//! LaTeX math equation renderer for egui.
//!
//! Parses a subset of LaTeX math and renders using egui's Painter with
//! proper superscripts, subscripts, fractions, square roots, matrices,
//! accents, boxes, and Greek/operator symbols.

use egui::{Color32, FontId, Pos2, Ui, Vec2};

// ── AST ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum MathNode {
    Sym(String),
    Row(Vec<MathNode>),
    Sup(Box<MathNode>, Box<MathNode>),
    Sub(Box<MathNode>, Box<MathNode>),
    SubSup(Box<MathNode>, Box<MathNode>, Box<MathNode>),
    Frac(Box<MathNode>, Box<MathNode>),
    Sqrt(Box<MathNode>),
    Matrix { rows: Vec<Vec<MathNode>>, left: char, right: char },
    Delimited(Box<MathNode>, char, char),
    Space(f32),
    Accent(Box<MathNode>, AccentKind),
    Boxed(Box<MathNode>),
    Underbrace(Box<MathNode>, Box<MathNode>), // content, label
    Overbrace(Box<MathNode>, Box<MathNode>),
}

#[derive(Debug, Clone, Copy)]
enum AccentKind { Dot, DDot, Hat, Bar, Vec, Tilde, Check }

// ── Layout metrics ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy)]
struct Met { w: f32, asc: f32, desc: f32 }

impl Met {
    fn h(&self) -> f32 { self.asc + self.desc }
    fn zero() -> Self { Self { w: 0.0, asc: 0.0, desc: 0.0 } }
}

// ── Symbol table ────────────────────────────────────────────────────────

fn latex_sym(cmd: &str) -> Option<&'static str> {
    Some(match cmd {
        "alpha" => "α", "beta" => "β", "gamma" => "γ", "delta" => "δ",
        "epsilon"|"varepsilon" => "ε", "zeta" => "ζ", "eta" => "η",
        "theta"|"vartheta" => "θ", "iota" => "ι", "kappa" => "κ",
        "lambda" => "λ", "mu" => "μ", "nu" => "ν", "xi" => "ξ",
        "pi" => "π", "rho"|"varrho" => "ρ", "sigma" => "σ",
        "tau" => "τ", "upsilon" => "υ", "phi"|"varphi" => "φ",
        "chi" => "χ", "psi" => "ψ", "omega" => "ω",
        "Gamma" => "Γ", "Delta" => "Δ", "Theta" => "Θ", "Lambda" => "Λ",
        "Xi" => "Ξ", "Pi" => "Π", "Sigma" => "Σ", "Upsilon" => "Υ",
        "Phi" => "Φ", "Psi" => "Ψ", "Omega" => "Ω",
        "times" => "×", "cdot" => "·", "pm" => "±", "mp" => "∓",
        "div" => "÷", "ast" => "∗", "circ" => "∘", "bullet" => "•",
        "leq"|"le" => "≤", "geq"|"ge" => "≥", "neq"|"ne" => "≠",
        "approx" => "≈", "equiv" => "≡", "sim" => "∼", "propto" => "∝",
        "ll" => "≪", "gg" => "≫",
        "rightarrow"|"to" => "→", "leftarrow"|"gets" => "←",
        "leftrightarrow" => "↔",
        "Rightarrow"|"implies" => "⇒", "Leftarrow" => "⇐",
        "Leftrightarrow"|"iff" => "⇔", "mapsto" => "↦",
        "uparrow" => "↑", "downarrow" => "↓",
        "sum" => "∑", "prod" => "∏", "int" => "∫",
        "iint" => "∬", "iiint" => "∭", "oint" => "∮",
        "bigcup" => "⋃", "bigcap" => "⋂",
        "in" => "∈", "notin" => "∉", "ni" => "∋",
        "subset" => "⊂", "supset" => "⊃",
        "subseteq" => "⊆", "supseteq" => "⊇",
        "cup" => "∪", "cap" => "∩",
        "emptyset"|"varnothing" => "∅",
        "forall" => "∀", "exists" => "∃",
        "neg"|"lnot" => "¬", "land"|"wedge" => "∧", "lor"|"vee" => "∨",
        "infty" => "∞", "partial" => "∂", "nabla" => "∇",
        "hbar" => "ℏ", "ell" => "ℓ", "Re" => "ℜ", "Im" => "ℑ",
        "aleph" => "ℵ",
        "dots"|"ldots" => "…", "cdots" => "⋯", "vdots" => "⋮", "ddots" => "⋱",
        "prime" => "′",
        "langle" => "⟨", "rangle" => "⟩",
        "lfloor" => "⌊", "rfloor" => "⌋",
        "lceil" => "⌈", "rceil" => "⌉",
        "lvert"|"vert" => "|", "rvert" => "|",
        "lVert"|"Vert" => "‖", "rVert" => "‖",
        "quad" => "\u{2003}", "qquad" => "\u{2003}\u{2003}",
        "," => "\u{2009}", ";" => "\u{2005}", "!" => "",
        "log" => "log", "ln" => "ln", "sin" => "sin", "cos" => "cos",
        "tan" => "tan", "exp" => "exp", "lim" => "lim",
        "min" => "min", "max" => "max", "sup" => "sup", "inf" => "inf",
        "det" => "det", "dim" => "dim", "ker" => "ker",
        "arg" => "arg", "deg" => "deg", "gcd" => "gcd",
        "arcsin" => "arcsin", "arccos" => "arccos", "arctan" => "arctan",
        "sinh" => "sinh", "cosh" => "cosh", "tanh" => "tanh",
        "sec" => "sec", "csc" => "csc", "cot" => "cot",
        _ => return None,
    })
}

fn mathbb_char(c: char) -> Option<char> {
    Some(match c {
        'A' => '𝔸', 'B' => '𝔹', 'C' => 'ℂ', 'D' => '𝔻', 'E' => '𝔼',
        'F' => '𝔽', 'G' => '𝔾', 'H' => 'ℍ', 'I' => '𝕀', 'J' => '𝕁',
        'K' => '𝕂', 'L' => '𝕃', 'M' => '𝕄', 'N' => 'ℕ', 'O' => '𝕆',
        'P' => 'ℙ', 'Q' => 'ℚ', 'R' => 'ℝ', 'S' => '𝕊', 'T' => '𝕋',
        'U' => '𝕌', 'V' => '𝕍', 'W' => '𝕎', 'X' => '𝕏', 'Y' => '𝕐',
        'Z' => 'ℤ',
        '0' => '𝟘', '1' => '𝟙', '2' => '𝟚', '3' => '𝟛', '4' => '𝟜',
        '5' => '𝟝', '6' => '𝟞', '7' => '𝟟', '8' => '𝟠', '9' => '𝟡',
        _ => return None,
    })
}

// ── Parser ──────────────────────────────────────────────────────────────

struct P { ch: Vec<char>, i: usize }

impl P {
    fn new(s: &str) -> Self { Self { ch: s.chars().collect(), i: 0 } }
    fn peek(&self) -> Option<char> { self.ch.get(self.i).copied() }
    fn adv(&mut self) -> Option<char> { let c = self.peek(); if c.is_some() { self.i += 1; } c }
    fn eat(&mut self, c: char) -> bool { if self.peek() == Some(c) { self.i += 1; true } else { false } }
    fn ws(&mut self) { while matches!(self.peek(), Some(' '|'\t'|'\n')) { self.adv(); } }

    fn alpha(&mut self) -> String {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphabetic() { s.push(c); self.adv(); } else { break; }
        }
        s
    }

    fn expr(&mut self) -> Vec<MathNode> {
        let mut v = Vec::new();
        while let Some(a) = self.atom() {
            v.push(self.scripts(a));
        }
        v
    }

    fn atom(&mut self) -> Option<MathNode> {
        match self.peek()? {
            '}' | '&' => None,
            '{' => { self.adv(); Some(self.braced()) }
            '\\' => self.backslash(),
            ' '|'\t'|'\n' => { self.adv(); Some(MathNode::Space(0.17)) }
            c => { self.adv(); Some(MathNode::Sym(c.to_string())) }
        }
    }

    fn braced(&mut self) -> MathNode {
        let n = self.expr();
        self.eat('}');
        flat(n)
    }

    fn req(&mut self) -> MathNode {
        self.ws();
        if self.eat('{') { self.braced() }
        else if let Some(a) = self.atom() { a }
        else { MathNode::Sym("?".into()) }
    }

    fn scripts(&mut self, base: MathNode) -> MathNode {
        let (mut sub, mut sup) = (None, None);
        for _ in 0..2 {
            self.ws();
            match self.peek() {
                Some('^') if sup.is_none() => { self.adv(); sup = Some(Box::new(self.req())); }
                Some('_') if sub.is_none() => { self.adv(); sub = Some(Box::new(self.req())); }
                _ => break,
            }
        }
        match (sub, sup) {
            (None, None) => base,
            (None, Some(s)) => MathNode::Sup(Box::new(base), s),
            (Some(s), None) => MathNode::Sub(Box::new(base), s),
            (Some(b), Some(p)) => MathNode::SubSup(Box::new(base), b, p),
        }
    }

    fn backslash(&mut self) -> Option<MathNode> {
        self.adv(); // skip '\'
        if self.peek() == Some('\\') { self.adv(); return Some(MathNode::Space(0.5)); }
        if let Some(c) = self.peek() {
            if !c.is_ascii_alphabetic() {
                self.adv();
                return Some(match c {
                    ',' => MathNode::Space(0.17),
                    ';' => MathNode::Space(0.28),
                    '!' => MathNode::Space(0.0),
                    '{' => MathNode::Sym("{".into()),
                    '}' => MathNode::Sym("}".into()),
                    ' ' => MathNode::Space(0.25),
                    '|' => MathNode::Sym("‖".into()),
                    _ => MathNode::Sym(c.to_string()),
                });
            }
        }
        let cmd = self.alpha();
        match cmd.as_str() {
            "frac"|"dfrac"|"tfrac" => {
                let n = self.req(); let d = self.req();
                Some(MathNode::Frac(Box::new(n), Box::new(d)))
            }
            "sqrt" => {
                // Handle optional \sqrt[n]{x}
                self.ws();
                if self.peek() == Some('[') {
                    self.adv();
                    // skip the index for now
                    while self.peek().is_some() && self.peek() != Some(']') { self.adv(); }
                    self.eat(']');
                }
                let a = self.req();
                Some(MathNode::Sqrt(Box::new(a)))
            }
            // Accent commands
            "dot" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::Dot)) }
            "ddot" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::DDot)) }
            "hat"|"widehat" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::Hat)) }
            "bar"|"overline" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::Bar)) }
            "vec"|"overrightarrow" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::Vec)) }
            "tilde"|"widetilde" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::Tilde)) }
            "check" => { let a = self.req(); Some(MathNode::Accent(Box::new(a), AccentKind::Check)) }
            // Boxed
            "boxed" => { let a = self.req(); Some(MathNode::Boxed(Box::new(a))) }
            // Underbrace / overbrace
            "underbrace" => {
                let content = self.req();
                // The label is typically given as _{label} after underbrace
                self.ws();
                let label = if self.peek() == Some('_') {
                    self.adv();
                    self.req()
                } else {
                    MathNode::Sym(String::new())
                };
                Some(MathNode::Underbrace(Box::new(content), Box::new(label)))
            }
            "overbrace" => {
                let content = self.req();
                self.ws();
                let label = if self.peek() == Some('^') {
                    self.adv();
                    self.req()
                } else {
                    MathNode::Sym(String::new())
                };
                Some(MathNode::Overbrace(Box::new(content), Box::new(label)))
            }
            // Font commands
            "text"|"mathrm"|"textrm"|"operatorname"|"mathit"|"textbf"|"boldsymbol" => {
                Some(self.req())
            }
            "mathbf" => { Some(self.req()) }
            "mathbb" => {
                let inner = self.req();
                // Convert to double-struck Unicode
                if let MathNode::Sym(ref s) = inner {
                    let converted: String = s.chars()
                        .map(|c| mathbb_char(c).unwrap_or(c))
                        .collect();
                    Some(MathNode::Sym(converted))
                } else {
                    Some(inner)
                }
            }
            "mathcal" => { Some(self.req()) } // just render content
            // Delimiters
            "left" => Some(self.left_right()),
            "right" => {
                let c = self.adv().unwrap_or(')');
                Some(MathNode::Sym(c.to_string()))
            }
            // Environments
            "begin" => { self.eat('{'); let e = self.alpha(); self.eat('}'); self.env(&e) }
            // Known symbols (checked via table)
            _ => {
                if let Some(s) = latex_sym(&cmd) {
                    if s.is_empty() { Some(MathNode::Space(0.0)) } else { Some(MathNode::Sym(s.into())) }
                } else {
                    // Unknown command — render name in upright
                    Some(MathNode::Sym(cmd))
                }
            }
        }
    }

    fn left_right(&mut self) -> MathNode {
        let lc = self.adv().unwrap_or('(');
        let ld = if lc == '.' { ' ' } else { lc };
        let start = self.i;
        let mut depth = 1;
        while self.i < self.ch.len() {
            if self.ch[self.i] == '\\' && self.i + 1 < self.ch.len() {
                let cs = self.i + 1;
                let mut ce = cs;
                while ce < self.ch.len() && self.ch[ce].is_ascii_alphabetic() { ce += 1; }
                let w: String = self.ch[cs..ce].iter().collect();
                if w == "left" { depth += 1; self.i = ce; continue; }
                if w == "right" {
                    depth -= 1;
                    if depth == 0 {
                        let inner: String = self.ch[start..self.i].iter().collect();
                        self.i = ce;
                        let rc = self.adv().unwrap_or(')');
                        let rd = if rc == '.' { ' ' } else { rc };
                        let n = P::new(&inner).top();
                        return MathNode::Delimited(Box::new(n), ld, rd);
                    }
                    self.i = ce; continue;
                }
                self.i = ce; continue;
            }
            self.i += 1;
        }
        let inner: String = self.ch[start..].iter().collect();
        MathNode::Delimited(Box::new(P::new(&inner).top()), ld, ' ')
    }

    fn env(&mut self, name: &str) -> Option<MathNode> {
        let (l, r) = match name {
            "bmatrix" => ('[', ']'), "pmatrix" => ('(', ')'),
            "vmatrix" => ('|', '|'), "Bmatrix" => ('{', '}'),
            "Vmatrix" => ('‖', '‖'), "matrix" => (' ', ' '),
            "cases" => ('{', ' '), "array" => (' ', ' '),
            "aligned"|"align"|"gather" => (' ', ' '),
            _ => (' ', ' '),
        };
        let end_m = format!("\\end{{{}}}", name);
        let src: String = self.ch[self.i..].iter().collect();
        let ep = src.find(&end_m).unwrap_or(src.len());
        let content = &src[..ep];
        self.i += ep + end_m.len();

        let mut rows: Vec<Vec<MathNode>> = Vec::new();
        for row_s in content.split("\\\\") {
            let t = row_s.trim();
            if t.is_empty() { continue; }
            let cells: Vec<MathNode> = t.split('&').map(|c| P::new(c.trim()).top()).collect();
            rows.push(cells);
        }
        Some(MathNode::Matrix { rows, left: l, right: r })
    }

    fn top(mut self) -> MathNode { let n = self.expr(); flat(n) }
}

fn flat(v: Vec<MathNode>) -> MathNode {
    match v.len() {
        0 => MathNode::Sym(String::new()),
        1 => v.into_iter().next().unwrap(),
        _ => MathNode::Row(v),
    }
}

fn parse_latex(input: &str) -> MathNode { P::new(input).top() }

// ── Constants ───────────────────────────────────────────────────────────

const SR: f32 = 0.7;
const FG: f32 = 3.0;
const FL: f32 = 1.0;
const SQ_OH: f32 = 2.0;
const SQ_LN: f32 = 1.5;
const MC: f32 = 14.0;
const MR: f32 = 6.0;
const DE: f32 = 3.0;
const ACC_H: f32 = 0.25; // accent extra height ratio

// ── Measure ─────────────────────────────────────────────────────────────

fn font(sz: f32) -> FontId { FontId::proportional(sz) }

fn mtxt(ui: &Ui, s: &str, sz: f32) -> Met {
    if s.is_empty() { return Met { w: 0.0, asc: sz * 0.7, desc: sz * 0.3 }; }
    let g = ui.fonts(|f| f.layout_no_wrap(s.to_string(), font(sz), Color32::BLACK));
    let (w, h) = (g.size().x, g.size().y);
    Met { w, asc: h * 0.75, desc: h * 0.25 }
}

fn meas(n: &MathNode, ui: &Ui, sz: f32) -> Met {
    match n {
        MathNode::Sym(s) => mtxt(ui, s, sz),
        MathNode::Space(em) => Met { w: sz * em, asc: sz * 0.7, desc: sz * 0.3 },
        MathNode::Row(ch) => {
            let mut tw = 0.0; let mut ma = 0.0_f32; let mut md = 0.0_f32;
            for c in ch { let m = meas(c, ui, sz); tw += m.w; ma = ma.max(m.asc); md = md.max(m.desc); }
            Met { w: tw, asc: ma, desc: md }
        }
        MathNode::Sup(b, s) => {
            let bm = meas(b, ui, sz); let sm = meas(s, ui, sz * SR);
            Met { w: bm.w + sm.w + 1.0, asc: bm.asc.max(bm.asc * 0.5 + sm.asc), desc: bm.desc }
        }
        MathNode::Sub(b, s) => {
            let bm = meas(b, ui, sz); let sm = meas(s, ui, sz * SR);
            let sd = bm.desc * 0.3 + sm.asc * 0.5;
            Met { w: bm.w + sm.w + 1.0, asc: bm.asc, desc: bm.desc.max(sd + sm.desc) }
        }
        MathNode::SubSup(b, sub, sup) => {
            let bm = meas(b, ui, sz);
            let ssub = meas(sub, ui, sz * SR); let ssup = meas(sup, ui, sz * SR);
            let sw = ssub.w.max(ssup.w);
            let a = bm.asc.max(bm.asc * 0.5 + ssup.asc);
            let sd = bm.desc * 0.3 + ssub.asc * 0.5;
            Met { w: bm.w + sw + 1.0, asc: a, desc: bm.desc.max(sd + ssub.desc) }
        }
        MathNode::Frac(nu, de) => {
            let nm = meas(nu, ui, sz * 0.85); let dm = meas(de, ui, sz * 0.85);
            let w = nm.w.max(dm.w) + 6.0;
            Met { w, asc: nm.h() + FG + FL, desc: dm.h() + FG }
        }
        MathNode::Sqrt(inner) => {
            let im = meas(inner, ui, sz);
            let sw = sz * 0.55;
            Met { w: sw + im.w + 2.0, asc: im.asc + SQ_OH + SQ_LN, desc: im.desc }
        }
        MathNode::Matrix { rows, left, right } => {
            if rows.is_empty() { return Met::zero(); }
            let nc = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            let cs = sz * 0.85;
            let mut cw = vec![0.0_f32; nc];
            let mut rh = Vec::new();
            for row in rows {
                let mut h = 0.0_f32;
                for (j, cell) in row.iter().enumerate() {
                    let cm = meas(cell, ui, cs);
                    if j < nc { cw[j] = cw[j].max(cm.w); }
                    h = h.max(cm.h());
                }
                rh.push(h);
            }
            let tw: f32 = cw.iter().sum::<f32>() + (nc as f32 - 1.0).max(0.0) * MC;
            let th: f32 = rh.iter().sum::<f32>() + (rows.len() as f32 - 1.0).max(0.0) * MR;
            let dw = if *left != ' ' || *right != ' ' { sz * 0.35 } else { 0.0 };
            Met { w: tw + dw * 2.0 + 8.0, asc: th / 2.0 + sz * 0.2, desc: th / 2.0 - sz * 0.2 }
        }
        MathNode::Delimited(inner, _l, _r) => {
            let im = meas(inner, ui, sz);
            let bw = 7.0;
            Met { w: im.w + bw * 2.0, asc: im.asc + DE, desc: im.desc + DE }
        }
        MathNode::Accent(inner, _kind) => {
            let im = meas(inner, ui, sz);
            Met { w: im.w.max(sz * 0.5), asc: im.asc + sz * ACC_H, desc: im.desc }
        }
        MathNode::Boxed(inner) => {
            let im = meas(inner, ui, sz);
            Met { w: im.w + 8.0, asc: im.asc + 4.0, desc: im.desc + 4.0 }
        }
        MathNode::Underbrace(content, label) => {
            let cm = meas(content, ui, sz);
            let lm = meas(label, ui, sz * 0.7);
            Met { w: cm.w.max(lm.w), asc: cm.asc, desc: cm.desc + lm.h() + sz * 0.35 }
        }
        MathNode::Overbrace(content, label) => {
            let cm = meas(content, ui, sz);
            let lm = meas(label, ui, sz * 0.7);
            Met { w: cm.w.max(lm.w), asc: cm.asc + lm.h() + sz * 0.35, desc: cm.desc }
        }
    }
}

// ── Render ──────────────────────────────────────────────────────────────

fn draw(n: &MathNode, p: &egui::Painter, x: f32, bl: f32, sz: f32, col: Color32, ui: &Ui) -> f32 {
    match n {
        MathNode::Sym(s) => {
            if s.is_empty() { return 0.0; }
            let g = ui.fonts(|f| f.layout_no_wrap(s.clone(), font(sz), col));
            let h = g.size().y;
            let w = g.size().x;
            let top = bl - h * 0.75;
            p.galley(Pos2::new(x, top), g, col);
            w
        }
        MathNode::Space(em) => sz * em,
        MathNode::Row(ch) => {
            let mut cx = x;
            for c in ch { cx += draw(c, p, cx, bl, sz, col, ui); }
            cx - x
        }
        MathNode::Sup(base, sup) => {
            let bm = meas(base, ui, sz);
            let bw = draw(base, p, x, bl, sz, col, ui);
            let ss = sz * SR;
            let sbl = bl - bm.asc * 0.5;
            let sw = draw(sup, p, x + bw + 1.0, sbl, ss, col, ui);
            bw + sw + 1.0
        }
        MathNode::Sub(base, sub) => {
            let bm = meas(base, ui, sz);
            let bw = draw(base, p, x, bl, sz, col, ui);
            let ss = sz * SR;
            let sbl = bl + bm.desc * 0.3 + ss * 0.5;
            let sw = draw(sub, p, x + bw + 1.0, sbl, ss, col, ui);
            bw + sw + 1.0
        }
        MathNode::SubSup(base, sub, sup) => {
            let bm = meas(base, ui, sz);
            let bw = draw(base, p, x, bl, sz, col, ui);
            let ss = sz * SR;
            let sup_bl = bl - bm.asc * 0.5;
            let sub_bl = bl + bm.desc * 0.3 + ss * 0.5;
            let w1 = draw(sup, p, x + bw + 1.0, sup_bl, ss, col, ui);
            let w2 = draw(sub, p, x + bw + 1.0, sub_bl, ss, col, ui);
            bw + w1.max(w2) + 1.0
        }
        MathNode::Frac(nu, de) => {
            let fs = sz * 0.85;
            let nm = meas(nu, ui, fs); let dm = meas(de, ui, fs);
            let tw = nm.w.max(dm.w) + 6.0;
            let line_y = bl - sz * 0.1;
            let nx = x + (tw - nm.w) / 2.0;
            let nbl = line_y - FG - FL - nm.desc;
            draw(nu, p, nx, nbl, fs, col, ui);
            p.line_segment([Pos2::new(x + 1.0, line_y), Pos2::new(x + tw - 1.0, line_y)],
                egui::Stroke::new(FL, col));
            let dx = x + (tw - dm.w) / 2.0;
            let dbl = line_y + FG + dm.asc;
            draw(de, p, dx, dbl, fs, col, ui);
            tw
        }
        MathNode::Sqrt(inner) => {
            let im = meas(inner, ui, sz);
            let sw = sz * 0.55;
            let top = bl - im.asc - SQ_OH;
            let bot = bl + im.desc;
            let stroke = egui::Stroke::new(SQ_LN, col);
            let pts = [
                Pos2::new(x, bl - im.asc * 0.3),
                Pos2::new(x + sw * 0.3, bl - im.asc * 0.3),
                Pos2::new(x + sw * 0.5, bot),
                Pos2::new(x + sw, top),
                Pos2::new(x + sw + im.w + 2.0, top),
            ];
            for w in pts.windows(2) { p.line_segment([w[0], w[1]], stroke); }
            draw(inner, p, x + sw + 1.0, bl, sz, col, ui);
            sw + im.w + 2.0
        }
        MathNode::Matrix { rows, left, right } => {
            if rows.is_empty() { return 0.0; }
            let nc = rows.iter().map(|r| r.len()).max().unwrap_or(0);
            let cs = sz * 0.85;
            let mut cw = vec![0.0_f32; nc];
            let mut rm: Vec<Met> = Vec::new();
            for row in rows {
                let mut rmet = Met::zero();
                for (j, cell) in row.iter().enumerate() {
                    let cm = meas(cell, ui, cs);
                    if j < nc { cw[j] = cw[j].max(cm.w); }
                    rmet.asc = rmet.asc.max(cm.asc);
                    rmet.desc = rmet.desc.max(cm.desc);
                }
                rm.push(rmet);
            }
            let th: f32 = rm.iter().map(|m| m.h()).sum::<f32>()
                + (rows.len() as f32 - 1.0).max(0.0) * MR;
            let has_d = *left != ' ' || *right != ' ';
            let dw = if has_d { sz * 0.35 } else { 0.0 };
            let cx0 = x + dw + 4.0;
            let mut cy = bl - th / 2.0;
            for (i, row) in rows.iter().enumerate() {
                let cbl = cy + rm[i].asc;
                let mut cx = cx0;
                for (j, cell) in row.iter().enumerate() {
                    draw(cell, p, cx, cbl, cs, col, ui);
                    if j < nc { cx += cw[j] + MC; }
                }
                cy += rm[i].h() + MR;
            }
            let tw: f32 = cw.iter().sum::<f32>() + (nc as f32 - 1.0).max(0.0) * MC;
            if has_d {
                let top = bl - th / 2.0 - 3.0;
                let bot = bl + th / 2.0 + 3.0;
                let s = egui::Stroke::new(1.5, col);
                if *left != ' ' { draw_bracket(p, *left, x + 2.0, top, bot, s, true); }
                if *right != ' ' { draw_bracket(p, *right, cx0 + tw + 2.0, top, bot, s, false); }
            }
            tw + dw * 2.0 + 8.0
        }
        MathNode::Delimited(inner, l, r) => {
            let im = meas(inner, ui, sz);
            let bw = 7.0; // bracket width allocation
            let top = bl - im.asc - DE;
            let bot = bl + im.desc + DE;
            let s = egui::Stroke::new(1.5, col);
            if *l != ' ' { draw_bracket(p, *l, x, top, bot, s, true); }
            draw(inner, p, x + bw, bl, sz, col, ui);
            if *r != ' ' { draw_bracket(p, *r, x + bw + im.w, top, bot, s, false); }
            im.w + bw * 2.0
        }
        MathNode::Accent(inner, kind) => {
            let im = meas(inner, ui, sz);
            let w = draw(inner, p, x, bl, sz, col, ui);
            let cw = w.max(sz * 0.5);
            let cx = x + cw / 2.0;
            let ay = bl - im.asc - sz * 0.08;
            let stroke = egui::Stroke::new(1.2, col);
            match kind {
                AccentKind::Dot => {
                    p.circle_filled(Pos2::new(cx, ay), 1.5, col);
                }
                AccentKind::DDot => {
                    p.circle_filled(Pos2::new(cx - 3.0, ay), 1.3, col);
                    p.circle_filled(Pos2::new(cx + 3.0, ay), 1.3, col);
                }
                AccentKind::Hat => {
                    let hw = (cw * 0.4).min(6.0);
                    p.line_segment([Pos2::new(cx - hw, ay + 2.0), Pos2::new(cx, ay - 2.0)], stroke);
                    p.line_segment([Pos2::new(cx, ay - 2.0), Pos2::new(cx + hw, ay + 2.0)], stroke);
                }
                AccentKind::Bar => {
                    let hw = (cw * 0.45).min(8.0);
                    p.line_segment([Pos2::new(cx - hw, ay), Pos2::new(cx + hw, ay)], stroke);
                }
                AccentKind::Vec => {
                    let hw = (cw * 0.45).min(8.0);
                    p.line_segment([Pos2::new(cx - hw, ay), Pos2::new(cx + hw, ay)], stroke);
                    p.line_segment([Pos2::new(cx + hw - 3.0, ay - 2.5), Pos2::new(cx + hw, ay)], stroke);
                    p.line_segment([Pos2::new(cx + hw - 3.0, ay + 2.5), Pos2::new(cx + hw, ay)], stroke);
                }
                AccentKind::Tilde => {
                    let hw = (cw * 0.4).min(6.0);
                    let steps = 10;
                    let mut pts = Vec::with_capacity(steps + 1);
                    for i in 0..=steps {
                        let t = i as f32 / steps as f32;
                        let px = cx - hw + t * hw * 2.0;
                        let py = ay + (t * std::f32::consts::PI * 2.0).sin() * 2.0;
                        pts.push(Pos2::new(px, py));
                    }
                    for pair in pts.windows(2) { p.line_segment([pair[0], pair[1]], stroke); }
                }
                AccentKind::Check => {
                    let hw = (cw * 0.35).min(5.0);
                    p.line_segment([Pos2::new(cx - hw, ay - 2.0), Pos2::new(cx, ay + 2.0)], stroke);
                    p.line_segment([Pos2::new(cx, ay + 2.0), Pos2::new(cx + hw, ay - 2.0)], stroke);
                }
            }
            cw
        }
        MathNode::Boxed(inner) => {
            let im = meas(inner, ui, sz);
            let pad = 4.0;
            let rect = egui::Rect::from_min_size(
                Pos2::new(x, bl - im.asc - pad),
                Vec2::new(im.w + pad * 2.0, im.h() + pad * 2.0),
            );
            p.rect_stroke(rect, 2.0, egui::Stroke::new(1.2, col), egui::StrokeKind::Outside);
            draw(inner, p, x + pad, bl, sz, col, ui);
            im.w + pad * 2.0
        }
        MathNode::Underbrace(content, label) => {
            let cm = meas(content, ui, sz);
            let cw = draw(content, p, x, bl, sz, col, ui);
            let brace_y = bl + cm.desc + sz * 0.08;
            let stroke = egui::Stroke::new(1.0, col);
            // Draw horizontal brace: ⏟
            let mid_x = x + cw / 2.0;
            let tip_y = brace_y + sz * 0.12;
            p.line_segment([Pos2::new(x + 2.0, brace_y), Pos2::new(mid_x, tip_y)], stroke);
            p.line_segment([Pos2::new(mid_x, tip_y), Pos2::new(x + cw - 2.0, brace_y)], stroke);
            // Draw label below
            let lsz = sz * 0.7;
            let lm = meas(label, ui, lsz);
            let lx = x + (cw - lm.w) / 2.0;
            let lbl = tip_y + sz * 0.12 + lm.asc;
            draw(label, p, lx, lbl, lsz, col, ui);
            cw
        }
        MathNode::Overbrace(content, label) => {
            let cm = meas(content, ui, sz);
            let cw = draw(content, p, x, bl, sz, col, ui);
            let brace_y = bl - cm.asc - sz * 0.08;
            let stroke = egui::Stroke::new(1.0, col);
            let mid_x = x + cw / 2.0;
            let tip_y = brace_y - sz * 0.12;
            p.line_segment([Pos2::new(x + 2.0, brace_y), Pos2::new(mid_x, tip_y)], stroke);
            p.line_segment([Pos2::new(mid_x, tip_y), Pos2::new(x + cw - 2.0, brace_y)], stroke);
            let lsz = sz * 0.7;
            let lm = meas(label, ui, lsz);
            let lx = x + (cw - lm.w) / 2.0;
            let lbl = tip_y - sz * 0.06 - lm.desc;
            draw(label, p, lx, lbl, lsz, col, ui);
            cw
        }
    }
}

fn draw_bracket(p: &egui::Painter, ch: char, x: f32, top: f32, bot: f32, s: egui::Stroke, _left: bool) {
    let bw = 6.0;
    match ch {
        '(' => {
            // ( shape: leftmost at middle, rightmost at top/bottom
            let mid = (top + bot) / 2.0;
            let half = ((bot - top) / 2.0).max(1.0);
            let steps = 18;
            let mut pts = Vec::with_capacity(steps + 1);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let y = top + t * (bot - top);
                let norm = (y - mid) / half;
                let dx = bw * norm * norm; // 0 at middle, bw at edges
                pts.push(Pos2::new(x + dx, y));
            }
            for w in pts.windows(2) { p.line_segment([w[0], w[1]], s); }
        }
        ')' => {
            // ) shape: rightmost at middle, leftmost at top/bottom
            let mid = (top + bot) / 2.0;
            let half = ((bot - top) / 2.0).max(1.0);
            let steps = 18;
            let mut pts = Vec::with_capacity(steps + 1);
            for i in 0..=steps {
                let t = i as f32 / steps as f32;
                let y = top + t * (bot - top);
                let norm = (y - mid) / half;
                let dx = bw * (1.0 - norm * norm); // bw at middle, 0 at edges
                pts.push(Pos2::new(x + dx, y));
            }
            for w in pts.windows(2) { p.line_segment([w[0], w[1]], s); }
        }
        '[' => {
            // [ shape: top cap left, vertical left, bottom cap left
            p.line_segment([Pos2::new(x + bw, top), Pos2::new(x, top)], s);
            p.line_segment([Pos2::new(x, top), Pos2::new(x, bot)], s);
            p.line_segment([Pos2::new(x, bot), Pos2::new(x + bw, bot)], s);
        }
        ']' => {
            // ] shape: top cap right, vertical right, bottom cap right
            p.line_segment([Pos2::new(x, top), Pos2::new(x + bw, top)], s);
            p.line_segment([Pos2::new(x + bw, top), Pos2::new(x + bw, bot)], s);
            p.line_segment([Pos2::new(x + bw, bot), Pos2::new(x, bot)], s);
        }
        '{' => {
            // { shape: tip points LEFT at middle
            let mid = (top + bot) / 2.0;
            let q1 = top + (mid - top) * 0.5;
            let q3 = bot - (bot - mid) * 0.5;
            let r = x + bw;       // right edge (start/end)
            let l = x;            // left edge (tip)
            let m = (l + r) / 2.0;
            p.line_segment([Pos2::new(r, top), Pos2::new(m, q1)], s);
            p.line_segment([Pos2::new(m, q1), Pos2::new(l, mid)], s);
            p.line_segment([Pos2::new(l, mid), Pos2::new(m, q3)], s);
            p.line_segment([Pos2::new(m, q3), Pos2::new(r, bot)], s);
        }
        '}' => {
            // } shape: tip points RIGHT at middle
            let mid = (top + bot) / 2.0;
            let q1 = top + (mid - top) * 0.5;
            let q3 = bot - (bot - mid) * 0.5;
            let l = x;            // left edge (start/end)
            let r = x + bw;       // right edge (tip)
            let m = (l + r) / 2.0;
            p.line_segment([Pos2::new(l, top), Pos2::new(m, q1)], s);
            p.line_segment([Pos2::new(m, q1), Pos2::new(r, mid)], s);
            p.line_segment([Pos2::new(r, mid), Pos2::new(m, q3)], s);
            p.line_segment([Pos2::new(m, q3), Pos2::new(l, bot)], s);
        }
        '|' => {
            p.line_segment([Pos2::new(x + bw / 2.0, top), Pos2::new(x + bw / 2.0, bot)], s);
        }
        '‖' => {
            p.line_segment([Pos2::new(x + bw * 0.3, top), Pos2::new(x + bw * 0.3, bot)], s);
            p.line_segment([Pos2::new(x + bw * 0.7, top), Pos2::new(x + bw * 0.7, bot)], s);
        }
        _ => {}
    }
}

// ── Public API ──────────────────────────────────────────────────────────

/// Render a LaTeX math equation in the UI.
pub fn render_math_equation(ui: &mut Ui, latex: &str, display: bool, text_color: Color32) {
    let node = parse_latex(latex);
    let base_sz = if display { 17.0 } else { 14.0 };
    let m = meas(&node, ui, base_sz);

    if display {
        ui.add_space(6.0);
        let avail = ui.available_width();
        let margin = ((avail - m.w) / 2.0).max(8.0);
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(avail, m.h() + 12.0), egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 6.0, Color32::from_rgba_premultiplied(88, 166, 255, 10));
        painter.rect_stroke(rect, 6.0, egui::Stroke::new(0.5, Color32::from_rgb(200, 215, 230)), egui::StrokeKind::Outside);
        let bl = rect.min.y + m.asc + 6.0;
        draw(&node, &painter, rect.min.x + margin, bl, base_sz, text_color, ui);
        ui.add_space(6.0);
    } else {
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(m.w + 4.0, m.h() + 2.0), egui::Sense::hover(),
        );
        let painter = ui.painter_at(rect);
        let bl = rect.min.y + m.asc + 1.0;
        draw(&node, &painter, rect.min.x + 2.0, bl, base_sz, text_color, ui);
    }
}

/// Split text into segments of (is_math, content) for inline $...$ rendering.
pub fn split_inline_math(text: &str) -> Vec<(bool, String)> {
    let mut result = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut buf = String::new();

    while i < len {
        if chars[i] == '$' {
            // Skip $$ (display math handled elsewhere)
            if i + 1 < len && chars[i + 1] == '$' {
                buf.push('$'); buf.push('$');
                i += 2;
                continue;
            }
            // Look for closing $
            let start = i + 1;
            let mut end = start;
            while end < len && chars[end] != '$' {
                end += 1;
            }
            if end < len && end > start {
                // Found $...$
                let math: String = chars[start..end].iter().collect();
                // Heuristic: skip if it looks like a dollar amount ($5, $100)
                let looks_like_money = math.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.');
                if looks_like_money {
                    buf.push('$');
                    buf.push_str(&math);
                    buf.push('$');
                    i = end + 1;
                    continue;
                }
                if !buf.is_empty() {
                    result.push((false, std::mem::take(&mut buf)));
                }
                result.push((true, math));
                i = end + 1;
            } else {
                buf.push('$');
                i += 1;
            }
        } else {
            buf.push(chars[i]);
            i += 1;
        }
    }
    if !buf.is_empty() {
        result.push((false, buf));
    }
    result
}
