# Rrecreate

A toolkit an AI agent uses to port DOS games to modern systems: the original runs in our deterministic
emulator, Clockwork, and a native port is compared with it tick by tick (`DESIGN.md` §1, §2).
**Status:** design only; M0 (`DESIGN.md` §12) is next.

## Notes for agents working on Rrecreate

- `DESIGN.md` is the design, `docs/decisions.md` the decisions and their reasons. Read the sections you need.
  When a decision changes: mark the old one `superseded by Dn`, add the new one, and update `DESIGN.md` in the
  same commit. Never leave text that describes a superseded decision.
- Licences: everything is MIT except `clockwork/`, which is GPL-2.0-or-later. Code from DOSBox or other GPL
  sources goes into `clockwork/` only. `rrc` and `runtime/` never depend on Clockwork's crates; they use its
  control protocol.
- Clockwork stays deterministic: never read the host clock or use randomness; time comes from executed
  instructions. Floating point must give the same bits on every host (`libm`, not the `f64` methods; no NaN bits
  produced by the host CPU in machine state). Runs are compared by state hash.
- Before adding a Clockwork feature, look at how DOSBox Staging, DOSBox-X, 86Box, PCem, MAME, Spice86 and MartyPC
  do it, and port or adapt the best. Start the module's docs with "Ported from <emulator> `<path>` (<licence>)",
  add a row to `THIRD_PARTY.md`, and add tests of our own. Only GPL-2.0-compatible sources may be copied. Their
  sources are cloned under `reference/` (git-ignored).
- Games only reveal gaps: implement each as a general feature with its own test, never a game-specific workaround.
  Unsupported things are reported (a stop, a diagnostic), never guessed.
- Game files are never committed. Each game's port lives in its own repository (`DESIGN.md` §3).
- IDs: `<MODULE>[#<n>]+<offset>` (`#n`: an overlay or an LE/LX object; the offset in the loaded image, uppercase
  hex, 5 to 8 digits), for functions and data alike.
- Tool output is read by agents: keep it short. Large data goes to the cache, and the output gives its hash.
- The predecessor, Recreate (`../Recreated`, github.com/MrMethor/Recreate), is a source of code as listed in
  `DESIGN.md` §11, not of design: its `PROJECT.md` describes the approach Rrecreate replaced.
