# The UI

What every click and hold does. This is the spec. `src/ui.rs` implements the clicks, `src/limits.rs` the low battery, the heat and what a failed flash, fuse or watchdog check does, `src/watchdog.rs` the proof and what each reset means, and `src/torch.rs` what a failed clock check does, and their tests check it. A failed register or RAM test is `src/rt.rs`'s, and the simulator checks that, so change this file in the same commit as the code.

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

These aren't clicks, they're what the light does on its own. The Action column says what's going on, and the examples set the scene with words `cargo xtask simulate` understands: `cell:3.05V` for the cell (it starts at 3.8 V), `temp:70C` for the MCU (it starts at 25 °C), `adc:0` for an ADC that reads 0 every time, `osc:40%` or `osc:dead` for the 32 kHz oscillator, `clk:400%` for the main clock and `tca:stopped` for a TCA0 that's stopped counting.

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
| Garbage readings, off | Nothing. The light never lights on its own | `adc:0 1H:3s wait:1s` → off | Done |
| 32 kHz oscillator stops, on | Off within two ticks, and it stays off until the cell comes out: clicks do nothing | `1C wait:1s osc:dead wait:1s 1C` → off | Done |
| 32 kHz oscillator dead from power-on | Off for good, as above | `osc:dead 1C wait:2s` → off | Done |
| Main clock badly wrong, on | Off within about a second, for good | `1C wait:1s clk:400% wait:2s 1C` → off | Done |
| 32 kHz oscillator badly wrong from power-on | Off for good, before a click can turn it on | `osc:200% 1C wait:2s` → off | Done |
| TCA0 stopped, on | Off within about a second, for good | `1C wait:1s tca:stopped wait:2s 1C` → off | Done |
| TCA0 stopped, then 1C | Off as usual, and for good at the next count while it's on, so a light turned on with TCA0 already stopped can stay lit for up to a second before the count turns it off | `1C wait:1s tca:stopped 1C wait:3s 1C wait:2s 1C wait:1s` → off | Done |
| 32 kHz oscillator stops during a count | Off for good, as above | `1C wait:576ms osc:dead wait:3s 1C` → off | Done |
| TCA0 and the 32 kHz oscillator both stop during a count | Off for good, as above | `1C wait:576ms osc:dead tca:stopped wait:3s 1C` → off | Done |
| A clock badly wrong only while the torch rests | Nothing, since no count starts at rest. Only a second go for a low count taken while it was on can finish there | `wait:1s osc:40% wait:1s osc:100% wait:1s 1C wait:1s` → level 50 | Done |
| Clocks at the edge of their spread | Nothing. The 32 kHz oscillator alone can be 30% out on a good chip | `osc:130% 1C wait:2s` → level 50 | Done |

The clock check adds up the clocks of the PWM cycles that fit between two 64 Hz ticks, which is the main clock against the 32 kHz oscillator. It runs once at power-on and once a second while the light's on, and a tick that doesn't come at all is caught sooner, by a timer on the main clock that gives up on it after about 26 ms. Anything from 39,525 to 130,560 clocks passes, less one PWM cycle at the bottom, against 78,125 on a perfect chip (`selftest::ClockCheck` has the sums). That's wide, since a false trip turns a working torch off for good, so it only catches a clock out by about a half or more. For the same reason a count that comes in low gets a second go on the next tick, which is why those rows say about a second: it can be a tick or two over. Two low counts in a row fail, and so does low, good, low, so a clock that's only wrong some of the time doesn't get away with it. A count that's too high, or a tick that never comes, fails at once. A light that fails it is off, and it looks like a torch that's off with the button LED lit, except that nothing turns it on until the cell's been taken out and put back.

