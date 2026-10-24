//! JP data and a static scan of the dispatch claims, shared by the tests
//! (written independently of the generator, which validates the same way).
//!
//! JP data is read from `AC3_JP_DIR` (default `~/ac3-jp`): the function map
//! `analysis/jp_pal_map.tsv` and `pal_side.tsv`, the resident
//! `extracted/disc1/disc/ACE3.BIN` and the Redux RAM captures
//! `analysis/redux/run1/probe-*/ram.bin`.
#![allow(dead_code)]
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

/// The JP analysis directory (`AC3_JP_DIR`, default `~/ac3-jp`).
pub fn jp_dir() -> PathBuf {
    match std::env::var_os("AC3_JP_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(std::env::var_os("HOME").expect("HOME")).join("ac3-jp"),
    }
}

/// This crate's directory and the PAL crate's.
pub fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}
pub fn pal_crate_dir() -> PathBuf {
    crate_dir().join("../ac3-flight")
}

/// One row of `jp_pal_map.tsv`.
#[derive(Clone, Debug)]
pub struct Row {
    pub jp: u32,
    pub size: u32,
    pub region: String,
    pub class: String,
    pub pal: Option<u32>,
    pub pal_space: String,
    pub kinds: String,
    pub owner: String,
    pub name: String,
}

fn hex(text: &str) -> Option<u32> {
    u32::from_str_radix(text, 16).ok()
}

fn table(path: &Path) -> (Vec<String>, Vec<Vec<String>>) {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let mut lines = text.lines();
    let head = lines
        .next()
        .expect("header")
        .split('\t')
        .map(String::from)
        .collect();
    let rows = lines
        .filter(|l| !l.is_empty())
        .map(|l| l.split('\t').map(String::from).collect())
        .collect();
    (head, rows)
}

/// Every row of the JP function map.
pub fn jp_map(dir: &Path) -> Vec<Row> {
    let (head, rows) = table(&dir.join("analysis/jp_pal_map.tsv"));
    let col = |name: &str| head.iter().position(|h| h == name).expect(name);
    let c = [
        col("jp_address"),
        col("jp_size"),
        col("region"),
        col("class"),
        col("pal_address"),
        col("pal_space"),
        col("diff_kinds"),
        col("rust_owner"),
        col("jp_name"),
    ];
    rows.iter()
        .map(|f| Row {
            jp: hex(&f[c[0]]).expect("jp_address"),
            size: f[c[1]].parse().expect("jp_size"),
            region: f[c[2]].clone(),
            class: f[c[3]].clone(),
            pal: hex(&f[c[4]]),
            pal_space: f[c[5]].clone(),
            kinds: f[c[6]].clone(),
            owner: f[c[7]].clone(),
            name: f[c[8]].clone(),
        })
        .collect()
}

/// PAL resident routine sizes (`pal_side.tsv`): address -> bytes.
pub fn pal_sizes(dir: &Path) -> BTreeMap<u32, u32> {
    let (head, rows) = table(&dir.join("analysis/pal_side.tsv"));
    let col = |name: &str| head.iter().position(|h| h == name).expect(name);
    let (space, address, size) = (col("pal_space"), col("pal_address"), col("pal_size"));
    rows.iter()
        .filter(|f| f[space] == "pal")
        .filter_map(|f| Some((hex(&f[address])?, f[size].parse().ok()?)))
        .collect()
}

/// PAL resident routines without a resident JP counterpart (`pal_side.tsv`:
/// class pal-only, or a match only in a JP overlay).
pub fn pal_only(dir: &Path) -> Vec<u32> {
    let (head, rows) = table(&dir.join("analysis/pal_side.tsv"));
    let col = |name: &str| head.iter().position(|h| h == name).expect(name);
    let (space, address, class, jp_space) = (
        col("pal_space"),
        col("pal_address"),
        col("class"),
        col("jp_space"),
    );
    rows.iter()
        .filter(|f| f[space] == "pal" && (f[class] == "pal-only" || f[jp_space] != "jp"))
        .filter_map(|f| hex(&f[address]))
        .collect()
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Hex(u32),
    Ident(String),
    Path,
    Arrow,
    Pipe,
    RArrow,
    Brace,
    BraceEnd,
    Open,
    Close,
    Comma,
    Colon,
    Eq,
    Semi,
    Zero,
    /// `.` (a method or field follows; not a path).
    Dot,
    Other,
}

