# The UI

What every click and hold does. This is the spec. `src/ui.rs` implements it and its tests check it, so change this file in the same commit as the code.

The notation is Anduril's. **1C** is one click, **1H** press and hold, and so on. Clicks count once the switch has been left alone for about 280 ms.

`cargo xtask simulate` runs this UI in a terminal, live or from a script in the same notation (`cargo xtask simulate 1C 2H:1.5s 3C`).

Every row has an example: a script in that notation, starting from a torch that's just had its battery put in, and how the light should be after it (`off`, `on` or `level N`). `cargo test` runs the examples against the library, and `cargo xtask size` runs them against the built firmware on a simulated chip, so a row without one, or one that's wrong, fails both. Only Planned rows can leave it as `-`.

For the first flash the UI is as small as it gets, so that when something doesn't work it's the hardware or the toolchain, not the UI. The full UI (ramping, turbo, lockout, moonlight) is on the `fancy-ui` branch and comes back once this has been checked on a real torch.

## Off

| Action | Does | Example | Status |
|---|---|---|---|
| 1C | On | `1C` → level 50 | Done |
| Anything else | Nothing | `2C 3C 1H 2H:2s wait:1s` → off | Done |

## On

| Action | Does | Example | Status |
|---|---|---|---|
| 1C | Off | `1C 1C` → off | Done |
| Anything else | Nothing | `1C 2C 3C 1H 2H:2s wait:1s` → level 50 | Done |

It always comes on at level 50 of 150, which is on the linear channel. The FET never turns on.

## Always on, in the background

| What | Does | Status |
|---|---|---|
| Thermal regulation | Steps the level down when the MCU's temperature sensor says the head's too hot, and back up as it cools | Planned |
| Low-voltage protection | Steps down as the cell runs low, then turns off before it's damaged | Planned |
| Sleep when off | Powers down and wakes on the switch, so the light can sit in a drawer for a year | Planned |

Until sleep, thermal regulation and low-voltage protection are in, this isn't firmware to carry. See the [roadmap](roadmap.md).
