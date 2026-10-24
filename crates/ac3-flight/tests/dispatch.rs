//! Every original routine has one implementation: a static scan of every
//! `pub fn dispatch` body in `src/` (nested module dispatchers included)
//! finds no address claimed by two dispatchers, apart from the listed
//! duplicates whose removal belongs to modules that are still being
//! extended. Every module with a top-level `dispatch` is asked by
//! `dispatch::call`.
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

/// Duplicates pending in a module still being extended (address and the
/// claiming modules, sorted): none.
const PENDING: &[(u32, &[&str])] = &[];

fn sources() -> Vec<(String, String)> {
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files: Vec<(String, String)> = fs::read_dir(&directory)
        .expect("src")
        .map(|entry| entry.expect("entry").path())
        .filter(|path| path.extension().is_some_and(|e| e == "rs"))
        .map(|path| {
            let module = path.file_stem().unwrap().to_string_lossy().into_owned();
            (module, fs::read_to_string(&path).expect("source"))
        })
        .collect();
    files.sort();
    files
}

#[derive(Debug, PartialEq)]
enum Token {
    Hex(u32),
    Arrow,
    Pipe,
    Open,
    Close,
    Other,
}

/// Tokens of `text` from byte `start`, with their offsets: hex literals,
/// `=>`, single `|`, braces; comments, string and char literals skipped.
fn tokens(text: &str, start: usize) -> Vec<(usize, Token)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        let rest = &bytes[i..];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if rest.starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if c == b'"' {
            i += 1;
            while i < bytes.len() && bytes[i] != b'"' {
                i += if bytes[i] == b'\\' { 2 } else { 1 };
            }
            i += 1;
        } else if c == b'\'' && rest.len() >= 3 && (rest[2] == b'\'' || rest[1] == b'\\') {
            // A char literal ('x' or '\x'); a lone quote is a lifetime.
            i += 2;
            while i < bytes.len() && bytes[i] != b'\'' {
                i += 1;
            }
            i += 1;
        } else if rest.starts_with(b"0x") {
            let digits: String = text[i + 2..]
                .chars()
                .take_while(|c| c.is_ascii_hexdigit() || *c == '_')
                .collect();
            let value = u64::from_str_radix(&digits.replace('_', ""), 16).unwrap_or(0);
            out.push((i, Token::Hex(value as u32)));
            i += 2 + digits.len();
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
        } else if rest.starts_with(b"=>") {
            out.push((i, Token::Arrow));
            i += 2;
        } else if rest.starts_with(b"||") {
            out.push((i, Token::Other));
            i += 2;
        } else if c == b'|' {
            out.push((i, Token::Pipe));
            i += 1;
        } else if c == b'{' {
            out.push((i, Token::Open));
            i += 1;
        } else if c == b'}' {
            out.push((i, Token::Close));
            i += 1;
        } else if c.is_ascii_alphanumeric() || c == b'_' {
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push((i, Token::Other));
        } else {
            out.push((i, Token::Other));
            i += 1;
        }
    }
    out
}

/// The addresses claimed by match arms in the `dispatch` body opening at
/// the first `{` after `start`: every literal of a pattern `0x.. | 0x.. =>`.
fn body_claims(text: &str, start: usize) -> Vec<u32> {
    let all = tokens(text, start);
    let open = all
        .iter()
        .position(|(_, t)| *t == Token::Open)
        .expect("body");
    let mut depth = 0;
    let mut end = open;
    for (k, (_, token)) in all.iter().enumerate().skip(open) {
        match token {
            Token::Open => depth += 1,
            Token::Close => {
                depth -= 1;
                if depth == 0 {
                    end = k;
                    break;
                }
            }
            _ => {}
        }
    }
    let body: Vec<&Token> = all[open..end].iter().map(|(_, t)| t).collect();
    let mut claims = Vec::new();
    let mut k = 0;
    while k < body.len() {
        if let Token::Hex(_) = body[k] {
            // A chain `hex (| hex)*` is a pattern when `=>` follows it.
            let mut chain = Vec::new();
            let mut j = k;
            while let Some(Token::Hex(value)) = body.get(j) {
                chain.push(*value);
                if body.get(j + 1) == Some(&&Token::Pipe) {
                    j += 2;
                } else {
                    j += 1;
                    break;
                }
            }
            let is_pattern = body.get(j) == Some(&&Token::Arrow)
                && (k == 0 || matches!(body[k - 1], Token::Open | Token::Close | Token::Other));
            if is_pattern {
                claims.extend(
                    chain
                        .iter()
                        .filter(|a| (0x8001_0000..0x8010_0000).contains(*a)),
                );
            }
            k = j.max(k + 1);
        } else {
            k += 1;
        }
    }
    claims
}