The cell's only read with the light on or the switch in use, since the ADC can't run while the torch [sleeps](#always-on-in-the-background). After five minutes off it forgets the last readings, so a cell that ran down in a drawer is judged on what it reads now, while clicking off and on can't buy a sagging cell more time. It also means a battery ceiling only climbs while the cell's being read: after stepping down, the light can come back dimmer than it should, then fade up.

The thresholds and the 0614 voltage calibration are a starting point, not measured on this firmware yet; see [hardware.md](hardware.md#the-driver).

## Self-tests at power-on

Before it turns anything on, the firmware checks flash against the CRC that `cargo xtask hex` stores in its last two bytes, and reads its fuses back (`src/selftest.rs` has which fields and why). It never writes them. A failure doesn't leave a dead torch: the light quietly refuses the FET until the cell comes out, and does everything else as usual, low-voltage protection included. The clock check runs at power-on too, but it keeps running and a failure turns the light off, so it's [above](#low-battery-and-heat).

Before any of that, before memory's even set up, it tests r0 to r31, SREG and the stack pointer on every reset, and all of RAM on a power-on or UPDI reset and after a hang (`src/rt.rs` has how, after AN2632 §3.1, §5 and §9.2). The watchdog proof's own resets skip the RAM test, and so do brown-out, software and external resets, which have nothing to say about RAM. Those failures are different. The Rust code that caps the light can't be trusted on a chip with broken RAM or registers, so the firmware stops there for good, with the FET and linear pins held low. Nothing responds, and the button LED stays dark too, which is how to tell it from a torch that's only off. It's most likely a damaged chip.

RAM gets tested again while the torch is awake, 16 bytes a tick, so a fault within one 16-byte section that turns up with the cell in is found within about 4 seconds of the light being on. A fault between bytes further apart than that isn't seen until the next time the whole of RAM is tested. It doesn't run while the torch rests in power-down, so a fault that turns up then waits until the light's next on. A failure puts the light out at once and stops feeding the watchdog, which restarts the chip within about 65 ms, or about 8 s after a failed proof. That's a hang, so all of RAM gets tested again. If the test finds the fault, it stops as above, for good. If the fault's gone, the light comes back off and refuses the FET until the cell comes out, as after any hang (see below). A fault can't arrive part-way through a script, so there's no row for this below; `periodic_faults` in `xtask/src/sim.rs` tests it.

Taking the cell out and putting it back runs the tests again. The library never runs on a chip like that, so `cargo test` only checks these rows' scripts parse and play dead; the real test is `cargo xtask hex`, which runs them on the simulated chip and also sweeps every stuck register bit, every short in a register, and a spread of RAM faults (`startup_faults`, `register_shorts` and `ram_coupling` in `xtask/src/sim.rs`).

After a power-on or a flash, and before any of the checks above but the register and RAM tests, it proves its watchdog works, by letting it reset the chip on purpose (`src/watchdog.rs` has the steps, after AN2632 §6). A watchdog that fails the proof is treated like a bad fuse, with one difference: a watchdog that bites early would reset the light every time on the usual settings, so it's left on an 8 s timeout instead, with no window. One that ignores its feeds altogether still times out, so that light turns itself off every 8 s or so. As with flash and fuses, the cap is nothing yet at level 50, so these examples look the same with or without it; it shows once the full UI's back. The torch can't respond until the proof's over, about 0.3 s by the sums, not measured, but a press in that time isn't lost: it counts as a click once the proof's done. The light stays off throughout.

A hang is what the proof is for. If the firmware stops feeding the watchdog, the chip resets, and the light comes back off and refuses the FET until the cell comes out, even if whatever hung never happens again.

The examples break a byte of flash with `flash:corrupt`, and set a fuse byte with `fuse:OSCCFG=0x01`, using the datasheet's names. `ram:stuck:0x3800.3=1` sticks bit 3 of 0x3800 high, `ram:alias:0x3900=0x3800` sends 0x3900 to 0x3800's byte, `ram:bridge:0x3C00.0.1` shorts two bits of a byte together, `ram:couple:0x3900.7=1:0x3800.0` has bit 7 of 0x3900 going high flip bit 0 of 0x3800 (a trailing `=0` or `=1` forces it instead), `reg:r24.0=1` sticks bit 0 of r24 high, and `reg:short:r24.0.2=0` shorts bits 0 and 2 of r24, with 0 winning. `wdt:never` is a watchdog that never resets the chip, `wdt:early` one that bites in under half the time, `wdt:deaf` one that ignores `wdr`, `wdt:stops` one that never resets the chip after a `wdr`, `wdt:open` a closed window that lets a feed through, `wdt:shut` a window that never opens, and `wdt:wipes` a reset that SRAM doesn't survive. `proof:press` is a press that comes and goes during the proof. A script's faults are there from power-on, so they go before anything that takes time. `hang` can go anywhere, and stops the firmware there until the watchdog resets it.

| Action | Does | Example | Status |
|---|---|---|---|
| Flash doesn't match its CRC, off | Nothing. Most likely a flash that didn't finish, or an image that didn't come from `cargo xtask hex` or `flash` | `flash:corrupt wait:3s` → off | Done |
| Flash doesn't match its CRC, on | Never above level 75, the top of the linear channel. At level 50 that's nothing yet; it shows once the full UI's back | `flash:corrupt 1C wait:2s` → level 50 | Done |
| Fuses not what the firmware expects, off | Nothing | `fuse:OSCCFG=0x01 wait:3s` → off | Done |
| Fuses not what the firmware expects, on | Never above level 75, as for flash | `fuse:OSCCFG=0x01 1C wait:2s` → level 50 | Done |
| Watchdog never resets the chip, off | Nothing | `wdt:never wait:3s` → off | Done |
| Watchdog never resets the chip, on | Never above level 75, as for flash | `wdt:never 1C wait:2s` → level 50 | Done |
| Watchdog bites in under half its time | Never above level 75, as for flash | `wdt:early 1C wait:2s` → level 50 | Done |
| Watchdog's closed window lets a feed through | Never above level 75, as for flash | `wdt:open 1C wait:2s` → level 50 | Done |
| The firmware hangs, on | The watchdog resets the chip, and the light comes back off | `1C hang wait:1s` → off | Done |
| After a hang | Works as usual, but never above level 75 until the cell comes out | `1C hang wait:1s 1C` → level 50 | Done |
| A press during the watchdog's proof | Counts as a click once the proof's over | `proof:press wait:1s` → level 50 | Done |
| A bit of RAM stuck | Nothing, ever, clicks included, until the cell comes out | `ram:stuck:0x3800.3=1 1C wait:2s` → off | Done |
| Two RAM addresses reaching the same byte | Nothing, ever, as for a stuck bit | `ram:alias:0x3900=0x3800 1C wait:2s` → off | Done |
| Two bits of a RAM byte shorted together | Nothing, ever. A test that only wrote 0x00 and 0xFF couldn't find this one | `ram:bridge:0x3C00.0.1 1C wait:2s` → off | Done |
| A store to one RAM byte disturbing another | Nothing, ever. With the culprit above its victim, only the test's descending pass can find this one | `ram:couple:0x3900.7=1:0x3800.0 1C wait:2s` → off | Done |
| A bit of a CPU register stuck | Nothing, ever, as for RAM | `reg:r24.0=1 1C wait:2s` → off | Done |
| Two bits of a CPU register shorted together | Nothing, ever. Bits an even distance apart need the test's patterns after 0x55 and 0xAA | `reg:short:r24.0.2=0 1C wait:2s` → off | Done |

## Always on, in the background

| What | Does | Status |
|---|---|---|
| Button LED | The amber LED under the switch glows dimly whenever there's a cell in, on or off, so you can find the torch in the dark. It's lit by the pin's pull-up, so it costs microamps. The blue one is the charger's | Done |
| Sleep when off | Powers down between ticks and looks at the switch when it wakes, so the light can sit in a drawer for a year | Done |

It powers down whenever the light's off and no click is being counted, and wakes 64 times a second to look at the switch and feed the watchdog. That should cost the chip around 10 µA, but it's sums from the datasheet and the simulator, not measured on a light. The button LED takes more than that, 50 to 64 µA going by the [teardown's measurements](hardware.md#the-button-led), so with it the torch should use about 600 mAh a year, before whatever the rest of the driver draws. See the [roadmap](roadmap.md).
