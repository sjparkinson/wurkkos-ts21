# Flashing

`cargo xtask` has a UPDI programmer built in. It runs on macOS and Linux, and sets up the serial port with `stty`.

## What you need

A serial UPDI adapter and some way to hold it on three tiny pads.

The easy option is a ready-made USB UPDI programmer. [JLHawaii808's kit](https://jlhawaii808.com/products/updi-programmer-driver-reflashling-kit) is a CH340 serial UPDI adapter with USB-C, in 3.3 V and 4.5 V versions, and either works for Wurkkos lights (the 4.5 V one is listed for Sofirn, Wurkkos and Emisar). thefreeman sells one on BLF ([thread](https://budgetlightforum.com/t/wts-usb-updi-programmers-for-attiny1616-avr32dd20-drivers-sofirn-wurkkos-fireflylite-emisar/71459)) that comes with loose pogo-pin leads as well as the fixed probe.

The cheap option is a USB-serial adapter (CH340, CP2102, FT232) wired as a serial UPDI programmer, with TX and RX joined and that joined line going to R. [pymcuprog's README](https://github.com/microchip-pic-avr-tools/pymcuprog) suggests a series resistor of about 1 kΩ between TX and UPDI, and an adapter running at the same voltage as the MCU. The CH340 module in [pingle.org's guide](https://www.pingle.org/2022/08/07/flashing-a-3-pin-t1616-flashlight-with-new-firmware) worked wired straight through. Add pogo pins or a steady hand.

Watch the pad spacing before you rely on a fixed probe. gchart's standard three-pad layout is `+ - R` at 1.27 mm pitch, and that's what most probes expect. Some Wurkkos and Sofirn boards use 2.54 mm instead (thefreeman ships loose leads for exactly that reason). The TS21's R, G, V is the same arrangement read from the R end, so mind which way round a probe goes. Loose pogo leads work whatever the layout. The TS21 (model 0614) is 1.27 mm (measured on mine), so a standard probe fits.

Use a short USB cable. JLHawaii808 notes failures with 1 m ones.

Adafruit's High Voltage UPDI Friend is a serial UPDI adapter too, but it puts a 12 V pulse on the UPDI line on every falling edge of RTS, which probably means every time a tool opens the port. Nothing turns that off. Keep it for a breakout and use a plain adapter on the torch: [bench-plan.md](bench-plan.md#the-high-voltage-updi-friends-12-v-pulse) has why. With that adapter, the "changes nothing" below about `ping` is unproven.

## Getting to the pads

Unscrew the head from the body. The driver's on the back of the head and the R, G, V pads are on it. Take the cell out, since the programmer powers the MCU through V (anything from 3.3 V to 5 V depending on the programmer, and the MCU's rated to 5.5 V). Before anything touches R, check with a multimeter that G connects to the battery negative contact, because pad order isn't the same on every light.

## Find the port

Run `ls /dev/cu.*` before and after plugging the adapter in. On macOS it's something like `/dev/cu.usbserial-1410` or `/dev/cu.wchusbserial1410`. On Linux it's usually `/dev/ttyUSB0`, and you may need to be in the `dialout` group.

## Check it's talking

```sh
cargo xtask ping /dev/cu.usbserial-XXXX
```

This reads the chip's System Information Block and changes nothing, not even a reset. You should see something like `found tinyAVR P:0D:1-3`. If you don't, see [If it doesn't talk](#if-it-doesnt-talk).

## Back up the factory firmware first

Do this before you flash anything. It's the only copy of Wurkkos's build and its calibration that you'll have.

```sh
cargo xtask backup /dev/cu.usbserial-XXXX backup/torch
```

That puts the chip in programming mode, checks it's an ATtiny1616, and saves `flash.hex` (all 16 KB), `eeprom.hex` and `fuses.hex` in the directory you name, creating it if it needs to. A relative path starts from where you run the command. The fuses are there for reference only. It refuses to run if `flash.hex` is already there, so a second run can't overwrite your factory copy with this firmware.

Give each chip its own directory: nothing in a backup says which chip it came from. `backup/` is git-ignored. Copy yours somewhere safe too.

## Flash this firmware

```sh
cargo xtask flash /dev/cu.usbserial-XXXX
```

That builds the firmware and then

1. puts the chip in programming mode and checks its signature (1E 94 21)
2. chip-erases it, which wipes flash and, unless the EESAVE fuse is set (Microchip ships it unset), EEPROM
3. writes the image a 64-byte page at a time, skipping blank pages (the erase already left them blank). The image is padded to all 16 KB with a CRC in the last two bytes, which the firmware checks at power-on, so the last page is always written
4. reads it all back and compares
5. resets the chip and turns UPDI off, so the new firmware starts

If a write or the compare fails, step 5 still runs, so the chip isn't left in programming mode. Whatever was half-written then runs, but with the cell out it's powered by the programmer alone. Run the command again.

## Going back

`restore` puts your backup's flash and EEPROM back. It writes every EEPROM page, blank ones included, since with EESAVE set the old contents survive the erase.

```sh
cargo xtask restore /dev/cu.usbserial-XXXX backup/torch
```

For current Anduril, download a release zip from [ToyKeeper's releases](https://github.com/ToyKeeper/anduril/releases), pull `anduril.sofirn-sp36-t1616.hex` out of it and flash that.

```sh
cargo xtask flash /dev/cu.usbserial-XXXX path/to/anduril.sofirn-sp36-t1616.hex
```

That's the build Wurkkos ships for model 0614, and people report recent ones running fine on the TS21.

## If it won't go above the linear channel

The power-on self-tests (docs/ui.md) quietly cap the light at the top of the linear channel if flash doesn't match its CRC or the fuses aren't what the firmware expects. If that happens after a flash that verified, suspect the CRC convention in `xtask/src/crc.rs` first, since it's never been checked against a real chip. Then the fuses: `cargo xtask backup` saves them to `fuses.hex` in its directory, to compare with the checks in `src/selftest.rs`. Nothing here writes fuses, so that's a decision for you, not the tool. After an interrupted flash, run `cargo xtask flash` again.

## If it doesn't talk

- "no reply from the chip" usually means poor contact or the wrong pad on R. Press harder, or check the pads again. The tool already retries once with a UPDI double break.
- "nothing echoed back" means the adapter isn't joining TX and RX, so it isn't a serial UPDI adapter (or the joining wire's come off).
- If a DIY adapter never works with TX and RX tied directly, add the 1 kΩ resistor.
- thefreeman's older 3.3 V programmers had trouble with some lights, which is why the current one supplies 4.5 V. If yours is 3.3 V and flaky, that might be it.
- The anduril.click [UPDI guide](https://anduril.click/drivers/updi.html) and the [reflash reference](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942) cover more adapters and their quirks.

## How it works

The programmer writes flash and EEPROM, and only those. That's all a TS21 needs, and it keeps fuses out of reach, since a bad fuse write can turn off UPDI for good.

`xtask/src/updi.rs` is the protocol, covering link set-up, the NVM programming key, reset, page-buffer writes through the NVM controller and reads in 256-byte blocks. The sequences follow Microchip's pymcuprog, and the registers come from the ATDF. `xtask/src/serial.rs` opens the port, configures it with `stty` (115200 baud, 8 data bits, even parity, 2 stop bits) and swallows the echo that a one-wire adapter sends back. It drains anything already waiting in the adapter when it opens the port and after a double break, as avrdude does, so a stray byte can't throw the echo check off.

The tests run all of it against a pretend ATtiny1616 in `updi.rs`. That catches slips in framing and sequencing, but it was written from the same reading of the datasheet as the programmer, so the first run against a real chip is the real test. Start with `ping`, then `backup`.
