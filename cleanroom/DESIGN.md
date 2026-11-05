# Clean-room design

What I'd build for the TS21 if I started again without looking at the firmware in `../src`. It's a prototype on a branch, not a replacement, and it has never run on a torch.

## How clean is it?

Less than the name suggests, so here's the list.

- **Read:** `vendor/`, `docs/hardware.md`, `docs/toolchain.md`, `.cargo/config.toml`, and CLAUDE.md, which is loaded whether I like it or not.
- **Not read while designing:** `../src`, `docs/ui.md`, `docs/roadmap.md`, `../link.x`, or Anduril. I read the first three afterwards, for the comparison at the end of this file. The design above that section didn't change as a result.
- **Leaks:** `docs/hardware.md` isn't only hardware. It told me the existing firmware runs at 5 MHz, uses single-slope PWM with a period of 254, and has cut-offs near 3 V. I ended up with the same clock and the same 3 V, and I can't claim I'd have got there alone. Pins, LED channels and calibration constants trace back to Anduril through that file.
- **After the design froze:** the tests borrow the CPU core in `../xtask/src/avr.rs`, which is a generic AVR with no torch in it. Nothing else from xtask is used, and the chip around it is new.

## The ideas

**The torch is one pure function.** `Torch::tick(Input) -> Output` runs 64 times a second and is the only thing the firmware calls. The input is the button level and maybe an ADC reading. The output is a level, the amber LED and whether to sleep. Everything else, the 300 lines of `board.rs` included, is plumbing, and everything that decides is host-tested with `#![forbid(unsafe_code)]`.

**Interrupts carry no information.** One seven-instruction handler clears the flags, because waking up was the whole message. So no state is shared with an interrupt: no `static mut`, no atomics, no volatile flags. The state is a struct on `main`'s stack. There's no `.data` or `.bss`, the linker script asserts that, and start-up is `clr r1; jmp main`.

**One set-up path.** Boot and wake both run `power_up`, which configures everything from scratch. Reset leaves the pins as inputs and the timer off, so a panic resets the chip and the light fails dark. So does the watchdog.

**Registers come from the ATDF, at build time.** `build.rs` is a 180-line scanner that emits every address, mask and shifted field value. Nothing is typed by hand, so there's nothing to check by hand either. The price is that a typo'd name is a compile error rather than a review comment, which I'll take.

**Compare in sensor units.** Readings are never turned into volts or degrees. Voltage thresholds become ADC counts at compile time, and temperature ones once at boot using the chip's own calibration. The running code has no multiplies or divides for this, and the rule about 32-bit maths stops being something to remember. The voltage readout, the one place that wants volts, searches a const table.

**One reference for both sensors.** Battery and temperature both use the 1.1 V reference. Battery measures it against VCC and temperature measures against it, so `VREF` is written once per wake and nothing needs to settle between channels.

**One governor for heat and battery.** Both are a raw reading that climbs as things get worse. So `Governor` is one type, used twice, holding a ceiling on the level. `min(want, ceilings)` is the only way limits reach the output, and the UI doesn't know they exist. A sagging cell on turbo isn't a special case: the ceiling comes down, the sag eases, and it settles. If the ceiling reaches the floor and the reading is still bad, the cell's flat and the light goes off.

**The ramp is computed.** Levels are a geometric scale of lumens worked out in a `const fn`. Below the linear channel's limit the PWM period stretches from 19.6 kHz down to 1.2 kHz, which buys duty resolution at the dim end. `TABLE` is 768 bytes.

**No EEPROM.** RAM survives power-down, so the memorised level only goes when the cell comes out. That's a whole peripheral, its wear and its failure modes gone, for losing one number at a battery swap.

**Unused pins get their input buffers disabled**, with no pull-ups. Nothing can float into a buffer that isn't there. PA0 is the exception: it's UPDI, so it stays as reset left it. Main disables the buffers too and adds pull-ups, so the only difference is the pull-up, and I've no evidence either way on which is better.

## The UI

The rule I wanted: no gesture ever waits to see if another click is coming. The decoder reports edges as they happen, and clicks build on each other.

| Gesture | Does |
|---|---|
| Press from off | Floor, on the first tick |
| Release of that press | The remembered level |
| Hold | Ramps, up from off, otherwise the other way from last time, dipping at the end stop |
| Release when on | Off |
| 2 clicks | Turbo, or back to the remembered level from turbo |
| 3 clicks | Blinks the voltage |
| 4 clicks | Lockout, or back out |

Floor-then-memory is the part I like. You get light in 16 ms, a hold never starts by blinding you, and a click still lands where you left it. The cost is that a double click from on goes dark for the gap between the clicks. That's a real wart, and it's the price of off having no delay.

## What the tests found

The host models earned their keep. My first governor stepped by whole levels and hunted between levels 87 and 112 against a thermal mass (70 J/K, 8 K/W), a sixfold swing in brightness you'd see. Making recovery proportional cut it to 93 to 107. It only settled when both directions earned fractional credit towards a level. I wouldn't have found that on a bench in an afternoon.

## What's verified

