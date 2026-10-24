//! Static validation of the generated crate (tools/jp/readdress.py): the
//! dispatch claims survive re-addressing. Every PAL claim of a resident
//! routine with a JP counterpart is a JP claim of that function, as often as
//! in PAL; the claims of PAL-only routines are gone; and no JP claim is
//! left that is not a function claim or a mapped inner address. Overlay
//! modules are not generated (JP overlays 1 and 2 are a different program),
//! so their claims are out of scope.
mod support;

use std::collections::BTreeMap;
use support::scan;

/// PAL resident address -> JP address, from the function map (function
/// starts of exact, near and changed routines).
fn counterparts(rows: &[scan::Row]) -> BTreeMap<u32, u32> {
    rows.iter()
        .filter(|r| r.region.starts_with("resident") && r.pal_space == "pal")
        .filter(|r| matches!(r.class.as_str(), "exact" | "near" | "changed"))
        .filter_map(|r| Some((r.pal?, r.jp)))
        .collect()
}

fn pal_claims() -> BTreeMap<u32, Vec<(String, usize)>> {
    scan::claims(&scan::pal_crate_dir(), &|name| name.starts_with("ov"))
}

/// Resident claims of the JP crate: generated modules and patch layer.
fn jp_claims() -> BTreeMap<u32, Vec<(String, usize)>> {
    scan::jp_resident_claims().0
}

fn generated(sites: &[(String, usize)]) -> usize {
    sites
        .iter()
        .filter(|(file, _)| !file.starts_with("jp/"))
        .count()
}

#[test]
#[ignore = "requires AC3_JP_DIR"]
fn dispatch_claims_survive_readdressing() {
    let dir = scan::jp_dir();
    let map = counterparts(&scan::jp_map(&dir));
    let pal_only = scan::pal_only(&dir);
    let (pal, jp) = (pal_claims(), jp_claims());
    let mut expected: BTreeMap<u32, usize> = BTreeMap::new();
    let (mut functions, mut inner, mut dropped) = (0, 0, 0);
    for (&address, sites) in &pal {
        if let Some(jp_address) = map.get(&address) {
            *expected.entry(*jp_address).or_default() += sites.len();
            functions += 1;
        } else if pal_only.contains(&address) {
            dropped += 1;
        } else {
            inner += 1; // a continuation inside or between routines
        }
    }
    let lost: Vec<String> = expected
        .iter()
        .filter(|&(a, n)| jp.get(a).map_or(0, Vec::len) != *n)
        .map(|(a, n)| format!("{a:08x}: PAL {n}, JP {}", jp.get(a).map_or(0, Vec::len)))
        .collect();
    let jp_starts: Vec<u32> = map.values().copied().collect();
    let pal_only_claimed: Vec<String> = jp
        .keys()
        .filter(|a| pal_only.contains(a) && !jp_starts.contains(a))
        .map(|a| format!("{a:08x}"))
        .collect();
    // claims of the generated modules beyond the function claims (the patch
    // layer's own routines are checked by `owners_claim_each_address_once`)
    let extra = jp
        .iter()
        .filter(|(a, sites)| !expected.contains_key(a) && generated(sites) > 0)
        .count();
    println!(
        "claims: PAL {} addresses ({functions} routines with a JP counterpart, {inner} inner \
         addresses, {dropped} PAL-only); JP {} addresses ({extra} inner); mismatches {}",
        pal.len(),
        jp.len(),
        lost.len()
    );
    assert!(lost.is_empty(), "claims lost or duplicated: {lost:?}");
    assert!(
        pal_only_claimed.is_empty(),
        "PAL-only routines still claimed: {pal_only_claimed:?}"
    );
    assert!(
        extra <= inner,
        "{extra} JP claims are neither function claims nor inner addresses"
    );
    assert!(dropped > 0 && functions > 1000, "the scan found the claims");
}

/// Every resident JP function whose PAL counterpart is dispatched in PAL is
/// claimed as often in the JP crate (once, apart from PAL's own pending
/// duplicates); the rest are called natively.
#[test]
#[ignore = "requires AC3_JP_DIR"]
fn translated_counterparts_are_claimed_once() {
    let rows = scan::jp_map(&scan::jp_dir());
    let (pal, jp) = (pal_claims(), jp_claims());
    let mut counts: BTreeMap<(String, &str), usize> = BTreeMap::new();
    let mut bad = Vec::new();
    for r in rows
        .iter()
        .filter(|r| r.region.starts_with("resident") && r.pal_space == "pal")
        .filter(|r| matches!(r.class.as_str(), "exact" | "near" | "changed"))
    {
        let Some(p) = r.pal else { continue };
        let in_pal = pal.get(&p).map_or(0, Vec::len);
        let in_jp = jp.get(&r.jp).map_or(0, Vec::len);
        let patch_only = jp.get(&r.jp).is_some_and(|sites| generated(sites) == 0);
        let state = match (in_pal, in_jp) {
            (0, 0) => "not dispatched (native calls only)",
            // A routine PAL never dispatched (untranslated there, e.g. unreached
            // by the PAL runs) that a JP patch translates and claims.
            (0, 1) if patch_only => "claimed by the patch layer only",
            (1, 1) => "claimed once",
            (a, b) if a == b => "claimed as often as in PAL",
            _ => {
                bad.push(format!(
                    "{:08x} (PAL {p:08x}): PAL {in_pal}, JP {in_jp}",
                    r.jp
                ));
                "mismatch"
            }
        };
        *counts.entry((r.class.clone(), state)).or_default() += 1;
    }
    for ((class, state), n) in &counts {
        println!("{class:8} {state:36} {n}");
    }
    assert!(bad.is_empty(), "{bad:?}");
}

