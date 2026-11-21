//! Tailwind's compiled CSS, turned into the style table orbit reads on a phone.
//!
//! A phone has no CSS engine. What Tailwind emits for an app is a stylesheet
//! of utility rules whose values go through custom properties (`--spacing`,
//! `--primary`, `--tw-shadow`…), `calc()`, `oklch()` and `color-mix()`. This
//! resolves, at build time, everything that does not change once the app
//! runs — the theme's variables (light and dark), lengths in px, colours as
//! `#rrggbbaa` — and keeps only what classes compose with one another at run
//! time: the variables a rule sets (`--tw-*` and the like), each with its
//! `@property` initial value.
//!
//! Selectors become conditions orbit evaluates on its own tree: an attribute
//! of the element (`data-[state=open]:`, `aria-invalid:`, `disabled:`), the
//! dark scheme, an ancestor (`group-data-[…]:`), a previous sibling
//! (`peer-checked:`), the element's descendants (`[&_svg]:`, `*:`), a
//! minimum viewport width (`sm:`). What a touch screen never is (`:hover`
//! behind `@media (hover:hover)`) or what orbit cannot see (`::after`,
//! `:has()`) is kept as a condition that never holds, so the class stays
//! known and does nothing.
//!
//! The table (`version` 1):
//!
//! ```text
//! { "version": 1,
//!   "properties": { "--tw-shadow": { "initial": "0 0 #0000", "inherits": false } },
//!   "rules": [ { "class": "bg-primary", "when": [...], "target": ..., "media": ...,
//!                "specificity": 10, "declarations": [["background-color", "#171717ff"]] } ] }
//! ```
//!
//! A declaration's value that differs between the light and the dark scheme
//! is `{ "light": "…", "dark": "…" }`.

use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};

/// The root font size `rem` is taken against, as in a browser.
const ROOT_PX: f64 = 16.0;

// ------------------------------------------------------------------ parsing

#[derive(Debug)]
enum Node {
    At {
        name: String,
        prelude: String,
        body: Option<Vec<Node>>,
        /// The declarations of a declaration block (`@property`, `@font-face`).
        decls: Vec<(String, String)>,
    },
    Rule {
        selector: String,
        decls: Vec<(String, String)>,
        nested: Vec<Node>,
    },
}

struct Parser<'a> {
    s: &'a [u8],
    src: &'a str,
    i: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Parser {
            s: src.as_bytes(),
            src,
            i: 0,
        }
    }

    fn skip_ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"/*") {
                match self.src[self.i + 2..].find("*/") {
                    Some(end) => self.i += end + 4,
                    None => self.i = self.s.len(),
                }
            } else {
                return;
            }
        }
    }

    /// Read up to (not including) one of `stops` at nesting depth 0,
    /// stepping over strings, escapes, comments and brackets.
    fn read_until(&mut self, stops: &[u8]) -> &'a str {
        let start = self.i;
        let mut depth = 0i32;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            match c {
                b'\\' => {
                    self.i += 2;
                    continue;
                }
                b'"' | b'\'' => {
                    self.i += 1;
                    while self.i < self.s.len() && self.s[self.i] != c {
                        if self.s[self.i] == b'\\' {
                            self.i += 1;
                        }
                        self.i += 1;
                    }
                }
                b'/' if self.s.get(self.i + 1) == Some(&b'*') => {
                    match self.src[self.i + 2..].find("*/") {
                        Some(end) => self.i += end + 4,
                        None => self.i = self.s.len(),
                    }
                    continue;
                }
                b'(' | b'[' => depth += 1,
                b')' | b']' => depth -= 1,
                _ if depth <= 0 && stops.contains(&c) => break,
                _ => {}
            }
            self.i += 1;
        }
        self.src[start..self.i.min(self.s.len())].trim()
    }

    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    /// Items until `}` or the end.
    fn block(&mut self) -> Vec<Node> {
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return out,
                Some(b'}') => {
                    self.i += 1;
                    return out;
                }
                Some(b';') => self.i += 1,
                Some(b'@') => out.push(self.at_rule(false)),
                Some(_) => {
                    let selector = self.read_until(b"{};").to_string();
                    if self.peek() == Some(b'{') {
                        self.i += 1;
                        let (decls, nested) = self.rule_body();
                        out.push(Node::Rule {
                            selector,
                            decls,
                            nested,
                        });
                    } else {
                        self.i += 1;
                    }
                }
            }
        }
    }

    /// `nested`: inside a style rule, where a conditional rule holds the
    /// rule's own declarations (`.a { @supports (…) { color: … } }`).
    fn at_rule(&mut self, nested: bool) -> Node {
        self.i += 1;
        let start = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'-')
        {
            self.i += 1;
        }
        let name = self.src[start..self.i].to_ascii_lowercase();
        let prelude = self.read_until(b"{;").to_string();
        if self.peek() != Some(b'{') {
            self.i += 1;
            return Node::At {
                name,
                prelude,
                body: None,
                decls: Vec::new(),
            };
        }
        self.i += 1;
        match name.as_str() {
            "media" | "supports" | "layer" | "container" | "scope" | "starting-style" if nested => {
                let (decls, inner) = self.rule_body();
                let body = vec![Node::Rule {
                    selector: "&".into(),
                    decls,
                    nested: inner,
                }];
                Node::At {
                    name,
                    prelude,
                    body: Some(body),
                    decls: Vec::new(),
                }
            }
            "media" | "supports" | "layer" | "container" | "scope" | "starting-style" => {
                let body = self.block();
                Node::At {
                    name,
                    prelude,
                    body: Some(body),
                    decls: Vec::new(),
                }
            }
            "property" | "font-face" | "page" => {
                let (decls, _) = self.rule_body();
                Node::At {
                    name,
                    prelude,
                    body: None,
                    decls,
                }
            }
            _ => {
                // @keyframes and anything else: skipped whole.
                let mut depth = 1;
                while self.i < self.s.len() && depth > 0 {
                    match self.s[self.i] {
                        b'{' => depth += 1,
                        b'}' => depth -= 1,
                        _ => {}
                    }
                    self.i += 1;
                }
                Node::At {
                    name,
                    prelude,
                    body: None,
                    decls: Vec::new(),
                }
            }
        }
    }

    /// Declarations, and the rules nested among them, until `}`.
    fn rule_body(&mut self) -> (Vec<(String, String)>, Vec<Node>) {
        let mut decls = Vec::new();
        let mut nested = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b'}') => {
                    self.i += 1;
                    break;
                }
                Some(b';') => self.i += 1,
                Some(b'@') => nested.push(self.at_rule(true)),
                Some(_) => {
                    let text = self.read_until(b";{}");
                    if self.peek() == Some(b'{') {
                        self.i += 1;
                        let (d, n) = self.rule_body();
                        nested.push(Node::Rule {
                            selector: text.to_string(),
                            decls: d,
                            nested: n,
                        });
                    } else if let Some((prop, value)) = text.split_once(':') {
                        let value = value.trim();
                        let value = value.strip_suffix("!important").unwrap_or(value).trim();
                        decls.push((prop.trim().to_string(), value.to_string()));
                    }
                }
            }
        }
        (decls, nested)
    }
}

