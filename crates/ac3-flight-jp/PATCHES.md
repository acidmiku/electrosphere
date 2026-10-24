# The JP patch layer

`crates/ac3-flight-jp` is generated from the PAL crate by `tools/jp/readdress.py` (REVIEW.md, JP_PORT_PLAN §7a).

**Never edit the generated files.** They are every file of the crate except the following:
- `src/jp/`
- this file

The hand-written JP code lives in `src/jp/`, the **patch layer**:
- The generator never writes it. It writes the skeleton once, when `src/jp/mod.rs` is missing.
- It reads the patch layer on every run and wires it in.
- So a PAL fix reaches JP by regenerating, and a JP patch survives the regeneration.

## Layout

| File | Contents |
| --- | --- |
| `src/jp/mod.rs` | `pub mod` of every patch module, and the two tables `REPLACES` and `TAKES_OVER` |
| `src/jp/<module>.rs` | Replacements of functions of the generated `src/<module>.rs`: same name, same signature |
| `src/jp/res_pN_<module>[_<subsystem>].rs` | The same, written by a wave part. A part may split a module by subsystem when the generated module's private names would collide in one file. A `REPLACES` function must be defined in exactly one of these files and `src/jp/<module>.rs`. A module with replaced functions gets `#[allow(dead_code, unused_imports)]` on its `pub mod` line in `lib.rs`, for the helpers only the replaced bodies used |
| `src/jp/ovN_<part>[_<subsystem>].rs` | Overlay N routines of a wave part, with their own `dispatch` gated by `loaded_module == N` |
| `src/jp/resident.rs` | JP-only resident routines (class `new`, REVIEW.md §6) and their `dispatch` |
| `src/jp/ov1.rs` .. `ov4.rs` | The JP overlay modules (see below); a large one may become `src/jp/ov2/mod.rs` with submodules |

Wiring:
- Every top-level `pub fn dispatch(m: &mut Machine, target: u32, args: [u32; 4]) -> Option<u32>` of a patch file is added to `dispatch::OWNERS`, first.
- A target that no owner claims goes to the development interpreter fallback: feature `dev-fallback`, `crate::fallback::miss`, the `ac3-dev` binary.
- Without the feature, an unclaimed target raises `Fault::Untranslated`.
- The fallback's reach report is the translation queue.

## Patching a near or changed routine (same signature)

1. **Find it in REVIEW.md** §2, §3 or §3b. The entry gives:
   - the JP and PAL addresses;
   - the diff kinds;
   - the generated Rust function, marked `// JP-REVIEW(Rn)` in `src/<module>.rs`.
2. **Compare the instructions.** For example, with the decoder in `tools/jp/mips.py` (`mips.disasm`), PAL from `extracted/programs/ACE.unpacked.bin` and JP from `~/ac3-jp/extracted/disc1/disc/ACE3.BIN`, or with `~/ac3-jp/tools/showdiff.py`.
   - The generated function already uses the JP addresses, gp offsets and data. Only the instructions the map reports as different need a change.
3. **Copy the generated function** into `src/jp/<module>.rs` as a `pub fn`, with the same name and signature, and edit it.
   - Call the generated module's public items through `crate::<module>::...`.
   - Copy private helpers you need.
   - Doc comment: `/// <JP address> (JP; PAL <address>, near, <kinds>): ...`
4. **Register it.**
   - Add `(<JP address>, "<module>::<fn>")` to `REPLACES`. Use `"<module>::<inline module>::<fn>"` for a function inside an inline module; the patch still lives in `src/jp/<module>.rs`.
   - Add the module to `src/jp/mod.rs`.
5. **Regenerate:** `python3 tools/jp/readdress.py`.
   - The generated function's body becomes a call of the patch (`// JP-PATCH` above it), so its dispatch arm and every native caller reach the patch.
   - REVIEW.md marks the routine **patched**.
   - `--check` fails if the crate is stale.
6. **Validate** (`AC3_JP_DIR`, default `~/ac3-jp`):
   - `cargo test --release -p ac3-flight-jp --test captures -- --include-ignored --nocapture` replays every JP capture. A patched routine must reproduce **all** of its captured calls, or the test fails.
   - `cargo test --release -p ac3-flight-jp --test differential -- --include-ignored --nocapture` runs the routine against the JP original (the oracle) on the JP RAM images with random arguments and fields. The patched routines are listed apart.
   - `cargo test --release -p ac3-flight-jp --test claims -- --include-ignored` checks one owner per address and that host-call IDs stay canonical.
   - fmt and strict clippy, as for the rest of the workspace.