- 55 host tests, including closed-loop thermal and cell models and a fuzz of the rules.
- 8 tests in `tests/on_chip.rs` that run the real hex on the CPU core inside a strict fake chip. It rejects unprotected CCP writes, high-byte-first writes, sleeping with no wake source, power-down with anything left running, and a starved watchdog. Five deliberate breaks to `board.rs` all failed the suite.
- Clippy on host and AVR.

- A review of `board.rs` against the datasheet and errata, done blind to the existing firmware. Nothing fatal. It did find a stale `RESRDY` after a wake that could file a heat reading as a battery one, spurious-wake flags, a watchdog disable that wasn't waited for, and a buffer update that could tear across a period boundary. All fixed.

**Not verified:** anything on silicon. The fake chip is my reading of the datasheet, so it can't catch me misreading it. The thermal and cell models are lumped guesses, and the constants (50 °C, 3.0 V, the floor at 0.2 lm) are unmeasured. The linear regulator almost certainly won't follow a 1.4 µs pulse, so the bottom of the ramp needs tuning on a real light.

**Open risks from the datasheet review:**

- **The shared 1.1 V reference rests on an inference.** The datasheet calls `MUXPOS` INTREF the "internal reference (from VREF peripheral)" and never says its level follows `ADC0REFSEL`. Anduril sets `ADC0REFSEL` to 1.5 V and gets correct volts from INTREF, so it almost certainly does. Likely, not confirmed. The real cost is calibration: the 0614 slope was fitted at 1.5 V and I ignore it, and the two references have their own errors, so blink the voltage against a meter before trusting it.
- **Channel settling isn't specified.** I swap channel and start converting in the same breath. `INITDLY` covers a change to an internal reference, but nothing says it covers a `MUXPOS` change or the switch back to VDD. Main throws away the first conversion after every switch, and I should too.
## Size

| | Flash | RAM | Stack |
|---|---|---|---|
| This | 3,406 B | 0 | 80 B |
| `../src` | 5,312 B | 0 | 110 B |

That's a difference in what each one spends flash on, not in how tight the code is. Main's UI is click on, click off, and its FET never turns on. It spends the rest on self-tests, plausibility checks and thermal prediction. This has ramping, turbo and lockout and almost none of that safety. Main's RAM is 0 too, so the no-statics idea wasn't new.

## Compared with main

Written after reading `../src`, so none of this fed the design above. Keep main. For a first flash its order is the right one: prove the hardware with the FET off, then add the UI.

**Where this design is worse:**

- **It has no clock while off.** I stop the PIT in power-down, so a sleeping torch can't tell ten seconds from a week. A near-flat cell that rests above 3.2 V gets its full ceiling back after any off and on, which is what `FORGET_TICKS` in `battery.rs` exists to stop. The current this saves doesn't matter: the amber LED draws about 50 µA on its own.
- **Broken sensors fail open.** An ADC stuck at 0 reads as a full, cool torch, on an unprotected cell. Main checks readings are plausible and drops to the linear channel.
- **Channel tracking lives in a register.** Using `MUXPOS` as the record of which channel was read caused the stale `RESRDY` bug, in the one file with no host tests. Main's `sensors::Schedule` is in the library.
- **No self-tests.** Main checks the fuses and the flash CRC and refuses the FET on a failure. Nothing here would notice a half-written flash.
- **It trusts `ADC0.CALIB`'s reset value.** The ATDF and the datasheet disagree on it, and main writes it explicitly. Generating registers from the ATDF is safe for addresses and masks, but not for reset values.

**Ideas that might be worth moving to main:**

- Dropping the `PORTA` interrupt, since the PIT already wakes it every tick.
- Generating registers from the ATDF in place of `regs.rs` and the checks in `xtask/src/atdf.rs`.
- Comparing voltage in ADC counts, which would take `centivolts()` out of the running code. Thermal's trend logic does want kelvin, so that one stays.
- Skipping EEPROM, because RAM survives power-down.
- The governor's single rule for FET sag, which needs testing on a real cell.
- Floor-then-memory, for the `fancy-ui` branch.

**Thermal:** `examples/thermal_shootout.rs` runs main's `Thermal` and this `Governor` on the same plants at 45 °C.

| 45 s sensor lag | Peak sensor | Peak head | Over 46 °C | Swing | Worst 1 s jump |
|---|---|---|---|---|---|
| Main | 49.2 °C | 59.1 °C | 146 s | 6.9× | 1.41× |
| This | 50.9 °C | 60.1 °C | 193 s | 3.6× | 1.23× |
| Main, no lookahead | 51.8 °C | 61.9 °C | 815 s | 8.1× | 1.37× |
| Main, lookahead doubled | 47.1 °C | 57.3 °C | 47 s | 2.2× | 1.46× |

Lookahead clearly helps main, and main peaks lower, but this gets close without it and swings about half as much. Doubling the lookahead makes the plants that were steady start to hunt, so the right value depends on the lag. That lag matters more than either controller: at 45 s the head reaches about 60 °C under a 45 °C limit with both, and at 120 s about 73 °C. The plants are guesses, so measure the lag on a real light before tuning anything.

## Running it

```sh
cd cleanroom
cargo test -q   # host tests
./hex.sh        # builds target/ts21-clean.hex, prints size, runs the on-chip tests
```
