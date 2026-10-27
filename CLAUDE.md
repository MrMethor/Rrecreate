# Rrecreate

A toolkit an AI agent uses to port DOS games to modern systems: the original runs in our deterministic
emulator, Clockwork, and a native port is compared with it tick by tick (`DESIGN.md` §1, §2).
**Status:** Clockwork is in `clockwork/`, driven by tools through `clockwork serve` (M0, M1); the toolkit, `rrc`,
is in `rrc/` (M2). The runtime and the comparison are M3 (`DESIGN.md` §12).

- **Porting a game:** follow `docs/porting.md`, inside the game's port repository (`rrc status` names the next
  step).
- **Working on Rrecreate itself:** the notes below.

## Notes for agents working on Rrecreate

- `DESIGN.md` is the design, `docs/decisions.md` the decisions and their reasons. Read the sections you need.
  When a decision changes: mark the old one `superseded by Dn`, add the new one, and update `DESIGN.md` in the
  same commit. Never leave text that describes a superseded decision.
- Tests: `cargo test --workspace`. Lint: `cargo clippy --workspace --all-targets -- -D warnings`. Format:
  `cargo fmt --all` (`rustfmt.toml`). CI runs all three on Windows and Linux.
- To check Clockwork on a real program: `cargo build --release -p clockwork`, then
  `target/release/clockwork run <dir> <program> --json`. When a program misbehaves, use `--hotspots 10` (where it
  loops), `--trace FILE` (which service call went wrong), `--disasm SEG:OFF` and `--dump SEG:OFF:LEN` before
  guessing (`clockwork/README.md`). After changing Clockwork, the test games' walkthroughs should still reach their
  screenshots (`clockwork/README.md`, "The test games").
- After changing the CPU, rerun the SingleStepTests suites (`clockwork/README.md`, "Validating the CPU"); its
  known differences are listed there, anything else failing is a regression.
- Tests that need DOS code: `clockwork/dos/tests/programs.rs` assembles small programs with iced-x86; 16-bit code
  can't use iced's label memory operands, so use the two-pass `Asm` helper there (`a.here("name")`,
  `a.addr("name")`, `a.l("name")`). CPU tests in `clockwork/cpu/tests/` hand-encode instructions (protected-mode
  tests in `protected.rs` assemble with iced-x86; FPU tests in `fpu.rs` use iced's `fsub_2`, `fdivr_2`... for
  two-operand register forms); shared helpers are in `tests/common/mod.rs`. `testbins/` has C programs built with
  Open Watcom (`testbins/README.md`); the DOS/4GW tests need `WATCOM` set (they skip without it).
- `clockwork/dos/src/fonts.rs` and `vga_params.rs` are generated from DOSBox Staging's sources by the scripts in
  `clockwork/tools/`; don't edit them by hand.
- Licences: everything is MIT except `clockwork/`, which is GPL-2.0-or-later. Code from DOSBox or other GPL
  sources goes into `clockwork/` only. `rrc` and `runtime/` never depend on Clockwork's crates; they use its
  control protocol.
- Clockwork stays deterministic: never read the host clock or use randomness; time comes from executed
  instructions. Floating point must give the same bits on every host (`libm`, not the `f64` methods; no NaN bits
  produced by the host CPU in machine state). Runs are compared by state hash, which covers the machine's state
  only: records (sound logs, frames, console; `dos/src/record.rs`) and the analysis tools (`Machine::probes`)
  aren't state, and the analysis tools never change what the machine does.
- Before adding a Clockwork feature, look at how DOSBox Staging, DOSBox-X, 86Box, PCem, MAME, Spice86 and MartyPC
  do it, and port or adapt the best. Start the module's docs with "Ported from <emulator> `<path>` (<licence>)",
  add a row to `THIRD_PARTY.md`, and add tests of our own. Only GPL-2.0-compatible sources may be copied. Their
  sources are cloned under `reference/` (git-ignored).
- Games only reveal gaps: implement each as a general feature with its own test, never a game-specific workaround.
  Unsupported things are reported (a stop, a diagnostic), never guessed.
- Game files are never committed. Each game's port lives in its own repository (`DESIGN.md` §3).
- IDs: `<MODULE>[#<n>]+<offset>` (`#n`: an overlay or an LE/LX object; the offset in the loaded image, uppercase
  hex, 5 to 8 digits), for functions and data alike; `MEM+<linear>` for memory outside the images (D53). The
  `ids` crate parses and prints them; its cases are in `testdata/function_ids.tsv`.
- Tool output is read by agents: keep it short. Large data goes to the cache, and the output gives its hash.
  `rrc` prints a label and a value per line.
- `rrc` (MIT) drives Clockwork as a program (`clockwork serve`, `clockwork run`, `clockwork unpack`), never as a
  library. `clockwork serve` speaks the emulator protocol, `docs/protocol.md` (D58): a change to it is a new
  revision (`PROTOCOL` in `serve.rs` and `rrc/src/clockwork.rs`) and goes into that document; keep what is
  DOS's in the `dos-pc` extension. The files it reads and writes are documented in `docs/formats.md`: change the two together, and
  `docs/porting.md` when a step of the workflow changes. `rrc/tests/flow.rs` runs the whole flow on a small
  program with the workspace's debug `clockwork`.
- Recreate, the project Rrecreate began from (`DESIGN.md` §11), is disregarded (D69): don't read its code,
  design or knowledge, or take anything from it. Commit messages and decisions that name it are history.
