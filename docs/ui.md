# The UI

What every click and hold does. This is the spec. `src/ui.rs` implements the clicks and `src/limits.rs` the low battery, the heat and what a failed self-test does, and their tests check it, so change this file in the same commit as the code.

The notation is Anduril's. **1C** is one click, **1H** press and hold, and so on. Clicks count once the switch has been left alone for about 280 ms.

`cargo xtask simulate` runs this UI in a terminal, live or from a script in the same notation (`cargo xtask simulate 1C 2H:1.5s 3C`).

Every row has an example: a script in that notation, starting from a torch that's just had its battery put in, and how the light should be after it (`off`, `on` or `level N`). `cargo test` runs the examples against the library, and `cargo xtask hex` runs them against the built firmware on a simulated chip, so a row without one, or one that's wrong, fails both. Only Planned rows can leave it as `-`. The background table at the end is the exception, since it isn't clicks.

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

## Low battery and heat

These aren't clicks, they're what the light does on its own. The Action column says what's going on, and the examples set the scene with words `cargo xtask simulate` understands: `cell:3.05V` for the cell (it starts at 3.8 V), `temp:70C` for the MCU (it starts at 25 °C) and `adc:0` for an ADC that reads 0 every time.

The light runs at the lowest of the UI's level, the thermal ceiling, the battery ceiling and the self-test cap below, and both ceilings fade a level at a time rather than jump. Readings that can't be real (a voltage outside 2.00 to 4.60 V, a temperature outside -40 to 150 °C) are never taken as safe: a second of them drops that ceiling to level 75, the top of the linear channel, and never turns the light off.

| Action | Does | Example | Status |
|---|---|---|---|
| Cell under 3.10 V for 5 s, on | Fades down 15 levels, a level a tick, and again every 5 s | `cell:3.05V 1C wait:6s` → level 35 | Done |
| Cell still low at level 1 | Off | `cell:3.05V 1C wait:30s` → off | Done |
| Cell under 2.99 V for 1 s, on the linear channel | Off. On the FET it fades to level 75 first, and turns off a second later if it's still low | `cell:2.9V 1C wait:2s` → off | Done |
| 1C with a flat cell | On as usual, then off again a second later | `cell:2.9V 1C wait:2s 1C` → level 50 | Done |
| Cell back at 3.30 V for 4 s | The battery ceiling climbs 15 levels, and again every 4 s | `cell:3.05V 1C wait:6s cell:3.4V wait:10s` → level 50 | Done |
| MCU over 45 °C, on | Fades down towards level 75, never below it, and back up as it cools. At level 50 that's nothing yet, since 50 is under the floor; it shows once the full UI's back | `temp:70C 1C wait:60s` → level 50 | Done |
| Garbage readings, on | Ceiling at level 75, so nothing changes at level 50 | `adc:0 1C wait:10s` → level 50 | Done |
| Garbage readings, off | Nothing. The light never lights on its own | `adc:0 wait:3s` → off | Done |

The thresholds and the 0614 voltage calibration are a starting point, not measured on this firmware yet; see [hardware.md](hardware.md#the-driver).

## Self-tests at power-on

Before it turns anything on, the firmware checks flash against the CRC that `cargo xtask hex` stores in its last two bytes, and reads its fuses back (`src/selftest.rs` has which fields and why). It never writes them. A failure doesn't leave a dead torch: the light quietly refuses the FET until the cell comes out, and does everything else as usual, low-voltage protection included.

The examples break a byte of flash with `flash:corrupt`, and set a fuse byte with `fuse:OSCCFG=0x01`, using the datasheet's names. The firmware only checks them at power-on, so they go before anything that takes time.

| Action | Does | Example | Status |
|---|---|---|---|
| Flash doesn't match its CRC, off | Nothing. Most likely a flash that didn't finish, or an image that didn't come from `cargo xtask hex` or `flash` | `flash:corrupt wait:3s` → off | Done |
| Flash doesn't match its CRC, on | Never above level 75, the top of the linear channel. At level 50 that's nothing yet; it shows once the full UI's back | `flash:corrupt 1C wait:2s` → level 50 | Done |
| Fuses not what the firmware expects, off | Nothing | `fuse:OSCCFG=0x01 wait:3s` → off | Done |
| Fuses not what the firmware expects, on | Never above level 75, as for flash | `fuse:OSCCFG=0x01 1C wait:2s` → level 50 | Done |

## Always on, in the background

| What | Does | Status |
|---|---|---|
| Button LED | The amber LED under the switch glows dimly whenever there's a cell in, on or off, so you can find the torch in the dark. It's lit by the pin's pull-up, so it costs microamps. The blue one is the charger's | Done |
| Sleep when off | Powers down and wakes on the switch, so the light can sit in a drawer for a year | Planned |

Until it powers down when off, this isn't firmware to carry. It idle-sleeps between ticks, on or off, which the datasheet puts at 1.1 mA typical at 10 MHz and 3 V against 3.1 mA awake (DS40002204A table 36-4), before the timer, the ADC's reference and the rest of the driver, so it still drains the cell in months. See the [roadmap](roadmap.md).