// ------------------------------------------------------------------ flattening

#[derive(Clone, Debug, PartialEq)]
enum Media {
    MinWidth(f64),
    MaxWidth(f64),
    Dark,
    Never,
}

struct Flat {
    selector: String,
    decls: Vec<(String, String)>,
    media: Vec<Media>,
}

fn media_of(prelude: &str) -> Vec<Media> {
    let p = prelude.replace(' ', "").to_ascii_lowercase();
    let length = |v: &str| -> Option<f64> {
        if let Some(n) = v.strip_suffix("rem") {
            n.parse::<f64>().ok().map(|n| n * ROOT_PX)
        } else if let Some(n) = v.strip_suffix("px") {
            n.parse().ok()
        } else if let Some(n) = v.strip_suffix("em") {
            n.parse::<f64>().ok().map(|n| n * ROOT_PX)
        } else {
            None
        }
    };
    let mut out = Vec::new();
    for part in p.split("and") {
        let part = part.trim_matches(|c| c == '(' || c == ')');
        if let Some(v) = part
            .strip_prefix("width>=")
            .or_else(|| part.strip_prefix("min-width:"))
        {
            match length(v) {
                Some(px) => out.push(Media::MinWidth(px)),
                None => out.push(Media::Never),
            }
        } else if let Some(v) = part
            .strip_prefix("width<")
            .or_else(|| part.strip_prefix("max-width:"))
        {
            match length(v) {
                // `width < x` excludes x itself.
                Some(px) => out.push(Media::MaxWidth(px - 0.01)),
                None => out.push(Media::Never),
            }
        } else if part == "prefers-color-scheme:dark" {
            out.push(Media::Dark);
        } else if part.is_empty() || part == "screen" || part == "all" {
        } else {
            // hover:hover, pointer:fine, prefers-reduced-motion, print…
            out.push(Media::Never);
        }
    }
    out
}

