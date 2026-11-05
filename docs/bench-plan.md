# Bench plan

Every hardware check the roadmap is waiting on, in one place and in the order to do them, for a bench with the torch, a Fluke 107, [JLHawaii808's 4.5 V UPDI kit](https://jlhawaii808.com/products/updi-programmer-driver-reflashling-kit) for the torch, and the [spare chip kit](#the-spare-chip-kit): an Adafruit ATtiny1616 breakout, an Adafruit High Voltage UPDI Friend, a breadboard, jumpers, tactile switches and blue LEDs. Nothing here has run on a real light yet, so treat each expected result as a prediction. [flashing.md](flashing.md) has how to reach the pads and use the programmer.

Each check says what it needs:

- **Now**: the firmware on main can do it.
- **T1**: needs a test build at another level (see [test builds](#test-builds)).
- **T2**: needs a test build that blinks a number out.

Checks this kit can't do at all are at [the end](#what-this-kit-cant-do), with what would unlock them.

## What the kit can and can't see

The [Fluke 107](https://www.fluke.com/en-us/product/electrical-testing/digital-multimeters/fluke-107), from Fluke's spec table:

| It does | Which means |
|---|---|
| DC volts, 1 mV resolution on the 6 V range | Cell voltage, loaded or not, is the one thing it's really good at here |
| DC amps, 4 A and 10 A ranges only, 1 mA resolution, ±(1.5% + 3 counts) | Fine for the linear channel. No mA or µA range, so nothing near the 60 to 75 µA off current |
| Frequency, 10 Hz to 100 kHz | Reaches every PWM frequency here, if it can get a clean signal |
| Duty cycle, specified around 50/60 Hz only | Don't trust it on the PWM |
| Ohms, continuity, diode, hold | Board checks with the cell out |
| 3 readings a second, no min/max | Can't catch anything shorter than about a second. Eyes are quicker than this meter |
| No temperature | Nothing here checks the sensor against a real temperature |
| 11 A fuse | See the rules below |

The programmer reads flash, EEPROM and fuses, and writes flash and EEPROM. It can't watch the firmware run. So apart from what the meter gets at the tail, what the firmware thinks is invisible unless a test build shows it with the light.

Your eyes are the third instrument. A dark room shows a millisecond flash from the FET far better than any of the above.

## The spare chip kit

The breakout is the same chip as the torch with nothing expensive attached, so everything risky happens there first. Its pins are also the one place the meter gets a clean logic signal: on the torch the PWM is buried in the driver, and here it's a header pin.

From [the breakout's schematic](https://github.com/adafruit/Adafruit-ATtinyx16-Breakouts-PCB), read net by net. The labels are the schematic's net names, so check them against the silkscreen:

| Torch | Chip pin | Breakout header | What's different |
|---|---|---|---|
| Linear channel | PB1 | SDA | 10 kΩ pull-up to VCC, and a STEMMA QT socket |
| FET channel | PB0 | SCL | The same |
| Side switch | PA5 | 1 | Nothing else on it |
| Button LED | PB5 | 4 | Nothing else on it |
| R pad | PA0 | UPDI | 10 kΩ pull-up to VCC |
| V pad | VCC | VIN | VCC is VIN unless the jumper on the back's been changed. There's no VCC pin as such. 3V is a regulator output, so don't feed it |
| G pad | GND | GND | |

Wiring, with the Friend's slide switch on 3 V:

- Friend's UPDI, power and GND (on its JST lead or 0.1" holes, in that order on the schematic) to the breakout's UPDI, VIN and GND.
- A tactile switch from pin 1 to GND. Mind which legs are joined: the 12 mm ones have four.
- A blue LED from SDA to GND, a second from SCL to GND, cathodes to GND.

Nothing on the order list limits an LED's current. At 3.3 V a blue LED's forward voltage does most of the job, so it's fine straight on a pin for this. At 5 V only the pin's own drive limits it, which could pass the 40 mA absolute maximum (DS40002204A table 36-1), so **keep the switch on 3 V** or add 1 kΩ in series. Some 1 kΩ resistors are worth having anyway (see [what this kit can't do](#what-this-kit-cant-do)). The header strips come with the board and need soldering.

What the breakout can't tell you: anything about the driver. There's no cell, no gates and no regulator to light. The pull-ups mean PB0 and PB1 go high through a reset, where the torch's float, so a glow on those LEDs at power-on is expected here and says nothing about 3.1. The green power LED is always on, so the board's supply current says nothing about sleep. And it's a different chip from the torch's, possibly another silicon revision, so a pass here makes a torch pass likely without closing the roadmap item.

## The High Voltage UPDI Friend's 12 V pulse

Read this before plugging the Friend into anything. It's a good serial UPDI adapter (CH340E, 1 kΩ from TX to RX, and it powers the target at 3.3 V or USB's 5 V), and it has one trick that matters here.

Its [schematic](https://github.com/adafruit/Adafruit-High-Voltage-UPDI-Friend-PCB) has RTS# going through a 10 nF capacitor to the control pin of an analog switch, which a 10 kΩ resistor holds high. High connects the UPDI line to the serial port. Low connects it to the 12 V rail, which is always up. So a falling edge on RTS# puts 12 V on UPDI for roughly 100 µs (my reading of the RC, not a spec), and there's no switch or jumper to stop it. From the feedback resistors the rail should be about 12.4 V, if the MT3608's reference is the usual 0.6 V.

Opening a serial port normally asserts RTS, which is that falling edge. `xtask/src/serial.rs` can't prevent it: the kernel does it during `open`, before `stty` runs. So assume **every `cargo xtask` command fires one pulse as it starts.** Likely, not confirmed: the CH340 driver on macOS might behave differently, and the meter's too slow to see it.

What the datasheet says about that (DS40002204A):

- The pin takes it. The absolute maximum on RESET is 13 V (table 36-1), and detection is 11.5 to 12.5 V (table 36-11). But the same page warns that overshoot past 13 V can latch the pin's ESD protection on, and a 12 V driver can then push enough current through it to do permanent damage. Nothing here has measured this board's overshoot.
- It only describes the pulse as a way into UPDI from another pin function (§33.3.2.1.3). What it does to a chip already set to UPDI, like these, isn't stated. It says the rising edge resets UPDI, and that UPDI then stays in reset until the line's driven low. That's probably harmless, and the tool's retry with a double break covers it. Whether it disturbs the running firmware is what 0.6 finds out.
- It's only rated for the RESET pin. On any other pin the limit is VDD + 0.5 V. **Never put the Friend's UPDI line on another pin**, which rules out using it as a serial console on the breakout's TX pin for T2.

The torch adds a question the breakout can't answer: what else is on the R pad. hardware.md doesn't know. So the Friend stays off the torch, and the JLHawaii808 kit does that job. Its listing doesn't mention high voltage or RTS.

## Rules

- **Never put the meter in series above the linear channel.** The FET connects the LEDs straight to the cell, which can blow the 11 A fuse and puts the meter's leads in the current path. Measure cell volts across the cell for those levels, never amps.
- **Cell out before anything touches the pads.** The programmer powers the chip through V. Check G is continuous with the battery negative first, every time you've had it apart.
- **Stay with it above level 75, in short bursts.** The thermal constants are tuned on a made-up head, and the cell's unprotected. A hand on the head is the only thermometer here, so stop when it's uncomfortable.
- **Don't take the cell under 2.8 V.** The firmware should turn off at 2.99 V, but that's one of the things being tested.
- **Nothing here writes fuses.** If a fuse looks wrong, stop and decide with the datasheet open. Having a high-voltage programmer on the bench doesn't change that: `xtask` still can't write them, and the Friend's rescue for a bad `SYSCFG0` is untested.
- **The Friend never touches the torch.** It's for the breakout, and the JLHawaii808 kit is for the torch (see [the pulse](#the-high-voltage-updi-friends-12-v-pulse)). The one reason to break this would be a torch that's lost UPDI, and nothing in this plan can cause that.
- **Get the breakout's backup out of `backup/` before Stage 1.** `xtask` always uses that one directory. If it's left there, the torch's backup refuses to run, and a later `restore` would put seesaw on the torch. Stage 0 moves it.

Any firmware problem below is recoverable: flash and EEPROM are all that ever change, so `cargo xtask restore` puts the stock light back whatever state it's in.

## Stage 0: the breakout

Every step of the first flash, rehearsed on a chip that doesn't matter. Wired as in [the spare chip kit](#the-spare-chip-kit), powered by the Friend, with the LEDs off the pins while the meter's on them, since they'd drag the reading. Volts are against GND, and VCC is whatever the meter reads on VIN.

The breakout ships running seesaw, so if the chip's locked, 0.1 or 0.2 will say it didn't enter programming mode. Stop there: the programmer won't unlock a chip.

This stage proves `xtask` against a real chip, using the Friend. It doesn't prove the JLHawaii808 kit, whose probe is made for 1.27 mm pads and won't fit the breakout's headers. A failure here might also be the Friend's pulse rather than `xtask`, so a flaky result isn't a verdict on the torch's adapter.

| # | Do | Needs | Expect | Rehearses |
|---|---|---|---|---|
| 0.1 | `cargo xtask ping <port>`, ten times | Now | `found tinyAVR P:0D:1-3` every time. A first attempt that only works on the retry would be the pulse holding UPDI in reset | 1.4 |
| 0.2 | `cargo xtask backup <port>` | Now | `flash.hex`, `eeprom.hex`, `fuses.hex` | 1.5 |
| 0.3 | Read `backup/fuses.hex` against `selftest::Fuses::ok`, as 1.6 | Now | Seesaw might not use the factory fuses. If this fails, the firmware refuses the FET here for a good reason, so 0.9 to 0.11 can't work. Don't write the fuses | 1.6 |
| 0.4 | `cargo xtask flash <port>`, then `restore`, then `flash` again | Now | All verify. Seesaw comes back in the middle, which proves `restore` before the torch needs it | 2.1, and `restore`, which nothing else tests |
| 0.5 | Move `backup/` out of the repo, somewhere like `~/ts21-breakout-backup` | | `ls backup` finds nothing. Don't skip this | |
| 0.6 | LED on SDA. 1C on, then `cargo xtask ping <port>` ten times with the light on, then 1C off | Now | Stays on throughout, and clicks still work. `ping` claims to change nothing, so the light going out means the pulse or the port opening resets the chip. It's no danger on this board, and the torch never gets this adapter, so all this decides is how far to trust Stage 0's results. Write it down | The pulse |
| 0.7 | Switch and LED, as 3.5: 1C on, 1C off, a few dozen times | Now | Clicks feel right. Power-cycling the Friend brings it up in about half a second. The LED glows through the proof's resets, thanks to the pull-up, which shows they happen | 3.5 |
| 0.8 | LED off. Level 50: meter on frequency from SDA to GND, then on DC volts | Now | 19.4 kHz, a clean signal, so 3.8's "may not trigger" doesn't apply. A wrong clock is unmistakable: 10 MHz would read double. DC volts should be 29.1% of VCC, 75 / 258, since the meter averages the PWM | 3.6, 3.8, "Check 5 MHz" on silicon |
| 0.9 | T1 at level 100, right after a flash: DC volts on SCL | T1 | 3.5% of VCC, 9 / 255, about 116 mV at 3.3 V. Zero means capped, so go through the suspects in 4.1 here, where it costs nothing. Covers the CRC, the proof and SRAM surviving a watchdog reset | 4.1 |
| 0.10 | The same build, left off for an hour, then on | T1 | Still 3.5%. Zero means a `wdr` got lost in power-down | 4.2 |
| 0.11 | T1 at 75, 76 and 150: DC volts on SCL and SDA | T1 | At 75: SCL 0, SDA VCC. At 76: SCL 0.39% of VCC, about 13 mV, the off-by-one check as a number. At 150: both VCC | 4.3, the duty arithmetic, with no heat to worry about |
| 0.12 | T1 at level 1: frequency and DC volts on SDA | T1 | 1.22 kHz, and 0.1% of VCC, about 3 mV | 5.5 |
| 0.13 | Develop T2 here, and compare its voltage against the Fluke on VIN at 3 V and 5 V (LEDs off the pins for the 5 V reading) | T2 | Within 0.1 V. There's no drop from cell to VCC to confuse it, so this checks the ADC sums alone | 6.2 |

Stop at anything unexplained. What goes wrong here would've gone wrong on the torch, except here `flash` puts it right.

## Stage 1: stock, before flashing

The last chance to measure Anduril, which is what the 5 MHz checks compare against, and the first run of the JLHawaii808 kit on any chip. Stage 0 goes first.

| # | Do | Expect | Closes |
|---|---|---|---|
| 1.1 | Cell voltage on the Fluke, out of the light | 3.6 to 4.2 V. Write it down | |
| 1.2 | Tail current on stock: cap off, meter from cell negative to the tube, at the top of the linear channel | About 350 mA, which is the forum figure this firmware's sums rest on. Start low and ramp up slowly: past about 0.4 A you're onto the FET, so stop | Confirms the linear channel's current |
| 1.3 | Note how bright that level looks, and whether the light whines | For comparison at 4.4 | |
| 1.4 | `cargo xtask ping <port>`, first with the probe touching nothing, then on the pads | In the air: "no reply from the chip", which proves the port and the adapter's echo with no chip at risk. "Nothing echoed back" means the adapter's the problem. On the pads: `found tinyAVR P:0D:1-3` | First half of "ping, back up and flash" |
| 1.5 | `cargo xtask backup <port>`, then copy `backup/` somewhere safe | `flash.hex`, `eeprom.hex`, `fuses.hex` | Second half |
| 1.6 | Read `backup/fuses.hex` against `selftest::Fuses::ok` | `WDTCFG` 0, `OSCCFG.FREQSEL` 20 MHz with `OSCLOCK` clear, `SYSCFG0.RSTPINCFG` UPDI, `APPEND` and `BOOTEND` 0 | "`backup/fuses.hex` passes" |
| 1.7 | Read `BODCFG` and `SYSCFG1.SUT` from the same file | `BODCFG` 0x00 (BOD off), `SUT` 64 ms | "Find out whether the BOD's on", and the start-up time in the watchdog item |

If 1.6 fails, stop: this firmware would quietly refuse the FET, and that's worth understanding before going further.

## Stage 2: first flash

| # | Do | Expect | Closes |
|---|---|---|---|
| 2.1 | `cargo xtask flash <port>` | Verify passes | "Ping, back up and flash", the batched UPDI frames, `PAGEWRITE` |
| 2.2 | If verify fails on flash but EEPROM was fine | Suspect `PAGEWRITE`, then split the batched frames (roadmap, section 1) | |

## Stage 3: does it run

In a dark room, on main's firmware, with level 50 the only level.

| # | Do | Expect | Closes |
|---|---|---|---|
| 3.1 | Put the cell in and do up the head, watching the emitters. Repeat ten times | No flash at all. The watchdog proof makes three resets on every power-on and the pins float through each. A flash here means a gate isn't held low: stop, and go to 3.2 | Stands in for the scope on the gates, for the proof's resets |
| 3.2 | Cell out, head off: ohms from PB0 and PB1 to G, if you can reach the pins or their gate resistors | Tens of kΩ or less means a pull-down, which settles the float question. Open means there isn't one | "Nobody knows if there's a pull-down" |
| 3.3 | Look at the button | Amber glow, steady | "See the amber button LED glow", and that PB5 is it |
| 3.4 | Watch the button LED closely in the dark for a minute, off | Steady. A blink or flicker every so often could be the chip resetting, since every pin floats through a reset | Hint on "`wdr` survives power-down" |
| 3.5 | 1C on, 1C off, a few dozen times, then a click straight after putting the cell in | Light in under about half a second after a cell goes in. Clicks feel like stock | 64 Hz tick, click feel, the 0.3 s proof time by eye |
| 3.6 | Tail current at level 50 | About 100 mA: level 50 is about 29% duty of about 350 mA | The ramp, PB1 and the n/TOP duty |
| 3.7 | Tail current, off, on the 4 A range | Reads 0.000 to 0.003. Anything steadily over about 5 mA isn't a sleeping chip | The rough version of "measure the off current", and a gross reset loop |
| 3.8 | With the meter on frequency across the cell or the current path, at level 50 | 19.4 kHz, which is 5 MHz over level 50's TOP of 258. It may not trigger on so small a ripple: no reading isn't a failure, but a reading is a direct check of the 5 MHz clock | "Check 5 MHz" |
| 3.9 | Level 50 for an hour, in sight | Stays on and doesn't move. It turning itself off is the watchdog or the clock check tripping. Write down how long it took | "It never trips in normal use", "a good chip doesn't trip" at room temperature |
| 3.10 | The same, with the head properly warm at the end, then clicks | Still on, clicks still work | The clock check, warm |
| 3.11 | Leave it off overnight, then 1C | Comes on at once, as normal | `wdr` surviving power-down, in part: it'd come back off and capped, and only T1 can see the cap |

## Stage 4: above the linear channel

Needs T1. Rebuild for each level, flash, and put the cell back. Short bursts.

The cap is the only way a failed proof, flash CRC or fuse check shows at all, and it only shows above level 75. Level 100 makes it obvious: capped at 75, it'll be far dimmer than it should be. The meter stays out of series for all of this stage.

| # | Do | Expect | Closes |
|---|---|---|---|
| 4.1 | T1 at level 100, right after a flash | Clearly brighter than a T1 at 75. If they look the same, it's capped: suspect `xtask/src/crc.rs`, then the fuses, then SRAM not surviving a watchdog reset | "Goes above level 75", the CRC convention, and the whole proof, starting with SRAM surviving |
| 4.2 | Leave that build off for an hour, then on | Still level 100. Capped means a `wdr` got lost in power-down and it took the bite for a hang | "`wdr` survives power-down" |
| 4.3 | T1 at level 76 and 75, side by side by eye | 76 a touch brighter, since a compare of 1 is where an off-by-one in the duty would show | "Check 5 MHz" |
| 4.4 | T1 at 75 and 150, against your notes from 1.3 | As bright as stock. Whine is no worse | "Check 5 MHz" |
| 4.5 | T1 at 150: cell volts across the cell, for a few seconds | Write down the sag | "How far the cell sags on the FET at turbo" |
| 4.6 | Same run, in the dark, turn it off and on a few times | No blip on either edge | "Switching off repeatedly", "a change of level shows no blip" |

## Stage 5: the bottom end

Needs T1, at the linear channel only, so the meter's fine here.

| # | Do | Expect | Closes |
|---|---|---|---|
| 5.1 | T1 at level 1 | It lights. If not, raise `ramp::MIN_PULSE` | "That level 1 lights" |
| 5.2 | Levels 1, 5, 11 in the dark, waving the light | Write down any strobing, since it's 1.22 kHz at the bottom | "Flicker at levels 1 to 11" |
| 5.3 | Levels 19, 22, 25, 28, 30 in turn | Even steps, each brighter | "Steps around levels 19 to 30" |
| 5.4 | Ear to the head at low levels | Any whine, compared with 1.3 | "Coil whine" |
| 5.5 | Meter on frequency, as 3.8, at level 1 | 1.22 kHz if it triggers | The dynamic TOP |

## Stage 6: voltage and heat

| # | Do | Needs | Expect | Closes |
|---|---|---|---|---|
| 6.1 | Run a cell down on stock, with the meter across it between bursts, to about 3.3 V. Flash main, then run level 50 until it steps down, checking the cell | Now | Steps down around 3.10 V (loaded) after 5 s. Turns off around 2.99 V after a second. Rest voltage will read higher | "The step-down and cut-off" |
| 6.2 | Compare a blinked-out voltage with the Fluke at three or four charge levels, off and at level 50 | T2 | Within 0.1 V, which is what stock manages | "The firmware's reading matches a multimeter" |
| 6.3 | Blink out the temperature at a stable room, three or four readings | T2 | Something near the room, ±3 °C by the datasheet. There's no reference to hold it to, so this only finds gross errors | Part of the thermal item |
| 6.4 | Blink out several temperature readings, light off, then at levels 10, 50 and 75 | T2 | How far they scatter, one against the next, at each. That's the noise #55's gain leans on | "Measure the sensor's noise with the light on" |
| 6.5 | Blink out the clock count, cold from a cell swap, then warm | T2 | About 78,125 clocks at room temperature (306 cycles of 255) | "A good chip counts near 306" |
| 6.6 | Charge it over USB with the cell in, watching for the light or the button LED resetting | Now | Nothing odd. There's no remembered level yet, so this is only a rehearsal | "USB charging should keep it" |

## Test builds

Neither exists yet, and they should stay on a throwaway branch: they aren't the firmware.

Build and debug both on the breakout first. A T1 that's wrong there costs a reflash, and on the torch it could be a FET running at the wrong duty.

**T1, another level.** Today's `ui::LEVEL` is 50, with an assert holding it to the linear channel. Changing both isn't enough: `cargo xtask hex` runs every docs/ui.md example, the examples expect level 50, and no hex gets written. Changing the examples to match doesn't get there either, since the low-battery rows work in levels too. And a hex made some other way misses the CRC, which would refuse the FET and spoil the test. So T1 needs a small `xtask` change first: build at a given level, skip the spec examples and keep everything else, and write the CRC as usual. `cargo xtask flash <port> <file>` already takes any hex. Bringing back `fancy-ui` would replace T1 altogether.

**T2, a number out.** A build that blinks a value out on the main LED, Anduril's version-check style, at level 50: tens on long blinks, units on short. One build per value is fine: `torch`'s battery reading, the temperature, the last count from `hw::count_tick`. It's the same thing as roadmap section 3's battery check on 3C, so that item could do most of this job for real.

## What this kit can't do

| Check | Why | What would unlock it |
|---|---|---|
| Off current against 60 to 75 µA | No mA or µA range, and the breakout's power LED hides it there | A 1 kΩ resistor in series with the cell: about 70 mV on the 600 mV range |
| Drop from cell to VCC | V is under the head, and the head has to be on for the light to run | A wire from the body to the head, so the meter can reach V with the head off |
| FET through a real hang (up to 64 ms) | Too quick for the meter, and needs a hang on demand | A scope, and a T1 that hangs on purpose. Eyes can do part of it: a flash that lingers. The LED on SCL is the safe place to try that build |
| Clearing CMPnEN mid-pulse, a waveform output on a stopped TCA0 | Needs a scope, and a failure on demand | A scope |
| Whether a BOTTOM under LUPD clears BV | Inside the chip. Only the blips in 4.6 would hint at it | A T2 counting reissued rows |
| A dead 32 kHz oscillator, TCB0 waking idle, `SYNC_POLLS` against a real sync | Needs a fault on demand | Nothing cheap. They stay simulator-only |
| Sensor against a real temperature, thermal retune | No thermometer | Any thermometer on the head |
| Clock check cold and hot | Needs a controlled range | A freezer gets the cold end, and the breakout can go in without an unprotected cell. Bag it against condensation |
| 4.2 V to 2.5 V sweep | No bench supply | A bench supply. 6.1 covers the part that matters |

Until those happen, the roadmap's watchdog item stays unticked, and docs/hardware.md's labels stay as they are. Move a fact from likely to confirmed only with a result from here beside it, and say so if the result came from the breakout rather than a TS21.

## Results

Fill this in as you go, with the commit you flashed.

| # | Date | Commit | Result |
|---|---|---|---|
| | | | |