/// One owner per address: the generated modules and the patch layer
/// together claim every resident address once (as often as PAL does for
/// PAL's own pending duplicates); each overlay patch module claims an
/// address once; every patch-layer claim is a JP function start of its
/// region (resident, or its overlay); a TAKES_OVER address is claimed by the
/// patch layer and by no generated module.
#[test]
#[ignore = "requires AC3_JP_DIR"]
fn owners_claim_each_address_once() {
    let rows = scan::jp_map(&scan::jp_dir());
    let pal = pal_claims();
    let map = counterparts(&rows);
    let mut allowed: BTreeMap<u32, usize> = BTreeMap::new();
    for (address, sites) in &pal {
        if let Some(jp) = map.get(address) {
            *allowed.entry(*jp).or_default() += sites.len();
        }
    }
    let jp = jp_claims();
    let mut bad = Vec::new();
    for (address, sites) in &jp {
        let limit = allowed.get(address).copied().unwrap_or(1).max(1);
        if sites.len() > limit {
            bad.push(format!(
                "{address:08x} claimed {} times: {sites:?}",
                sites.len()
            ));
        }
    }
    let resident: Vec<u32> = rows
        .iter()
        .filter(|r| r.region.starts_with("resident"))
        .map(|r| r.jp)
        .collect();
    for (address, sites) in &jp {
        if sites.iter().any(|(f, _)| f.starts_with("jp/")) && !resident.contains(address) {
            bad.push(format!(
                "patch claim {address:08x} ({sites:?}) is not a JP resident function start"
            ));
        }
    }
    for ((module, address), sites) in scan::jp_overlay_claims() {
        let region = format!("ov{module}");
        if sites.len() > 1 {
            bad.push(format!(
                "overlay {module} claims {address:08x} {} times",
                sites.len()
            ));
        }
        if !rows.iter().any(|r| r.region == region && r.jp == address) {
            bad.push(format!(
                "overlay {module} claim {address:08x} is not a function start of {region}"
            ));
        }
    }
    for address in scan::patch_table("TAKES_OVER") {
        let sites = jp.get(&address).cloned().unwrap_or_default();
        if generated(&sites) > 0 || sites.is_empty() {
            bad.push(format!("TAKES_OVER {address:08x}: claims {sites:?}"));
        }
    }
    println!("owners: {} resident addresses claimed", jp.len());
    assert!(bad.is_empty(), "{bad:?}");
}

/// Host-call IDs stay canonical PAL SDK addresses: every `host_call` /
/// `host_call_result` first argument of the JP crate is one of the PAL
/// crate's in the same file, with the same value (JP and PAL SDK addresses
/// collide, so a re-addressed ID would name another routine).
#[test]
fn host_call_ids_stay_canonical() {
    let pal = scan::host_ids(&scan::pal_crate_dir());
    let jp = scan::host_ids(&scan::crate_dir());
    let mut bad = Vec::new();
    let mut n = 0;
    // the patch layer's own files: every ID it names is one of the PAL crate's
    let all: Vec<u32> = pal.values().flatten().filter_map(|id| id.1).collect();
    for (file, ids) in &jp {
        let Some(pal_ids) = pal.get(file) else {
            for id in ids {
                n += 1;
                if id.1.is_some_and(|v| !all.contains(&v)) {
                    bad.push(format!(
                        "{file}: {} = {:08x?} is not a PAL host-call ID",
                        id.0, id.1
                    ));
                }
            }
            continue;
        };
        for id in ids {
            n += 1;
            if !pal_ids.contains(id) {
                bad.push(format!("{file}: {} = {:08x?}", id.0, id.1));
            }
        }
    }
    println!("host-call IDs: {n} first arguments checked");
    assert!(bad.is_empty(), "re-addressed host-call IDs: {bad:?}");
    assert!(n > 100, "the scan found the host calls ({n})");
}

/// No PAL-only routine address is left in code, except as the argument of
/// an explicit `jp_review::unmapped` fault or where it is a JP function too.
#[test]
#[ignore = "requires AC3_JP_DIR"]
fn no_pal_only_address_remains() {
    let dir = scan::jp_dir();
    let jp_starts: Vec<u32> = scan::jp_map(&dir).iter().map(|r| r.jp).collect();
    let pal_only = scan::pal_only(&dir);
    let mut left = Vec::new();
    for (name, text) in scan::sources(&scan::crate_dir()) {
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("jp_review::unmapped(") {
                continue;
            }
            for word in code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
                let Some(digits) = word.strip_prefix("0x") else {
                    continue;
                };
                let digits: String = digits
                    .chars()
                    .take_while(|c| c.is_ascii_hexdigit() || *c == '_')
                    .collect();
                let Ok(v) = u32::from_str_radix(&digits.replace('_', ""), 16) else {
                    continue;
                };
                if pal_only.contains(&v) && !jp_starts.contains(&v) {
                    left.push(format!("{name}:{}: {v:08x}", n + 1));
                }
            }
        }
    }
    assert!(left.is_empty(), "{left:?}");
    assert!(!pal_only.is_empty());
}