fn flatten(
    nodes: Vec<Node>,
    media: &[Media],
    parent: Option<&str>,
    out: &mut Vec<Flat>,
    props: &mut Vec<(String, Vec<(String, String)>)>,
) {
    for node in nodes {
        match node {
            Node::At {
                name,
                prelude,
                body,
                decls,
            } => match name.as_str() {
                "layer" if prelude.trim() == "properties" => {}
                "layer" | "supports" | "scope" => {
                    if let Some(body) = body {
                        flatten(body, media, parent, out, props);
                    }
                }
                "media" => {
                    if let Some(body) = body {
                        let mut m = media.to_vec();
                        m.extend(media_of(&prelude));
                        flatten(body, &m, parent, out, props);
                    }
                }
                "property" => props.push((prelude.trim().to_string(), decls)),
                _ => {}
            },
            Node::Rule {
                selector,
                decls,
                nested,
            } => {
                let selector = match parent {
                    Some(p) if selector.contains('&') => selector.replace('&', p),
                    Some(p) => format!("{p} {selector}"),
                    None => selector,
                };
                if !decls.is_empty() {
                    out.push(Flat {
                        selector: selector.clone(),
                        decls,
                        media: media.to_vec(),
                    });
                }
                if !nested.is_empty() {
                    flatten(nested, media, Some(&selector), out, props);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ values

/// The theme's variables for one scheme, and the names left to run time.
struct Vars<'a> {
    light: &'a HashMap<String, String>,
    dark: &'a HashMap<String, String>,
    dynamic: &'a HashSet<String>,
    is_dark: bool,
}

impl Vars<'_> {
    fn get(&self, name: &str) -> Option<&String> {
        if self.is_dark {
            self.dark.get(name).or_else(|| self.light.get(name))
        } else {
            self.light.get(name)
        }
    }
}

/// Find the `)` matching the `(` just before `from`.
fn closing(s: &str, from: usize) -> Option<usize> {
    let b = s.as_bytes();
    let mut depth = 1;
    let mut i = from;
    while i < b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\\' => i += 1,
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split at top-level commas.
fn split_commas(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b'\\' => i += 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// Substitute the theme's variables; keep `var()` of the others — those a
/// rule sets, and those set nowhere here (an inline style may set them), which
/// orbit resolves at run time against its fallback.
fn substitute(value: &str, vars: &Vars, depth: u32) -> String {
    if depth > 32 {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(at) = rest.find("var(") {
        // `var(` inside a longer identifier is not a call.
        let ident_before = rest[..at]
            .chars()
            .last()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-');
        out.push_str(&rest[..at]);
        let Some(end) = closing(rest, at + 4) else {
            out.push_str(&rest[at..]);
            return out;
        };
        if ident_before {
            out.push_str(&rest[at..=end]);
            rest = &rest[end + 1..];
            continue;
        }
        let inner = &rest[at + 4..end];
        let (name, fallback) = match inner.find(',') {
            Some(c) => (inner[..c].trim(), Some(&inner[c + 1..])),
            None => (inner.trim(), None),
        };
        let theme = vars.get(name);
        if vars.dynamic.contains(name) || theme.is_none() {
            match fallback {
                Some(f) => {
                    out.push_str("var(");
                    out.push_str(name);
                    out.push(',');
                    out.push_str(&substitute(f, vars, depth + 1));
                    out.push(')');
                }
                None => {
                    out.push_str("var(");
                    out.push_str(name);
                    out.push(')');
                }
            }
        } else if let Some(v) = theme {
            out.push_str(&substitute(v, vars, depth + 1));
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A number with its unit, as found in a value.
fn number_unit(s: &str) -> Option<(f64, &str)> {
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|&(i, c)| {
            !(c.is_ascii_digit()
                || c == '.'
                || ((c == '-' || c == '+') && i == 0)
                || c == 'e'
                    && i > 0
                    && s[..i].chars().last().is_some_and(|p| p.is_ascii_digit())
                    && s[i + 1..].starts_with(|n: char| n.is_ascii_digit() || n == '-'))
        })
        .map_or(s.len(), |(i, _)| i);
    if end == 0 {
        return None;
    }
    let n: f64 = s[..end].parse().ok()?;
    Some((n, &s[end..]))
}

fn fmt_num(n: f64) -> String {
    let r = (n * 10_000.0).round() / 10_000.0;
    if r == r.trunc() {
        format!("{}", r as i64)
    } else {
        let s = format!("{r:.4}");
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    }
}

/// Values in a calc() expression: a number and its unit ("" or "px" or "%").
#[derive(Clone, Copy, Debug)]
struct Q {
    n: f64,
    unit: &'static str,
}

/// Evaluate `calc()` arithmetic over px, % and plain numbers; None when it
/// mixes what cannot be added (px and %) or holds anything else.
fn calc(expr: &str) -> Option<Q> {
    let tokens = calc_tokens(expr)?;
    let mut pos = 0;
    let q = calc_sum(&tokens, &mut pos)?;
    (pos == tokens.len()).then_some(q)
}

#[derive(Clone, Debug)]
enum Tok {
    Num(Q),
    Op(u8),
    Open,
    Close,
}

fn calc_tokens(expr: &str) -> Option<Vec<Tok>> {
    let b = expr.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if c == b'(' {
            out.push(Tok::Open);
            i += 1;
        } else if c == b')' {
            out.push(Tok::Close);
            i += 1;
        } else if c == b'*'
            || c == b'/'
            // `+` / `-` between operands (with the space CSS requires) is an
            // operator; before a number, a sign.
            || (c == b'+' || c == b'-')
                && !matches!(out.last(), None | Some(Tok::Op(_)) | Some(Tok::Open))
                && b.get(i + 1).is_some_and(|n| n.is_ascii_whitespace())
        {
            out.push(Tok::Op(c));
            i += 1;
        } else if expr[i..].starts_with("infinity") {
            // `calc(infinity * 1px)` (rounded-full): the largest length a box
            // could want, clamped as browsers clamp it.
            out.push(Tok::Num(Q { n: 1e6, unit: "" }));
            i += "infinity".len();
        } else if c.is_ascii_digit() || c == b'.' || c == b'-' || c == b'+' {
            let start = i;
            i += 1;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'%') {
                i += 1;
            }
            let (n, unit) = number_unit(&expr[start..i])?;
            let q = match unit {
                "" => Q { n, unit: "" },
                "px" => Q { n, unit: "px" },
                "rem" => Q {
                    n: n * ROOT_PX,
                    unit: "px",
                },
                "%" => Q { n, unit: "%" },
                _ => return None,
            };
            out.push(Tok::Num(q));
        } else {
            return None;
        }
    }
    Some(out)
}

fn calc_sum(t: &[Tok], pos: &mut usize) -> Option<Q> {
    let mut acc = calc_product(t, pos)?;
    while let Some(Tok::Op(op @ (b'+' | b'-'))) = t.get(*pos) {
        *pos += 1;
        let rhs = calc_product(t, pos)?;
        let unit = match (acc.unit, rhs.unit) {
            (a, b) if a == b => a,
            (a, "") if acc.n == 0.0 || rhs.n == 0.0 => a,
            ("", b) if acc.n == 0.0 => b,
            _ => return None,
        };
        acc = Q {
            n: if *op == b'+' {
                acc.n + rhs.n
            } else {
                acc.n - rhs.n
            },
            unit,
        };
    }
    Some(acc)
}

fn calc_product(t: &[Tok], pos: &mut usize) -> Option<Q> {
    let mut acc = calc_atom(t, pos)?;
    while let Some(Tok::Op(op @ (b'*' | b'/'))) = t.get(*pos) {
        *pos += 1;
        let rhs = calc_atom(t, pos)?;
        acc = if *op == b'*' {
            match (acc.unit, rhs.unit) {
                ("", u) | (u, "") => Q {
                    n: acc.n * rhs.n,
                    unit: u,
                },
                _ => return None,
            }
        } else {
            match rhs.unit {
                "" if rhs.n != 0.0 => Q {
                    n: acc.n / rhs.n,
                    unit: acc.unit,
                },
                _ => return None,
            }
        };
    }
    Some(acc)
}

fn calc_atom(t: &[Tok], pos: &mut usize) -> Option<Q> {
    match t.get(*pos)? {
        Tok::Num(q) => {
            *pos += 1;
            Some(*q)
        }
        Tok::Open => {
            *pos += 1;
            let q = calc_sum(t, pos)?;
            matches!(t.get(*pos), Some(Tok::Close)).then(|| {
                *pos += 1;
                q
            })
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ colours

/// A colour: linear-light-free sRGB components 0..1 and alpha.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Rgba(f64, f64, f64, f64);

fn to_hex(c: Rgba) -> String {
    let b = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}{:02x}", b(c.0), b(c.1), b(c.2), b(c.3))
}

fn srgb_to_linear(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn oklab_to_rgb(l: f64, a: f64, b: f64) -> (f64, f64, f64) {
    let l_ = l + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_ = l - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_ = l - 0.089_484_177_5 * a - 1.291_485_548 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    let r = 4.076_741_662_1 * l3 - 3.307_711_591_3 * m3 + 0.230_969_929_2 * s3;
    let g = -1.268_438_004_6 * l3 + 2.609_757_401_1 * m3 - 0.341_319_396_5 * s3;
    let bl = -0.004_196_086_3 * l3 - 0.703_418_614_7 * m3 + 1.707_614_701 * s3;
    (linear_to_srgb(r), linear_to_srgb(g), linear_to_srgb(bl))
}

fn rgb_to_oklab(r: f64, g: f64, b: f64) -> (f64, f64, f64) {
    let (r, g, b) = (srgb_to_linear(r), srgb_to_linear(g), srgb_to_linear(b));
    let l = 0.412_221_470_8 * r + 0.536_332_536_3 * g + 0.051_445_992_9 * b;
    let m = 0.211_903_498_2 * r + 0.680_699_545_1 * g + 0.107_396_956_6 * b;
    let s = 0.088_302_461_9 * r + 0.281_718_837_6 * g + 0.629_978_700_5 * b;
    let (l_, m_, s_) = (l.cbrt(), m.cbrt(), s.cbrt());
    (
        0.210_454_255_3 * l_ + 0.793_617_785 * m_ - 0.004_072_046_8 * s_,
        1.977_998_495_1 * l_ - 2.428_592_205 * m_ + 0.450_593_709_9 * s_,
        0.025_904_037_1 * l_ + 0.782_771_766_2 * m_ - 0.808_675_766 * s_,
    )
}

/// `n`, `n%` (of `full`) or `none`.
fn component(text: &str, full: f64) -> Option<f64> {
    let t = text.trim();
    if t == "none" {
        return Some(0.0);
    }
    match t.strip_suffix('%') {
        Some(p) => p.parse::<f64>().ok().map(|p| p / 100.0 * full),
        None => t.parse().ok(),
    }
}

/// The components of a colour function and its alpha (`a b c / alpha`, or commas).
fn components(args: &str) -> Option<(Vec<String>, f64)> {
    let (main, alpha) = match args.split_once('/') {
        Some((m, a)) => (m, component(a, 1.0)?),
        None => (args, 1.0),
    };
    let parts: Vec<String> = main
        .split(|c: char| c == ',' || c.is_ascii_whitespace())
        .filter(|p| !p.is_empty())
        .map(str::to_string)
        .collect();
    if parts.len() == 4 && !args.contains('/') {
        let a = component(&parts[3], 1.0)?;
        return Some((parts[..3].to_vec(), a));
    }
    (parts.len() == 3).then_some((parts, alpha))
}

fn parse_color(text: &str) -> Option<Rgba> {
    let t = text.trim().to_ascii_lowercase();
    if let Some(hex) = t.strip_prefix('#') {
        let v: Vec<u8> = hex
            .chars()
            .map(|c| c.to_digit(16).map(|d| d as u8))
            .collect::<Option<_>>()?;
        let f = |x: u8| f64::from(x) / 255.0;
        return match v.len() {
            3 | 4 => {
                let c = |i: usize| f(v[i] * 17);
                Some(Rgba(
                    c(0),
                    c(1),
                    c(2),
                    if v.len() == 4 { c(3) } else { 1.0 },
                ))
            }
            6 | 8 => {
                let c = |i: usize| f(v[i] * 16 + v[i + 1]);
                Some(Rgba(
                    c(0),
                    c(2),
                    c(4),
                    if v.len() == 8 { c(6) } else { 1.0 },
                ))
            }
            _ => None,
        };
    }
    match t.as_str() {
        "transparent" => return Some(Rgba(0.0, 0.0, 0.0, 0.0)),
        "black" => return Some(Rgba(0.0, 0.0, 0.0, 1.0)),
        "white" => return Some(Rgba(1.0, 1.0, 1.0, 1.0)),
        _ => {}
    }
    let open = t.find('(')?;
    let name = &t[..open];
    let args = &t[open + 1..t.rfind(')')?];
    match name {
        "rgb" | "rgba" => {
            let (p, a) = components(args)?;
            let c = |i: usize| component(&p[i], 255.0).map(|v| v / 255.0);
            Some(Rgba(c(0)?, c(1)?, c(2)?, a))
        }
        "oklch" => {
            let (p, a) = components(args)?;
            let l = component(&p[0], 1.0)?;
            let c = component(&p[1], 0.4)?;
            let h = p[2]
                .trim_end_matches("deg")
                .parse::<f64>()
                .ok()?
                .to_radians();
            let (r, g, b) = oklab_to_rgb(l, c * h.cos(), c * h.sin());
            Some(Rgba(r, g, b, a))
        }
        "oklab" => {
            let (p, a) = components(args)?;
            let (r, g, b) = oklab_to_rgb(
                component(&p[0], 1.0)?,
                component(&p[1], 0.4)?,
                component(&p[2], 0.4)?,
            );
            Some(Rgba(r, g, b, a))
        }
        "color-mix" => color_mix(args),
        _ => None,
    }
}

/// `color-mix(in oklab|srgb, a p?, b q?)`, interpolated premultiplied (CSS Color 5).
fn color_mix(args: &str) -> Option<Rgba> {
    let parts = split_commas(args);
    if parts.len() != 3 {
        return None;
    }
    let space = parts[0].trim().strip_prefix("in ")?.trim();
    let side = |p: &str| -> Option<(Rgba, Option<f64>)> {
        let p = p.trim();
        // The percentage may come before or after the colour.
        let (color, pct) = match p.rsplit_once(' ') {
            Some((c, pc)) if pc.ends_with('%') && !c.ends_with('(') => {
                (c, Some(component(pc, 1.0)?))
            }
            _ => match p.split_once(' ') {
                Some((pc, c)) if pc.ends_with('%') => (c, Some(component(pc, 1.0)?)),
                _ => (p, None),
            },
        };
        Some((parse_color(color)?, pct))
    };
    let (c1, p1) = side(parts[1])?;
    let (c2, p2) = side(parts[2])?;
    let (p1, p2) = match (p1, p2) {
        (Some(a), Some(b)) => (a, b),
        (Some(a), None) => (a, 1.0 - a),
        (None, Some(b)) => (1.0 - b, b),
        (None, None) => (0.5, 0.5),
    };
    let sum = p1 + p2;
    if sum <= 0.0 {
        return None;
    }
    let (w1, w2) = (p1 / sum, p2 / sum);
    let to_space = |c: Rgba| match space {
        "srgb" => (c.0, c.1, c.2),
        _ => rgb_to_oklab(c.0, c.1, c.2),
    };
    let (a1, a2) = (c1.3, c2.3);
    let alpha = a1 * w1 + a2 * w2;
    let (x1, x2) = (to_space(c1), to_space(c2));
    let mix = |u: f64, v: f64| {
        if alpha == 0.0 {
            0.0
        } else {
            (u * a1 * w1 + v * a2 * w2) / alpha
        }
    };
    let m = (mix(x1.0, x2.0), mix(x1.1, x2.1), mix(x1.2, x2.2));
    let (r, g, b) = match space {
        "srgb" => m,
        _ => oklab_to_rgb(m.0, m.1, m.2),
    };
    // Weights summing under 100% scale the alpha down.
    Some(Rgba(r, g, b, alpha * sum.min(1.0)))
}

/// A value with its constant functions evaluated: `calc()` to px, colours to
/// `#rrggbbaa`, `rem` to px. What still holds a run-time `var()` stays text.
fn evaluate(value: &str) -> String {
    let mut out = String::new();
    let b = value.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c.is_ascii_alphabetic() || c == b'-' && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic)
        {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'-') {
                i += 1;
            }
            let name = &value[start..i];
            if b.get(i) == Some(&b'(') {
                let Some(end) = closing(value, i + 1) else {
                    out.push_str(&value[start..]);
                    return out;
                };
                let inner = evaluate(&value[i + 1..end]);
                i = end + 1;
                let lower = name.to_ascii_lowercase();
                if inner.contains("var(") {
                    out.push_str(name);
                    out.push('(');
                    out.push_str(&inner);
                    out.push(')');
                    continue;
                }
                match lower.as_str() {
                    "calc" => match calc(&inner) {
                        Some(q) => {
                            out.push_str(&fmt_num(q.n));
                            out.push_str(q.unit);
                        }
                        None => {
                            out.push_str("calc(");
                            out.push_str(&inner);
                            out.push(')');
                        }
                    },
                    "oklch" | "oklab" | "rgb" | "rgba" | "color-mix" => {
                        let text = format!("{lower}({inner})");
                        match parse_color(&text) {
                            Some(color) => out.push_str(&to_hex(color)),
                            None => out.push_str(&text),
                        }
                    }
                    _ => {
                        out.push_str(name);
                        out.push('(');
                        out.push_str(&inner);
                        out.push(')');
                    }
                }
            } else {
                match name.to_ascii_lowercase().as_str() {
                    "transparent" => out.push_str("#00000000"),
                    "black" => out.push_str("#000000ff"),
                    "white" => out.push_str("#ffffffff"),
                    _ => out.push_str(name),
                }
            }
        } else if c == b'#' {
            let start = i;
            i += 1;
            while i < b.len() && b[i].is_ascii_hexdigit() {
                i += 1;
            }
            match parse_color(&value[start..i]) {
                Some(color) => out.push_str(&to_hex(color)),
                None => out.push_str(&value[start..i]),
            }
        } else if c.is_ascii_digit() || c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit) {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'.' || b[i] == b'%') {
                i += 1;
            }
            let token = &value[start..i];
            match number_unit(token) {
                Some((n, "rem")) => {
                    out.push_str(&fmt_num(n * ROOT_PX));
                    out.push_str("px");
                }
                Some((n, unit)) => {
                    out.push_str(&fmt_num(n));
                    out.push_str(unit);
                }
                None => out.push_str(token),
            }
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    out
}

// ------------------------------------------------------------------ selectors

fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Read an identifier (with escapes) from the start of `s`; returns it and its byte length.
fn ident(s: &str) -> (String, usize) {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            i += 2;
        } else if b[i].is_ascii_alphanumeric() || b[i] == b'-' || b[i] == b'_' || b[i] >= 0x80 {
            i += 1;
        } else {
            break;
        }
    }
    let i = i.min(b.len());
    (unescape(&s[..i]), i)
}

#[derive(Default)]
struct Compound {
    classes: Vec<String>,
    tag: Option<String>,
    conds: Vec<Value>,
    pseudo_element: Option<String>,
    specificity: u32,
}

/// One compound selector (`.a[data-x=y]:hover`), its conditions as JSON.
fn compound(s: &str) -> Option<Compound> {
    let mut c = Compound::default();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'.' => {
                let (name, len) = ident(&s[i + 1..]);
                c.classes.push(name);
                c.specificity += 10;
                i += 1 + len;
            }
            b'*' => i += 1,
            b'[' => {
                let end = s[i..].find(']')? + i;
                let body = &s[i + 1..end];
                // `[a]`, `[a=v]`, and the substring forms `[a*=v]`, `[a^=v]`, `[a$=v]`, `[a~=v]`.
                let cond = match body.split_once('=') {
                    Some((n, v)) => {
                        let v = v
                            .trim()
                            .trim_end_matches(" i")
                            .trim_matches('"')
                            .trim_matches('\'')
                            .to_string();
                        match n.trim().strip_suffix(['*', '^', '$', '~', '|']) {
                            Some(name) => {
                                let op = &n.trim()[name.len()..];
                                json!({ "attribute": unescape(name.trim()), "operator": op, "value": v })
                            }
                            None => json!({ "attribute": unescape(n.trim()), "equals": v }),
                        }
                    }
                    None => json!({ "attribute": unescape(body.trim()), "equals": null }),
                };
                c.conds.push(cond);
                c.specificity += 10;
                i = end + 1;
            }
            b':' if b.get(i + 1) == Some(&b':') => {
                let (name, len) = ident(&s[i + 2..]);
                c.pseudo_element = Some(name);
                c.specificity += 1;
                i += 2 + len;
            }
            b':' => {
                let (name, len) = ident(&s[i + 1..]);
                i += 1 + len;
                let args = if b.get(i) == Some(&b'(') {
                    let end = closing(s, i + 1)?;
                    let a = &s[i + 1..end];
                    i = end + 1;
                    Some(a)
                } else {
                    None
                };
                // CSS2's one-colon pseudo-elements.
                if matches!(
                    name.as_str(),
                    "after" | "before" | "first-line" | "first-letter"
                ) && args.is_none()
                {
                    c.pseudo_element = Some(name);
                    c.specificity += 1;
                    continue;
                }
                // `:where(.group)`, `:is(.peer)`: one plain compound joins this one.
                if let (Some(a), "is" | "where") = (args, name.as_str()) {
                    if split_commas(a).len() == 1 && combinators(a).is_some_and(|p| p.len() == 1) {
                        if let Some(inner) = compound(a.trim()) {
                            c.classes.extend(inner.classes);
                            c.conds.extend(inner.conds);
                            if inner.tag.is_some() {
                                c.tag = inner.tag;
                            }
                            if name == "is" {
                                c.specificity += inner.specificity;
                            }
                            continue;
                        }
                    }
                }
                let (cond, spec) = pseudo_class(&name, args)?;
                c.conds.push(cond);
                c.specificity += spec;
            }
            _ if b[i].is_ascii_alphabetic() => {
                let (name, len) = ident(&s[i..]);
                c.tag = Some(name.to_ascii_lowercase());
                c.specificity += 1;
                i += len;
            }
            _ => return None,
        }
    }
    Some(c)
}

fn pseudo_class(name: &str, args: Option<&str>) -> Option<(Value, u32)> {
    let state = |s: &str| (json!({ "state": s }), 10);
    Some(match (name, args) {
        (
            "hover" | "focus" | "focus-visible" | "focus-within" | "active" | "visited" | "target"
            | "autofill" | "placeholder-shown" | "invalid" | "valid" | "required" | "optional"
            | "read-only" | "open" | "popover-open" | "user-invalid" | "user-valid" | "default"
            | "in-range" | "out-of-range",
            None,
        ) => state(name),
        ("disabled", None) => (json!({ "attribute": "disabled", "equals": null }), 10),
        ("enabled", None) => (
            json!({ "not": [{ "attribute": "disabled", "equals": null }] }),
            10,
        ),
        ("checked", None) => (json!({ "attribute": "checked", "equals": null }), 10),
        ("indeterminate", None) => (json!({ "attribute": "indeterminate", "equals": null }), 10),
        (
            "first-child" | "last-child" | "only-child" | "first-of-type" | "last-of-type"
            | "only-of-type" | "empty",
            None,
        ) => (json!({ "position": name }), 10),
        ("nth-child" | "nth-last-child" | "nth-of-type" | "nth-last-of-type", Some(a)) => {
            (json!({ "position": name, "of": a.trim() }), 10)
        }
        ("dir", Some(a)) => (json!({ "dir": a.trim() }), 10),
        ("root", None) => state("root"),
        ("is" | "where" | "not" | "has", Some(a)) => {
            let spec_of = |sel: &str| -> u32 { complex(sel).map_or(0, |x| x.1) };
            let alternatives: Vec<&str> = split_commas(a);
            let max_spec = alternatives.iter().map(|s| spec_of(s)).max().unwrap_or(0);
            let spec = if name == "where" { 0 } else { max_spec };
            if name == "has" {
                return Some((json!({ "has": a.trim() }), spec));
            }
            let mut any = Vec::new();
            for alt in alternatives {
                any.push(relation(alt.trim())?);
            }
            let cond = if any.len() == 1 {
                any.remove(0)
            } else {
                json!({ "any": any })
            };
            if name == "not" {
                (json!({ "not": [cond] }), spec)
            } else {
                (cond, spec)
            }
        }
        _ => return None,
    })
}

/// What a selector inside `:is()` / `:where()` / `:not()` says about the
/// element it applies to: its own attributes and states, or a relation to an
/// ancestor (`X *`) or a previous sibling (`X ~ *`).
fn relation(sel: &str) -> Option<Value> {
    let parts = combinators(sel)?;
    match parts.as_slice() {
        [(None, own)] => {
            let c = compound(own)?;
            if c.classes.iter().any(|n| n == "dark") && c.conds.is_empty() && c.tag.is_none() {
                return Some(json!({ "scheme": "dark" }));
            }
            if !c.classes.is_empty() || c.tag.is_some() || c.pseudo_element.is_some() {
                // A class or tag of the element itself: orbit matches only
                // utilities by class, so this never holds.
                return Some(json!({ "state": "unsupported" }));
            }
            Some(if c.conds.len() == 1 {
                c.conds.into_iter().next()?
            } else {
                json!({ "all": c.conds })
            })
        }
        [(None, other), (Some(comb), last)] if *last == "*" => {
            let c = compound(other)?;
            let other = json!({ "classes": c.classes, "when": c.conds });
            if c.classes == ["dark"] && c.conds.is_empty() && *comb == ' ' {
                return Some(json!({ "scheme": "dark" }));
            }
            match comb {
                ' ' => Some(json!({ "ancestor": other })),
                '>' => Some(json!({ "parent": other })),
                '~' => Some(json!({ "previous": other })),
                '+' => Some(json!({ "previousAdjacent": other })),
                _ => None,
            }
        }
        _ => Some(json!({ "state": "unsupported" })),
    }
}

/// A selector split at its top-level combinators: (combinator before, compound).
fn combinators(sel: &str) -> Option<Vec<(Option<char>, &str)>> {
    let b = sel.as_bytes();
    let mut out: Vec<(Option<char>, &str)> = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    let mut pending: Option<char> = None;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\\' => {
                i += 2;
                continue;
            }
            b'(' | b'[' => depth += 1,
            b')' | b']' => depth -= 1,
            b' ' | b'>' | b'~' | b'+' if depth == 0 => {
                let part = sel[start..i].trim();
                if !part.is_empty() {
                    out.push((pending.take(), part));
                }
                let mut comb = if c == b' ' { ' ' } else { c as char };
                let mut j = i + 1;
                while j < b.len() && matches!(b[j], b' ' | b'>' | b'~' | b'+') {
                    if b[j] != b' ' {
                        comb = b[j] as char;
                    }
                    j += 1;
                }
                pending = Some(comb);
                i = j;
                start = j;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    let part = sel[start..].trim();
    if !part.is_empty() {
        out.push((pending.take(), part));
    }
    if out.is_empty() {
        return None;
    }
    // The first compound has no combinator before it.
    out[0].0 = None;
    Some(out)
}

/// A selector's utility class, its conditions, target and specificity.
struct Parsed {
    class: String,
    when: Vec<Value>,
    target: Value,
    specificity: u32,
}

/// A selector's compounds, each with the combinator before it.
type Compounds = Vec<(Option<char>, Compound)>;

fn complex(sel: &str) -> Option<(Compounds, u32)> {
    let parts = combinators(sel)?;
    let mut spec = 0;
    let mut out = Vec::new();
    for (comb, text) in parts {
        let c = compound(text)?;
        spec += c.specificity;
        out.push((comb, c));
    }
    Some((out, spec))
}

fn parse_selector(sel: &str) -> Option<Parsed> {
    let sel = sel.trim();
    // `:where(.space-y-2>:not(:last-child))`, `:is(.\*\:x>*)`: the utility is inside.
    for wrap in [":where(", ":is("] {
        if let Some(inner) = sel.strip_prefix(wrap) {
            if closing(sel, wrap.len()) == Some(sel.len() - 1) {
                let mut p = parse_selector(&inner[..inner.len() - 1])?;
                if wrap == ":where(" {
                    p.specificity = 0;
                }
                return Some(p);
            }
        }
    }
    let (parts, specificity) = complex(sel)?;
    let mut parts = parts.into_iter();
    let (_, first) = parts.next()?;
    let class = first.classes.first()?.clone();
    if first.tag.is_some() {
        return None;
    }
    let mut when = first.conds;
    // `[.border-b]:pb-6`: the element also carries another class.
    for other in &first.classes[1..] {
        when.push(json!({ "class": other }));
    }
    let rest: Vec<(Option<char>, Compound)> = parts.collect();
    let target = match (rest.as_slice(), first.pseudo_element) {
        ([], None) => json!("self"),
        ([], Some(pe)) => json!({ "pseudo": pe }),
        ([(Some(comb), t)], None) if t.classes.is_empty() => json!({
            "relation": if *comb == '>' { "child" } else { "descendant" },
            "tag": t.tag,
            "when": t.conds,
            "pseudo": t.pseudo_element,
        }),
        _ => {
            when.push(json!({ "state": "unsupported" }));
            json!("self")
        }
    };
    Some(Parsed {
        class,
        when,
        target,
        specificity,
    })
}

// ------------------------------------------------------------------ the table

/// A rule without a class (`*`, `body`, `button`, `[hidden]`): the base
/// layer's defaults, such as nebula's `* { border-color: var(--border) }`.
/// Only one compound; anything relative to another element is left out.
fn element_rule(sel: &str, rule: &Flat, resolve: &dyn Fn(&str) -> Value) -> Option<Value> {
    let (parts, specificity) = complex(sel)?;
    let [(_, c)] = parts.as_slice() else {
        return None;
    };
    if !c.classes.is_empty() || !rule.media.is_empty() {
        return None;
    }
    if c.conds.iter().any(|cond| cond.get("state").is_some()) {
        return None;
    }
    let declarations: Vec<Value> = rule
        .decls
        .iter()
        .map(|(p, v)| json!([p, resolve(v)]))
        .collect();
    let mut entry = Map::new();
    entry.insert(
        "element".into(),
        json!(c.tag.clone().unwrap_or_else(|| "*".into())),
    );
    if !c.conds.is_empty() {
        entry.insert("when".into(), Value::Array(c.conds.clone()));
    }
    if let Some(pe) = &c.pseudo_element {
        entry.insert("target".into(), json!({ "pseudo": pe }));
    }
    entry.insert("specificity".into(), json!(specificity));
    entry.insert("declarations".into(), Value::Array(declarations));
    Some(Value::Object(entry))
}

/// Compile Tailwind's output into orbit's style table.
pub fn compile(css: &str) -> Value {
    let mut parser = Parser::new(css);
    let tree = parser.block();
    let mut flat = Vec::new();
    let mut at_props = Vec::new();
    flatten(tree, &[], None, &mut flat, &mut at_props);

    // The theme: what :root / :host set (light) and what .dark overrides.
    let mut light = HashMap::new();
    let mut dark = HashMap::new();
    let mut dynamic: HashSet<String> = HashSet::new();
    for rule in &flat {
        let selectors: Vec<String> = split_commas(&rule.selector)
            .iter()
            .map(|s| s.trim().to_string())
            .collect();
        let theme_light = selectors.iter().all(|s| s == ":root" || s == ":host");
        let theme_dark = selectors.iter().all(|s| s == ".dark");
        for (prop, value) in &rule.decls {
            if !prop.starts_with("--") {
                continue;
            }
            if theme_light && rule.media.is_empty() {
                light.insert(prop.clone(), value.clone());
            } else if theme_dark && rule.media.is_empty() {
                dark.insert(prop.clone(), value.clone());
            } else {
                dynamic.insert(prop.clone());
            }
        }
    }
    let mut properties = Map::new();
    for (name, decls) in &at_props {
        dynamic.insert(name.clone());
        let get = |k: &str| decls.iter().find(|(p, _)| p == k).map(|(_, v)| v.as_str());
        let initial = get("initial-value").map(|v| evaluate(v.trim_matches('"')));
        let inherits = get("inherits").map(str::trim) == Some("true");
        properties.insert(
            name.clone(),
            json!({ "initial": initial, "inherits": inherits }),
        );
    }

    let light_vars = Vars {
        light: &light,
        dark: &dark,
        dynamic: &dynamic,
        is_dark: false,
    };
    let dark_vars = Vars {
        light: &light,
        dark: &dark,
        dynamic: &dynamic,
        is_dark: true,
    };
    let resolve = |value: &str| -> Value {
        let l = evaluate(&substitute(value, &light_vars, 0));
        let d = evaluate(&substitute(value, &dark_vars, 0));
        if l == d {
            Value::String(l)
        } else {
            json!({ "light": l, "dark": d })
        }
    };

    let mut rules = Vec::new();
    let mut known: Vec<String> = Vec::new();
    let mut seen_known: HashSet<String> = HashSet::new();
    for rule in &flat {
        for sel in split_commas(&rule.selector) {
            let sel = sel.trim();
            if !sel.starts_with('.') && !sel.starts_with(":where(.") && !sel.starts_with(":is(.") {
                if let Some(entry) = element_rule(sel, rule, &resolve) {
                    rules.push(entry);
                }
                continue;
            }
            let Some(parsed) = parse_selector(sel) else {
                // A class we could not read: known, never applied.
                if let Some(c) = sel.strip_prefix('.') {
                    let (name, _) = ident(c);
                    if seen_known.insert(name.clone()) {
                        known.push(name);
                    }
                }
                continue;
            };
            let mut when = parsed.when;
            let mut media = Map::new();
            for m in &rule.media {
                match m {
                    Media::MinWidth(px) => {
                        media.insert("minWidth".into(), json!(px));
                    }
                    Media::MaxWidth(px) => {
                        media.insert("maxWidth".into(), json!(px));
                    }
                    Media::Dark => when.push(json!({ "scheme": "dark" })),
                    Media::Never => when.push(json!({ "state": "media" })),
                }
            }
            let declarations: Vec<Value> = rule
                .decls
                .iter()
                .map(|(p, v)| json!([p, resolve(v)]))
                .collect();
            let mut entry = Map::new();
            entry.insert("class".into(), json!(parsed.class));
            if !when.is_empty() {
                entry.insert("when".into(), Value::Array(when));
            }
            if parsed.target != json!("self") {
                entry.insert("target".into(), parsed.target);
            }
            if !media.is_empty() {
                entry.insert("media".into(), Value::Object(media));
            }
            entry.insert("specificity".into(), json!(parsed.specificity));
            entry.insert("declarations".into(), Value::Array(declarations));
            rules.push(Value::Object(entry));
        }
    }
    json!({ "version": 1, "properties": properties, "rules": rules, "inert": known })
}

/// `ream css:compile <input> [-o out]`.
pub fn run_compile(input: &str, out: Option<&str>) -> Result<(), String> {
    let css = std::fs::read_to_string(input).map_err(|e| format!("{input}: {e}"))?;
    let table = compile(&css).to_string();
    match out {
        Some(path) => std::fs::write(path, table).map_err(|e| format!("{path}: {e}")),
        None => {
            println!("{table}");
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules_of(table: &Value, class: &str) -> Vec<Value> {
        table["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["class"] == class)
            .cloned()
            .collect()
    }

    const THEME: &str = r#"@layer theme{:root,:host{--spacing:.25rem;--text-sm:.875rem;--text-sm--line-height:calc(1.25 / .875);--radius-md:.375rem}}
:root{--radius:0.625rem;--primary:oklch(0.205 0 0);--ring:oklch(0.708 0 0);--destructive:oklch(0.577 0.245 27.325)}
.dark{--primary:oklch(0.922 0 0)}
@property --tw-shadow{syntax:"*";inherits:false;initial-value:0 0 #0000}
@property --tw-leading{syntax:"*";inherits:false}
"#;

    #[test]
    fn resolves_the_theme_lengths_and_colours() {
        let t = compile(&format!(
            "{THEME}@layer utilities{{.h-9{{height:calc(var(--spacing) * 9)}}.rounded-md{{border-radius:calc(var(--radius) - 2px)}}.text-sm{{font-size:var(--text-sm);line-height:var(--tw-leading,var(--text-sm--line-height))}}.bg-primary{{background-color:var(--primary)}}}}"
        ));
        assert_eq!(
            rules_of(&t, "h-9")[0]["declarations"],
            json!([["height", "36px"]])
        );
        assert_eq!(
            rules_of(&t, "rounded-md")[0]["declarations"],
            json!([["border-radius", "8px"]])
        );
        assert_eq!(
            rules_of(&t, "text-sm")[0]["declarations"],
            json!([
                ["font-size", "14px"],
                ["line-height", "var(--tw-leading,1.4286)"]
            ])
        );
        // oklch(0.205 0 0) is #171717; dark mode swaps it.
        assert_eq!(
            rules_of(&t, "bg-primary")[0]["declarations"],
            json!([["background-color", { "light": "#171717ff", "dark": "#e5e5e5ff" }]])
        );
        assert_eq!(
            t["properties"]["--tw-shadow"],
            json!({ "initial": "0 0 #00000000", "inherits": false })
        );
    }

    #[test]
    fn mixes_colours_with_transparent_and_keeps_run_time_variables() {
        let t = compile(&format!(
            "{THEME}.ring-ring\\/50{{--tw-ring-color:color-mix(in oklab, var(--ring) 50%, transparent)}}.shadow-xs{{--tw-shadow:0 1px 2px 0 var(--tw-shadow-color,#0000000d);box-shadow:var(--tw-ring-shadow), var(--tw-shadow)}}"
        ));
        // oklch(0.708 0 0) is #a1a1a1; half of it over transparent keeps the colour.
        assert_eq!(
            rules_of(&t, "ring-ring/50")[0]["declarations"],
            json!([["--tw-ring-color", "#a1a1a180"]])
        );
        assert_eq!(
            rules_of(&t, "shadow-xs")[0]["declarations"],
            json!([
                [
                    "--tw-shadow",
                    "0 1px 2px 0 var(--tw-shadow-color,#0000000d)"
                ],
                ["box-shadow", "var(--tw-ring-shadow), var(--tw-shadow)"]
            ])
        );
    }

    #[test]
    fn turns_variants_into_conditions() {
        let t = compile(concat!(
            r#".data-\[state\=checked\]\:bg-primary[data-state=checked]{background-color:#f00}"#,
            r#".dark\:bg-input\/30:is(.dark *){background-color:red}"#,
            r#".group-data-\[collapsible\=icon\]\:hidden:is(:where(.group)[data-collapsible=icon] *){display:none}"#,
            r#".peer-checked\:translate-x-2:is(:where(.peer):checked~*){--tw-translate-x:8px}"#,
            r#".\[\&_svg\]\:size-4 svg{width:16px;height:16px}"#,
            r#":where(.space-y-2>:not(:last-child)){margin-block-end:8px}"#,
            r#".placeholder\:text-muted::placeholder{color:red}"#,
            r#"@media (hover:hover){.hover\:bg-accent:hover{background-color:red}}"#,
            r#"@media (width>=40rem){.sm\:flex{display:flex}}"#,
            r#".disabled\:opacity-50:disabled{opacity:.5}"#,
            r#".has-\[\>svg\]\:px-3:has(>svg){padding-inline:12px}"#,
        ));
        let one = |c: &str| rules_of(&t, c).remove(0);
        assert_eq!(
            one("data-[state=checked]:bg-primary")["when"],
            json!([{ "attribute": "data-state", "equals": "checked" }])
        );
        assert_eq!(
            one("data-[state=checked]:bg-primary")["declarations"],
            json!([["background-color", "#ff0000ff"]])
        );
        assert_eq!(
            one("dark:bg-input/30")["when"],
            json!([{ "scheme": "dark" }])
        );
        assert_eq!(
            one("group-data-[collapsible=icon]:hidden")["when"],
            json!([{ "ancestor": { "classes": ["group"], "when": [{ "attribute": "data-collapsible", "equals": "icon" }] } }])
        );
        assert_eq!(
            one("peer-checked:translate-x-2")["when"],
            json!([{ "previous": { "classes": ["peer"], "when": [{ "attribute": "checked", "equals": null }] } }])
        );
        assert_eq!(
            one("[&_svg]:size-4")["target"],
            json!({ "relation": "descendant", "tag": "svg", "when": [], "pseudo": null })
        );
        let space = one("space-y-2");
        assert_eq!(space["target"]["relation"], "child");
        assert_eq!(space["specificity"], 0);
        assert_eq!(
            one("placeholder:text-muted")["target"],
            json!({ "pseudo": "placeholder" })
        );
        assert_eq!(
            one("hover:bg-accent")["when"],
            json!([{ "state": "hover" }, { "state": "media" }])
        );
        assert_eq!(one("sm:flex")["media"], json!({ "minWidth": 640.0 }));
        assert_eq!(
            one("disabled:opacity-50")["when"],
            json!([{ "attribute": "disabled", "equals": null }])
        );
        assert_eq!(one("has-[>svg]:px-3")["when"], json!([{ "has": ">svg" }]));
        let t2 = compile(
            r#"@layer base{*,:after{border-color:#e5e5e5}body{background-color:#fff}[hidden]:where(:not([hidden=until-found])){display:none}::placeholder{opacity:1}}.\[\&_svg\:not\(\[class\*\=\'size-\'\]\)\]\:size-4 svg:not([class*='size-']){width:16px}"#,
        );
        let elements: Vec<&Value> = t2["rules"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r.get("element").is_some())
            .collect();
        let of = |tag: &str| {
            elements
                .iter()
                .filter(|r| r["element"] == tag)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            of("*")[0]["declarations"],
            json!([["border-color", "#e5e5e5ff"]])
        );
        // `:after` is a pseudo-element of `*`, not `*` itself.
        assert_eq!(of("*")[1]["target"], json!({ "pseudo": "after" }));
        assert_eq!(of("body").len(), 1);
        assert_eq!(
            of("*")[2]["when"][0],
            json!({ "attribute": "hidden", "equals": null })
        );
        assert_eq!(of("*")[3]["target"], json!({ "pseudo": "placeholder" }));
        assert_eq!(
            rules_of(&t2, "[&_svg:not([class*='size-'])]:size-4")[0]["target"]["when"],
            json!([{ "not": [{ "attribute": "class", "operator": "*", "value": "size-" }] }])
        );
        let t = compile(
            r#".\[\.border-b\]\:pb-6.border-b{padding-bottom:24px}.after\:absolute:after{position:absolute}"#,
        );
        assert_eq!(
            rules_of(&t, "[.border-b]:pb-6")[0]["when"],
            json!([{ "class": "border-b" }])
        );
        assert_eq!(
            rules_of(&t, "after:absolute")[0]["target"],
            json!({ "pseudo": "after" })
        );
        // Specificity: the attribute variant outranks a plain utility.
        assert_eq!(one("data-[state=checked]:bg-primary")["specificity"], 20);
    }

    #[test]
    fn evaluates_calc_and_numbers() {
        assert_eq!(evaluate("calc(3px + 0px)"), "3px");
        assert_eq!(evaluate("calc(100% - 2px)"), "calc(100% - 2px)");
        assert_eq!(evaluate("calc(.25rem * -8)"), "-32px");
        assert_eq!(evaluate("1px solid #fff"), "1px solid #ffffffff");
        assert_eq!(evaluate("calc(1.25 / .875)"), "1.4286");
        assert_eq!(evaluate("0 0 #0000"), "0 0 #00000000");
        assert_eq!(evaluate("calc(infinity * 1px)"), "1000000px");
        // A conditional rule nested in a style rule holds that rule's declarations.
        let t = compile("::placeholder{color:currentcolor;@supports (color:color-mix(in lab, red, red)){color:color-mix(in oklab,currentcolor 50%,transparent)}}");
        let decls: Vec<Value> = t["rules"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["declarations"].clone())
            .collect();
        assert_eq!(
            decls,
            vec![
                json!([["color", "currentcolor"]]),
                json!([["color", "color-mix(in oklab,currentcolor 50%,#00000000)"]])
            ]
        );
    }

    #[test]
    fn reads_nebula_whole() {
        // nebula 0.2.6's prebuilt stylesheet: a whole component set's Tailwind output.
        let t = compile(include_str!("../tests/fixtures/nebula.css"));
        let rules = t["rules"].as_array().unwrap();
        assert!(rules.len() > 900, "{}", rules.len());
        assert_eq!(t["inert"], json!([]), "every class of nebula is read");
        // No declaration keeps a theme variable: only run-time ones remain.
        for r in rules {
            for d in r["declarations"].as_array().unwrap() {
                let text = d[1].to_string();
                for v in text.split("var(").skip(1) {
                    let name: String = v.chars().take_while(|c| *c != ',' && *c != ')').collect();
                    assert!(
                        t["properties"].get(&name).is_some()
                            || name.starts_with("--tw")
                            || !name.is_empty(),
                        "{name} in {r}"
                    );
                }
                assert!(!text.contains("oklch("), "{text}");
            }
        }
        assert!(rules_of(&t, "bg-primary").len() == 1);
    }
}
