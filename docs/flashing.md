# Flashing

This only applies to the TS21 v2 (ATtiny1616, model 0614). Check which one you've got first, see [hardware.md](hardware.md#which-ts21-have-you-got).

## What you need

A UPDI programmer and some way to hold it on three tiny pads.

- **The easy way**: a ready-made USB UPDI programmer. thefreeman sells one on BLF ([thread](https://budgetlightforum.com/t/wts-usb-updi-programmers-for-attiny1616-avr32dd20-drivers-sofirn-wurkkos-fireflylite-emisar/71459)) that comes with loose pogo-pin leads as well as the fixed probe, and JLHawaii808 sells a kit that names the TS10 and SP10 Pro, which share this layout.
- **The cheap way**: a USB-serial adapter (CH340, CP2102, FT232) wired as a "serial UPDI" programmer: TX and RX joined, and that joined line goes to R. [pymcuprog's README](https://github.com/microchip-pic-avr-tools/pymcuprog) suggests a series resistor of about 1 kΩ between TX and UPDI, and an adapter running at the same voltage as the MCU. The CH340 module in [pingle.org's guide](https://www.pingle.org/2022/08/07/flashing-a-3-pin-t1616-flashlight-with-new-firmware) needed neither resistor nor diode. Plus pogo pins or a steady hand.
- **avrdude** 7.0 or newer (for `-c serialupdi`), or **pymcuprog**. On macOS: `brew install avrdude`, or `uv tool install pymcuprog`.

Watch the pad spacing before you buy a fixed probe. gchart's standard three-pad layout is `+ - R` at 1.27 mm pitch, and that's what most probes expect. Some Wurkkos and Sofirn boards use 2.54 mm instead (thefreeman ships loose leads for exactly that reason). The TS21's R, G, V is the same arrangement read from the R end, so mind which way round a probe goes. Loose pogo leads work whatever the layout. I haven't measured a TS21 v2 yet, so check yours.

## Getting to the pads

Unscrew the head from the body. The driver's on the back of the head and the R, G, V pads are on it. Take the cell out; the programmer powers the MCU through V (anything from 3.3 V to 5 V depending on the programmer, and the MCU's happy with any of it). Before anything touches R, check with a multimeter that G connects to the battery negative contact, because pad order isn't the same on every light.

## Back up the factory firmware first

Do this before you flash anything. It's the only copy of Wurkkos's build and its calibration that you'll have.

```sh
# Is it talking? This reads the signature and does nothing else.
avrdude -c serialupdi -p t1616 -P /dev/cu.usbserial-XXXX -n

# Save flash, EEPROM and fuses into backup/
cargo xtask backup /dev/cu.usbserial-XXXX
```

`backup/` is git-ignored. Copy it somewhere safe too.

On macOS the port is something like `/dev/cu.usbserial-1410` or `/dev/cu.wchusbserial1410`; `ls /dev/cu.*` before and after plugging in the adapter tells you which. On Linux it's `/dev/ttyUSB0`.

## Flash this firmware

```sh
cargo xtask flash /dev/cu.usbserial-XXXX
```

That builds the firmware, writes `target/ts21.hex` and runs:

```sh
avrdude -c serialupdi -p t1616 -P /dev/cu.usbserial-XXXX -e -Uflash:w:target/ts21.hex:i
```

`-e` erases the chip first, which also wipes the EEPROM unless the EESAVE fuse is set (Microchip ships it unset). To use pymcuprog instead:

```sh
cargo xtask hex
pymcuprog write --erase --verify -t uart -u /dev/cu.usbserial-XXXX -d attiny1616 -f target/ts21.hex
```

Neither writes fuses, and nothing here ever should. See [hardware.md](hardware.md#the-mcu) for why.

## Going back

To your backup:

```sh
avrdude -c serialupdi -p t1616 -P /dev/cu.usbserial-XXXX -e \
  -Uflash:w:backup/flash.hex:i -Ueeprom:w:backup/eeprom.hex:i
```

Or to current Anduril: [ToyKeeper's releases](https://github.com/ToyKeeper/anduril/releases) ship as one zip per release. Pull `anduril.sofirn-sp36-t1616.hex` out of it and flash it the same way. That's the build Wurkkos ships for model 0614, and people report recent ones running fine on a v2.

## If it doesn't talk

- `avrdude: initialization failed` or a timeout usually means poor contact or the wrong pad on R. Press harder, or check the pads again.
- If it never works with TX and RX tied directly, add the 1 kΩ resistor.
- thefreeman's older 3.3 V programmers had trouble with some lights, which is why the current one supplies 4.5 V. If yours is 3.3 V and flaky, that might be it.
- Some adapters need a lower baud rate: add `-b 57600` (avrdude) or `-c 57600` (pymcuprog).
- The anduril.click [UPDI guide](https://anduril.click/drivers/updi.html) and the [reflash reference](https://budgetlightforum.com/t/anduril-flashlight-reflash-reference-updated-for-2024/216942) cover more adapters and their quirks.