/// Code tokens of a Rust source with their lines (comments, strings and
/// chars skipped).
fn tokens(text: &str) -> Vec<(usize, Tok)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut i, mut line) = (0, 1);
    while i < b.len() {
        let c = b[i];
        let rest = &b[i..];
        if c == b'\n' {
            line += 1;
            i += 1;
        } else if c.is_ascii_whitespace() {
            i += 1;
        } else if rest.starts_with(b"//") {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if rest.starts_with(b"/*") {
            let mut depth = 0;
            while i < b.len() {
                if b[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if b[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    line += usize::from(b[i] == b'\n');
                    i += 1;
                }
            }
        } else if c == b'"' {
            i += 1;
            while i < b.len() && b[i] != b'"' {
                line += usize::from(b[i] == b'\n');
                i += if b[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c == b'\'' && rest.len() >= 3 && (rest[2] == b'\'' || rest[1] == b'\\') {
            i += 2;
            while i < b.len() && b[i] != b'\'' {
                i += 1;
            }
            i += 1;
        } else if rest.starts_with(b"0x") {
            let digits: String = text[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '_')
                .collect();
            let value = u64::from_str_radix(&digits.replace('_', ""), 16).unwrap_or(0);
            out.push((line, Tok::Hex(value as u32)));
            i += 2 + digits.len();
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
        } else if c.is_ascii_alphanumeric() || c == b'_' {
            let start = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let word = &text[start..i];
            out.push((
                line,
                if word == "0" {
                    Tok::Zero
                } else if c.is_ascii_digit() {
                    Tok::Other
                } else {
                    Tok::Ident(word.to_string())
                },
            ));
        } else {
            let (tok, n) = match (c, rest.get(1)) {
                (b'=', Some(b'>')) => (Tok::Arrow, 2),
                (b'-', Some(b'>')) => (Tok::RArrow, 2),
                (b':', Some(b':')) => (Tok::Path, 2),
                (b'|', Some(b'|')) | (b'=', Some(b'=')) => (Tok::Other, 2),
                (b'|', _) => (Tok::Pipe, 1),
                (b'{', _) => (Tok::Brace, 1),
                (b'}', _) => (Tok::BraceEnd, 1),
                (b'(' | b'[', _) => (Tok::Open, 1),
                (b')' | b']', _) => (Tok::Close, 1),
                (b',', _) => (Tok::Comma, 1),
                (b':', _) => (Tok::Colon, 1),
                (b'=', _) => (Tok::Eq, 1),
                (b';', _) => (Tok::Semi, 1),
                (b'.', _) => (Tok::Dot, 1),
                _ => (Tok::Other, 1),
            };
            out.push((line, tok));
            i += n;
        }
    }
    out
}

/// The Rust sources of a crate directory's `src/`, sorted by name; the
/// JP patch layer's files (`src/jp/`, at any depth) are named `jp/...`.
pub fn sources(crate_dir: &Path) -> Vec<(String, String)> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, String)>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for p in entries.map(|e| e.expect("entry").path()) {
            let name = format!("{prefix}{}", p.file_name().unwrap().to_string_lossy());
            if p.is_dir() && (name == "jp" || name.starts_with("jp/")) {
                walk(&p, &format!("{name}/"), out);
            } else if p.extension().is_some_and(|e| e == "rs") {
                out.push((name, fs::read_to_string(&p).expect("source")));
            }
        }
    }
    let mut files = Vec::new();
    walk(&crate_dir.join("src"), "", &mut files);
    files.sort();
    files
}

/// The overlay module a patch-layer file belongs to (`jp/ov2.rs`,
/// `jp/ov2/hub.rs` -> 2), if any.
pub fn overlay_of(file: &str) -> Option<u32> {
    let rest = file.strip_prefix("jp/ov")?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let after = &rest[digits.len()..];
    (after == ".rs" || after.starts_with('/') || after.starts_with('_'))
        .then(|| digits.parse().ok())
        .flatten()
}

/// Resident claims of the JP crate: the generated modules and the patch
/// layer's resident files (the overlay patch modules are apart).
pub fn jp_resident_claims() -> (Claims, BTreeSet<u32>) {
    claims_and_void(&crate_dir(), &|name| overlay_of(name).is_some())
}

/// Claims of the patch layer's overlay modules: (module, address) -> sites.
pub fn jp_overlay_claims() -> BTreeMap<(u32, u32), Vec<(String, usize)>> {
    let mut out = BTreeMap::new();
    for n in 1..=4 {
        let (claims, _) = claims_and_void(&crate_dir(), &|name| overlay_of(name) != Some(n));
        for (address, sites) in claims {
            out.insert((n, address), sites);
        }
    }
    out
}

/// The JP addresses of a `pub const NAME: &[u32] = &[...];` table of the
/// patch layer (`src/jp/mod.rs`), e.g. `TAKES_OVER`.
pub fn patch_table(name: &str) -> Vec<u32> {
    let Ok(text) = fs::read_to_string(crate_dir().join("src/jp/mod.rs")) else {
        return Vec::new();
    };
    let t = tokens(&text);
    let Some(at) = t.iter().position(|x| x.1 == Tok::Ident(name.to_string())) else {
        return Vec::new();
    };
    t[at..]
        .iter()
        .take_while(|x| x.1 != Tok::Semi)
        .filter_map(|x| match x.1 {
            Tok::Hex(v) => Some(v),
            _ => None,
        })
        .collect()
}

/// Dispatch claims of a crate: the patterns of every `match target` arm in
/// every `fn dispatch` (nested module dispatchers included), a literal or a
/// `const` resolved by name over the crate: address -> claiming sites.
/// `skip` leaves out files (the PAL overlay modules).
pub fn claims(crate_dir: &Path, skip: &dyn Fn(&str) -> bool) -> Claims {
    claims_and_void(crate_dir, skip).0
}

/// Claimed address -> the (file, line) of each claiming arm.
pub type Claims = BTreeMap<u32, Vec<(String, usize)>>;

/// The claims, and the claimed addresses whose arm is `{ ...; 0 }` or calls
/// a fn that models no result (see [`Items`]): the translation models no
/// return value (the original's `v0` is whatever its last computation
/// left), so `v0` is not compared for them.
pub fn claims_and_void(crate_dir: &Path, skip: &dyn Fn(&str) -> bool) -> (Claims, BTreeSet<u32>) {
    let all: Vec<(String, Vec<(usize, Tok)>)> = sources(crate_dir)
        .into_iter()
        .map(|(name, text)| (name, tokens(&text)))
        .collect();
    let items = Items::new(&all);
    let files: Vec<(String, Vec<(usize, Tok)>)> =
        all.into_iter().filter(|(name, _)| !skip(name)).collect();
    let mut consts: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (_, t) in &files {
        for w in t.windows(7) {
            if let [
                (_, Tok::Ident(k)),
                (_, Tok::Ident(name)),
                (_, Tok::Colon),
                (_, Tok::Ident(ty)),
                (_, Tok::Eq),
                (_, Tok::Hex(v)),
                (_, Tok::Semi),
            ] = w
                && k == "const"
                && ty == "u32"
            {
                consts.entry(name.clone()).or_default().push(*v);
            }
        }
    }
    let resolve = |name: &str| match consts.get(name).map(Vec::as_slice) {
        Some([first, rest @ ..]) if rest.iter().all(|v| v == first) => Some(*first),
        _ => None,
    };
    let mut out = Claims::new();
    let mut void = BTreeSet::new();
    for (file, t) in &files {
        let void_fn = |path: &[String]| items.void_fn(file, path, 0);
        for i in 0..t.len().saturating_sub(1) {
            let is_fn = matches!((&t[i].1, &t[i + 1].1),
                (Tok::Ident(a), Tok::Ident(b)) if a == "fn" && b == "dispatch");
            if !is_fn {
                continue;
            }
            let Some(body) = (i..t.len()).find(|&j| t[j].1 == Tok::Brace) else {
                continue;
            };
            let end = block_end(t, body);
            for k in body..end.saturating_sub(2) {
                let is_match = matches!((&t[k].1, &t[k + 1].1, &t[k + 2].1),
                    (Tok::Ident(a), Tok::Ident(b), Tok::Brace) if a == "match" && b == "target");
                if is_match {
                    arms(
                        file,
                        &t[k + 2..block_end(t, k + 2) + 1],
                        &resolve,
                        &void_fn,
                        &mut out,
                        &mut void,
                    );
                }
            }
        }
    }
    (out, void)
}

/// The fns and `use` imports of a crate's sources, to find the fn a
/// dispatch arm calls by its module path: a bare name is the file's own fn,
/// else the one its `use` imports name (or a glob import provides); `a::b::f`
/// resolves `a::b` from the file (`crate`, `self`, `super`, an imported
/// module, a child module). Found by `jp-ov2b`: two files' local `set_flag`
/// helpers, one of them without a result, made every `set_flag` arm void
/// when the scan went by the bare name.
struct Items {
    /// The source files (`jp/ov1_p1.rs`, `lib.rs`, ...).
    files: BTreeSet<String>,
    /// File -> fn name -> whether every fn of that name in the file models
    /// no result (no `->`, or a body ending `; 0`).
    fns: BTreeMap<String, BTreeMap<String, bool>>,
    /// File -> its `use` imports: (local name, path as written); the name
    /// is `*` for a glob import.
    uses: BTreeMap<String, Vec<(String, Vec<String>)>>,
}

impl Items {
    /// Fns nested in the sources' bodies and impls count for their file.
    fn new(files: &[(String, Vec<(usize, Tok)>)]) -> Self {
        let mut items = Items {
            files: files.iter().map(|(name, _)| name.clone()).collect(),
            fns: BTreeMap::new(),
            uses: BTreeMap::new(),
        };
        for (file, t) in files {
            let fns = items.fns.entry(file.clone()).or_default();
            let uses = items.uses.entry(file.clone()).or_default();
            for i in 0..t.len().saturating_sub(1) {
                match (&t[i].1, &t[i + 1].1) {
                    (Tok::Ident(f), Tok::Ident(name)) if f == "fn" => {
                        // the body: the first `{` outside the signature's
                        // brackets (`[u32; 4]`); a `;` first is a declaration
                        let mut depth = 0i32;
                        let Some(body) = (i..t.len()).find(|&j| {
                            match t[j].1 {
                                Tok::Open => depth += 1,
                                Tok::Close => depth -= 1,
                                _ => {}
                            }
                            depth == 0 && matches!(t[j].1, Tok::Brace | Tok::Semi)
                        }) else {
                            continue;
                        };
                        if t[body].1 != Tok::Brace {
                            continue;
                        }
                        let end = block_end(t, body);
                        let returns = t[i..body].iter().any(|x| x.1 == Tok::RArrow);
                        let void = !returns
                            || (end >= 2 && t[end - 2].1 == Tok::Semi && t[end - 1].1 == Tok::Zero);
                        let entry = fns.entry(name.clone()).or_insert(true);
                        *entry = *entry && void;
                    }
                    (Tok::Ident(u), _) if u == "use" => {
                        use_tree(t, i + 1, &[], uses);
                    }
                    _ => {}
                }
            }
        }
        items
    }

    /// The source of a module (`["jp", "ov1_p1"]` -> `jp/ov1_p1.rs` or
    /// `jp/ov1_p1/mod.rs`; the crate root `lib.rs`).
    fn file_of(&self, module: &[String]) -> Option<String> {
        let candidates = if module.is_empty() {
            vec!["lib.rs".to_string(), "main.rs".to_string()]
        } else {
            let base = module.join("/");
            vec![format!("{base}.rs"), format!("{base}/mod.rs")]
        };
        candidates.into_iter().find(|f| self.files.contains(f))
    }

    /// The module path written in `file` (`super::x`, an imported or child
    /// module) from the crate root; None outside the crate.
    fn module(&self, file: &str, path: &[String], depth: u32) -> Option<Vec<String>> {
        let here = module_of(file);
        let (first, rest) = path.split_first()?;
        let mut out = match first.as_str() {
            "crate" => Vec::new(),
            "self" => here,
            "super" => {
                let mut parent = here;
                parent.pop()?;
                parent
            }
            name => {
                let imported = self
                    .uses
                    .get(file)
                    .and_then(|u| u.iter().find(|(a, _)| a == name));
                match imported {
                    Some((_, p)) if depth < 8 => self.module(file, p, depth + 1)?,
                    Some(_) => return None,
                    None => {
                        let mut child = here;
                        child.push(name.to_string());
                        self.file_of(&child)?;
                        child
                    }
                }
            }
        };
        for segment in rest {
            match segment.as_str() {
                "super" => {
                    out.pop()?;
                }
                "self" => {}
                name => out.push(name.to_string()),
            }
        }
        Some(out)
    }

    /// Whether the fn `path` names in `file` models no result; None when the
    /// path is no fn of the crate's sources (a closure, a variable, a
    /// method, another crate's fn).
    fn void_fn(&self, file: &str, path: &[String], depth: u32) -> Option<bool> {
        let (name, module) = path.split_last()?;
        if module.is_empty() {
            return self.lookup(file, name, depth);
        }
        let target = self.file_of(&self.module(file, module, 0)?)?;
        self.lookup(&target, name, depth)
    }

    /// The fn `name` of `file`: its own, else imported by name, else from a
    /// glob import (void only when every such fn is).
    fn lookup(&self, file: &str, name: &str, depth: u32) -> Option<bool> {
        if depth > 8 {
            return None;
        }
        if let Some(void) = self.fns.get(file).and_then(|f| f.get(name)) {
            return Some(*void);
        }
        let uses = self.uses.get(file)?;
        if let Some((_, p)) = uses.iter().find(|(a, _)| a == name) {
            return self.void_fn(file, p, depth + 1);
        }
        let mut found = None;
        for (_, p) in uses.iter().filter(|(a, _)| a == "*") {
            let target = self.module(file, p, 0).and_then(|m| self.file_of(&m));
            if let Some(void) = target.and_then(|f| self.lookup(&f, name, depth + 1)) {
                found = Some(found.unwrap_or(true) && void);
            }
        }
        found
    }
}

/// The module path of a source file (`jp/ov2/hub.rs` -> jp::ov2::hub,
/// `jp/mod.rs` -> jp, `lib.rs` -> the root).
fn module_of(file: &str) -> Vec<String> {
    let stem = file.strip_suffix(".rs").unwrap_or(file);
    let mut out: Vec<String> = stem.split('/').map(str::to_string).collect();
    if matches!(out.last().map(String::as_str), Some("mod" | "lib" | "main")) {
        out.pop();
    }
    out
}

/// One `use` tree from `t[i]` (after `use`, or inside its braces) with the
/// path so far, into `out` as (local name, path); returns the index after it.
fn use_tree(
    t: &[(usize, Tok)],
    mut i: usize,
    prefix: &[String],
    out: &mut Vec<(String, Vec<String>)>,
) -> usize {
    let mut path = prefix.to_vec();
    loop {
        match t.get(i).map(|x| &x.1) {
            Some(Tok::Ident(s)) if s != "as" => {
                path.push(s.clone());
                i += 1;
                if t.get(i).map(|x| &x.1) == Some(&Tok::Path) {
                    i += 1;
                    continue;
                }
                break;
            }
            Some(Tok::Brace) => {
                i += 1;
                while let Some(tok) = t.get(i) {
                    match tok.1 {
                        Tok::BraceEnd => return i + 1,
                        Tok::Comma => i += 1,
                        Tok::Semi => return i,
                        _ => {
                            let next = use_tree(t, i, &path, out);
                            i = if next > i { next } else { i + 1 };
                        }
                    }
                }
                return i;
            }
            Some(Tok::Other) => {
                // `*`
                out.push(("*".to_string(), path));
                return i + 1;
            }
            _ => return i,
        }
    }
    let mut name = path.last().cloned().unwrap_or_default();
    if name == "self" {
        path.pop();
        name = path.last().cloned().unwrap_or_default();
    }
    if let (Some(Tok::Ident(a)), Some(Tok::Ident(alias))) =
        (t.get(i).map(|x| &x.1), t.get(i + 1).map(|x| &x.1))
        && a == "as"
    {
        name = alias.clone();
        i += 2;
    }
    out.push((name, path));
    i
}

/// Whether the statements `s1; s2; ...;` (each ending with its `;`) are all
/// calls of fns without a result, and there is one at least.
fn void_statements(statements: &[&Tok], void_fn: &dyn Fn(&[String]) -> Option<bool>) -> bool {
    let (mut depth, mut start, mut any) = (0i32, 0usize, false);
    for (i, tok) in statements.iter().enumerate() {
        match tok {
            Tok::Open | Tok::Brace => depth += 1,
            Tok::Close | Tok::BraceEnd => depth -= 1,
            Tok::Semi if depth == 0 => {
                match body_paths(&statements[start..i]).first() {
                    Some((0, head)) if void_fn(head) == Some(true) => any = true,
                    _ => return false,
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    any && start == statements.len()
}

/// The paths a dispatch arm's body names (`f`, `a::b::f`), in order, with
/// their token index; a name after `.` (a method, a field) is none.
fn body_paths(body: &[&Tok]) -> Vec<(usize, Vec<String>)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let after_dot = i > 0 && *body[i - 1] == Tok::Dot;
        let Tok::Ident(first) = body[i] else {
            i += 1;
            continue;
        };
        let start = i;
        let mut path = vec![first.clone()];
        i += 1;
        while i + 1 < body.len() && *body[i] == Tok::Path {
            let Tok::Ident(next) = body[i + 1] else {
                break;
            };
            path.push(next.clone());
            i += 2;
        }
        if !after_dot {
            out.push((start, path));
        }
    }
    out
}

/// `const NAME: u32 = 0x...;` values of a crate by name.
fn const_values(files: &[(String, Vec<(usize, Tok)>)]) -> BTreeMap<String, Vec<u32>> {
    let mut consts: BTreeMap<String, Vec<u32>> = BTreeMap::new();
    for (_, t) in files {
        for w in t.windows(7) {
            if let [
                (_, Tok::Ident(k)),
                (_, Tok::Ident(name)),
                (_, Tok::Colon),
                (_, Tok::Ident(ty)),
                (_, Tok::Eq),
                (_, Tok::Hex(v)),
                (_, Tok::Semi),
            ] = w
                && k == "const"
                && ty == "u32"
            {
                consts.entry(name.clone()).or_default().push(*v);
            }
        }
    }
    consts
}

/// The first argument of every `host_call`/`host_call_result` call, per
/// file: its text (a literal, a path or a variable) and its value when it
/// is a literal or a const (of the same file, else of the crate when its
/// name has one value).
pub fn host_ids(crate_dir: &Path) -> BTreeMap<String, Vec<(String, Option<u32>)>> {
    let files: Vec<(String, Vec<(usize, Tok)>)> = sources(crate_dir)
        .into_iter()
        .map(|(name, text)| (name, tokens(&text)))
        .collect();
    let consts = const_values(&files);
    let mut out: BTreeMap<String, Vec<(String, Option<u32>)>> = BTreeMap::new();
    for (file, t) in &files {
        let local = const_values(std::slice::from_ref(&(file.clone(), t.clone())));
        for i in 0..t.len().saturating_sub(2) {
            let is_call = matches!((&t[i].1, &t[i + 1].1),
                (Tok::Ident(f), Tok::Open) if f == "host_call" || f == "host_call_result");
            if !is_call {
                continue;
            }
            let mut text = String::new();
            let mut value = None;
            let mut simple = true;
            for (_, tok) in &t[i + 2..] {
                match tok {
                    Tok::Comma | Tok::Close => break,
                    Tok::Hex(v) => {
                        text.push_str(&format!("{v:#x}"));
                        value = Some(*v);
                    }
                    Tok::Ident(name) => {
                        text.push_str(name);
                        let table = if local.contains_key(name) {
                            &local
                        } else {
                            &consts
                        };
                        value = match table.get(name).map(Vec::as_slice) {
                            Some([first, rest @ ..]) if rest.iter().all(|v| v == first) => {
                                Some(*first)
                            }
                            _ => None,
                        };
                    }
                    Tok::Path => text.push_str("::"),
                    _ => simple = false,
                }
            }
            if simple {
                out.entry(file.clone()).or_default().push((text, value));
            }
        }
    }
    out
}

/// Index of the `}` closing the brace at `open`.
fn block_end(t: &[(usize, Tok)], open: usize) -> usize {
    let mut depth = 0;
    for (j, tok) in t.iter().enumerate().skip(open) {
        match tok.1 {
            Tok::Brace => depth += 1,
            Tok::BraceEnd => {
                depth -= 1;
                if depth == 0 {
                    return j;
                }
            }
            _ => {}
        }
    }
    t.len()
}

/// The arm patterns of the match block `t` (from its `{` to its `}`):
/// tokens at the block's own depth between an arm's start and its `=>`.
fn arms(
    file: &str,
    t: &[(usize, Tok)],
    resolve: &dyn Fn(&str) -> Option<u32>,
    void_fn: &dyn Fn(&[String]) -> Option<bool>,
    out: &mut Claims,
    void: &mut BTreeSet<u32>,
) {
    let mut depth = 0i32;
    let mut pattern: Vec<&(usize, Tok)> = Vec::new();
    let mut in_body = false;
    let mut arm_values: Vec<u32> = Vec::new();
    let mut prev: [&Tok; 2] = [&Tok::Other, &Tok::Other];
    let mut body: Vec<&Tok> = Vec::new();
    for tok in t {
        if in_body && depth >= 1 {
            body.push(&tok.1);
        }
        match tok.1 {
            Tok::Brace | Tok::Open => depth += 1,
            Tok::BraceEnd | Tok::Close => {
                depth -= 1;
                if depth == 1 && in_body && tok.1 == Tok::BraceEnd {
                    // a block body ends the arm (an optional `,` follows):
                    // `{ ...; 0 }` is void, and so is `{ f(m); ...; Some(0) }`
                    // (an `Option` dispatcher) when every statement calls a
                    // fn without a result (found by `jp-res3`)
                    let n = body.len();
                    let some_zero = n >= 7
                        && *body[n - 6] == Tok::Semi
                        && *body[n - 5] == Tok::Ident("Some".to_string())
                        && *body[n - 4] == Tok::Open
                        && *body[n - 3] == Tok::Zero
                        && *body[n - 2] == Tok::Close
                        && void_statements(&body[1..n - 5], void_fn);
                    if (*prev[0] == Tok::Semi && *prev[1] == Tok::Zero) || some_zero {
                        void.extend(arm_values.iter().copied());
                    }
                    in_body = false;
                    pattern.clear();
                }
            }
            Tok::Arrow if depth == 1 && !in_body => {
                for alt in pattern.split(|x| x.1 == Tok::Pipe) {
                    let value = match alt {
                        [(_, Tok::Hex(v))] => Some(*v),
                        [.., (_, Tok::Ident(name))]
                            if alt.iter().all(|x| matches!(x.1, Tok::Ident(_) | Tok::Path)) =>
                        {
                            resolve(name)
                        }
                        _ => None,
                    };
                    if let (Some(v), Some(first)) = (value, alt.first()) {
                        out.entry(v).or_default().push((file.to_string(), first.0));
                        arm_values.push(v);
                    }
                }
                pattern.clear();
                in_body = true;
                body.clear();
            }
            Tok::Comma if depth == 1 => {
                // a body that calls a fn without a result is void; so is one
                // that names such a fn (a table of wrappers, or one passed to
                // a `void(m, f)` closure) unless it calls a fn with a result.
                // The fn is the one the name resolves to ([`Items`]).
                if body.first() != Some(&&Tok::Brace) {
                    let paths = body_paths(&body[..body.len().min(12)]);
                    let named = |paths: &[(usize, Vec<String>)]| {
                        paths.iter().any(|(_, p)| void_fn(p) == Some(true))
                    };
                    let is_void = match paths.split_first() {
                        Some(((0, head), rest)) => void_fn(head).unwrap_or_else(|| named(rest)),
                        _ => named(&paths),
                    };
                    if is_void {
                        void.extend(arm_values.iter().copied());
                    }
                }
                in_body = false;
                pattern.clear();
                arm_values.clear();
            }
            _ if depth == 1 && !in_body => {
                if pattern.is_empty() {
                    arm_values.clear();
                }
                pattern.push(tok);
            }
            _ => {}
        }
        prev = [prev[1], &tok.1];
    }
}