/// Overlay modules (`ov<N>*.rs`) share load addresses with each other, so
/// the one-owner rule holds within each overlay and within resident code.
fn group(module: &str) -> String {
    match module.strip_prefix("ov") {
        Some(rest) if rest.starts_with(|c: char| c.is_ascii_digit()) => {
            format!("ov{}", &rest[..1])
        }
        _ => "resident".to_owned(),
    }
}

/// Every claimed (group, address) with the modules claiming it (one entry
/// per claim).
fn scan() -> BTreeMap<(String, u32), Vec<String>> {
    let mut claims: BTreeMap<(String, u32), Vec<String>> = BTreeMap::new();
    for (module, text) in sources() {
        for (start, _) in text.match_indices("pub fn dispatch(") {
            for address in body_claims(&text, start) {
                claims
                    .entry((group(&module), address))
                    .or_default()
                    .push(module.clone());
            }
        }
    }
    claims
}

#[test]
fn no_routine_is_claimed_twice() {
    let claims = scan();
    assert!(claims.len() > 600, "only {} claims found", claims.len());
    let mut duplicates: Vec<(u32, Vec<String>)> = claims
        .into_iter()
        .filter(|(_, modules)| modules.len() > 1)
        .map(|((_, address), mut modules)| {
            modules.sort();
            (address, modules)
        })
        .collect();
    duplicates.sort();
    let expected: Vec<(u32, Vec<String>)> = PENDING
        .iter()
        .map(|(address, modules)| (*address, modules.iter().map(|m| m.to_string()).collect()))
        .collect();
    let describe = |list: &[(u32, Vec<String>)]| {
        list.iter()
            .map(|(a, m)| format!("{a:08x} {m:?}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    assert!(
        duplicates == expected,
        "addresses claimed by several dispatchers:\n{}\nexpected only the pending ones:\n{}",
        describe(&duplicates),
        describe(&expected)
    );
}

/// The arm scan sees or-patterns, nested dispatchers and multi-line arms,
/// and ignores expressions, comments and strings.
#[test]
fn claim_scan_reads_patterns_only() {
    let text = r#"
pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32> {
    // 0x8002_0000 => in a comment
    let s = "0x8002_0004 =>";
    let x = args[0] | 0x8002_0008;
    Some(match target {
        0x8002_0010 => f(m, x | 0x8002_000c),
        0x8002_0014
        | 0x8002_0018 | 0x8002_001c => {
            if x == 0x8002_0020 { 1 } else { 2 }
        }
        0x8002_0024 => match args[1] { 0x8002_0028 => 1, _ => 0 },
        _ => return None,
    })
}"#;
    let claims = body_claims(text, text.find("pub fn dispatch(").unwrap());
    assert_eq!(
        claims,
        [
            0x8002_0010,
            0x8002_0014,
            0x8002_0018,
            0x8002_001c,
            0x8002_0024,
            0x8002_0028
        ],
        "inner matches on arguments are claims too: keep them out of dispatch bodies"
    );
}

#[test]
fn every_dispatcher_is_asked() {
    let sources = sources();
    let table = &sources
        .iter()
        .find(|(module, _)| module == "dispatch")
        .expect("dispatch.rs")
        .1;
    let owners: Vec<&str> = table
        .match_indices("crate::")
        .filter_map(|(at, _)| table[at + 7..].split("::dispatch").next())
        .filter(|name| {
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .collect();
    for (module, text) in &sources {
        if text
            .lines()
            .any(|line| line.starts_with("pub fn dispatch("))
        {
            let asked = owners.iter().filter(|owner| *owner == module).count();
            assert_eq!(asked, 1, "{module}::dispatch in dispatch::OWNERS");
        }
    }
}