The checked-in example is `src/jp/init_39a68.rs`, `reset_gp_344`:
- JP 800409AC, near, `imm:1`: 0x80 becomes 0x78, the screen centre of the 240-line display.
- The generated translation misses the JP capture `flight-002` #77 by that one byte; the patch reproduces it.

A single captured call, as in the PAL tests:

```rust
let index = scan::jp_dir().join("analysis/redux/flight-002/snapshots-module0.jsonl");
let changed = ac3_flight_jp::snapshot::check_call(&index, 0x8004_09ac, 0, |m, _| {
    ac3_flight_jp::jp::init_39a68::reset_gp_344(m);
    None
})
.unwrap();
assert!(changed > 0);
```

## A routine whose signature changes

1. Write the new function in a patch module.
2. Claim its JP address in that module's `dispatch`: for example, `0x8004_1120 => Some(packets(m, a0, a1))`.
3. Add the address to `TAKES_OVER`. The generator removes the generated dispatch arm, and the claim moves to the patch layer.
4. Patch the native callers of the old generated function too. They are near or changed routines themselves.

## JP-only resident routines

Translate the routine from the JP instructions into `src/jp/resident.rs` and claim it in its `dispatch`:
- Examples: Krom2RawAdd, LoadImage2, the JP card code; REVIEW.md §6 lists them all.
- An address must be a JP function start (`jp_pal_map.tsv`), claimed once.

## Overlay modules

`src/jp/ovN.rs` is overlay N, loaded whole at `OVERLAY_BASE` = 0x800CBFE8. It holds:
- `MODULE`, `BASE`, `BYTES`, `ARENA` and `CALLBACKS` (the state table record, JP_PORT_PLAN §2.1);
- a `dispatch` that answers only while `common::loaded_module(m) == MODULE`, because the overlays share their addresses.

Rules:
- Claims count per overlay. An address must be a function start of that overlay in `jp_pal_map.tsv` (region `ovN`).
- Overlay routines are reached mostly through data tables, so follow the fallback's reach counter.
- **Captures:**
  - Overlay 1 calls are in `~/ac3-jp/analysis/redux/{boot,save}-002/snapshots-module1.jsonl`.
  - Overlay 2 calls are in `{flight,save,story}-002/snapshots-module2.jsonl`.
  - `tests/captures.rs` replays the ones the patch layer claims.
- `tests/fallback_smoke.rs` (`--features dev-fallback`) runs overlay 1 from the JP probe RAM through the state dispatcher. Drive the dispatcher (`mainloop::dispatch_state`), not a callback: a state's substate-0 callback initialises once and returns NEXT_SUBSTATE.

## Rules

- **Host-call IDs stay canonical PAL SDK addresses.** The first argument of `host_call`, use the generated `bios`/`sdk`/`boot` consts. JP and PAL SDK addresses collide (JP `_card_format` 800AD808 is PAL `open`). `tests/claims.rs` checks every ID the patch layer names.
- **Everything else is a JP address.**
- **One owner per address.** The generator and `tests/claims.rs` fail on a second claim.
- **The FLIGHT_MACHINE.md translation rules apply:**
  - translate from the instructions, with exact integer semantics;
  - no captured state in runtime code;
  - validate every routine against the JP original.
- **Never write a PAL address.** A site no map covers is in REVIEW.md §1 with a run-time fault, and is fixed in the patch layer like any other routine.

## Generator errors

Each of these fails generation and `--check`. `tools/jp/test_patches.py` exercises them all.

- REPLACES names a function that no longer exists.
- REPLACES names a function that is not the routine at its address (neither its doc address nor a dispatch arm).
- REPLACES names a function whose signature changed.
- REPLACES names a function that has no patch function.
- A patch claim that the generated crate also has, when the address is not in TAKES_OVER.
- A TAKES_OVER without its patch claim, or without a generated arm.
- An address claimed twice in the patch layer.
- A claim that is not a JP function start of its region.
