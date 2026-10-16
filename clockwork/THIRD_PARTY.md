# Third-party code and data

Recreate is GPL-2.0-or-later (see `LICENSE`). This file lists every piece of code or data that
came from another project, so its origin and license stay traceable. Ported files also say so in
their module documentation.

Reference checkout: DOSBox Staging, commit `0fdf977d9c4e41908700d7eb2255142044f881cd`, in
`reference/dosbox-staging` (git-ignored; see CLAUDE.md for how to fetch it).

| Recreate file | Origin | License | What |
|---|---|---|---|
| `host/dos/src/mouse.rs` | DOSBox Staging `src/hardware/input/mouseif_dos_driver.cpp` | GPL-2.0-or-later | Behavior reference for ranges, granularity and reset values; the list of ignored vendor INT 33h functions |
| `COPYING` | GNU GPL version 2 text (via DOSBox Staging `licenses/GPL-2.0.txt`) | — | License text |
