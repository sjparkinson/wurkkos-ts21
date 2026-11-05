//! `cargo xtask <command>`: the AVR build and the steps around it that cargo
//! can't do.
//!
//! No dependencies beyond the firmware's own library. ELF reading, Intel
//! HEX, the UPDI programmer and the AVR simulator are all small enough to do
//! by hand, which saves needing avr-objcopy, avrdude, pymcuprog or simavr.

#[cfg(test)]
mod atdf;
mod avr;
mod crc;
mod elf;
mod ihex;
mod layout;
mod serial;
mod sim;
mod simulate;
mod spec;
mod switching;
mod terminal;
mod updi;
mod world;

use std::env;
use std::fmt::Display;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serial::Port;
use updi::{Programmer, map};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  hex                     build the firmware, write target/wurkkos-ts21-0614.hex
                          and print flash, RAM, stack and loop use
  ping <port>             check the programmer can see the chip, change nothing
  backup <port> <dir>     save the chip's flash, EEPROM and fuses into <dir>
  flash <port> [file.hex] build and flash the firmware, or flash file.hex
  restore <port> <dir>    put <dir>/flash.hex and <dir>/eeprom.hex back
  simulate [script...]    try src/ui.rs live in the terminal, or on a script
                          such as `1C 2H:1.5s wait:1s 3C`

<port> is the serial UPDI adapter, e.g. /dev/cu.usbserial-1410 on macOS
or /dev/ttyUSB0 on Linux. <dir> names one chip's backup, e.g. backup/torch.
";

const ELF: &str = "target/avr-none/release/wurkkos-ts21-0614.elf";
/// src/bin/link_probe.rs, which `build` builds alongside the firmware.
const PROBE: &str = "target/avr-none/release/link-probe.elf";
/// src/bin/pwm_probe.rs, built the same way.
const PWM_PROBE: &str = "target/avr-none/release/pwm-probe.elf";
const HEX: &str = "target/wurkkos-ts21-0614.hex";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["hex"] => hex(),
        ["ping", port] => ping(port),
        ["backup", port, dir] => backup(port, Path::new(dir)),
        ["flash", port] => build()
            .and_then(|(elf, _)| crc::sign(&elf.flash_image()))
            .and_then(|image| flash(port, &image)),
        ["flash", port, file] => read_hex(Path::new(file)).and_then(|image| flash(port, &image)),
        ["restore", port, dir] => restore(port, Path::new(dir)),
        ["simulate"] => simulate::interactive(),
        ["simulate", script @ ..] => simulate::script(script),
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn root() -> PathBuf {
    // xtask/ sits one below the workspace root.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has a parent directory")
        .to_path_buf()
}

/// Builds the firmware for the AVR, then checks the ELF's layout and runs it
/// on the simulator.
fn build() -> Result<(elf::Elf, sim::Report), String> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run(Command::new(cargo).current_dir(root()).args([
        "build",
        "--package",
        "ts21",
        "--release",
        "--target",
        "avr-none",
        "-Zbuild-std=core",
    ]))?;
    let path = root().join(ELF);
    let bytes = fs::read(&path).map_err(at(&path))?;
    let elf = elf::Elf::parse(&bytes).map_err(at(&path))?;
    layout::check(&elf).map_err(at(&path))?;
    let report = sim::check(&elf).map_err(|e| format!("{}: simulator: {e}", path.display()))?;

    let probe = root().join(PROBE);
    let bytes = fs::read(&probe).map_err(at(&probe))?;
    let probe_elf = elf::Elf::parse(&bytes).map_err(at(&probe))?;
    layout::memory(&probe_elf).map_err(at(&probe))?;
    sim::probe(&probe_elf).map_err(|e| format!("{}: simulator: {e}", probe.display()))?;

    let probe = root().join(PWM_PROBE);
    let bytes = fs::read(&probe).map_err(at(&probe))?;
    let probe_elf = elf::Elf::parse(&bytes).map_err(at(&probe))?;
    sim::pwm_probe(&probe_elf).map_err(|e| format!("{}: simulator: {e}", probe.display()))?;
    Ok((elf, report))
}

/// Writes the image `flash` would: the firmware padded to all of flash with
/// 0xFF and its CRC in the last two bytes, which the firmware checks at
/// power-on (`crc.rs`). Then prints flash, RAM, stack and loop use.
fn hex() -> Result<(), String> {
    let (elf, sim) = build()?;
    let firmware = elf.flash_image();
    let image = crc::sign(&firmware)?;
    write(&root().join(HEX), &ihex::encode(&image))?;
    eprintln!("wrote {HEX}");

    let flash = firmware.len() as u64;
    let ram = elf.ram_bytes().into();
    let ram_size = (elf::RAM.end - elf::RAM.start).into();
    println!(
        "{}",
        row("flash", flash, Some(map::FLASH_SIZE as u64), "bytes")
    );
    println!("{}", row("ram", ram, Some(ram_size), "bytes"));
    println!("{}", row("stack", sim.stack.into(), None, "bytes"));
    println!(
        "{}",
        row(
            "loop",
            sim.loop_instructions,
            Some(sim.loop_budget),
            "instructions per 64 Hz tick"
        )
    );
    println!(
        "{}",
        row(
            "off",
            sim.off_instructions,
            Some(sim.loop_budget),
            "instructions per 64 Hz tick"
        )
    );
    Ok(())
}

/// One line of the size report, in columns: used, out of, percent, unit.
fn row(label: &str, used: u64, of: Option<u64>, unit: &str) -> String {
    let (of, percent) = match of {
        Some(of) => (
            format!("/ {:>6}", commas(of)),
            format!("{:>3}%", used * 100 / of),
        ),
        None => (String::new(), String::new()),
    };
    format!(
        "{label:<5} {:>6} {of:<8} {percent:>4}  {unit}",
        commas(used)
    )
}

/// `39062` as `39,062`.
fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

fn connect(port: &str) -> Result<Programmer<Port>, String> {
    eprintln!("connecting on {port}");
    Programmer::connect(Port::open(port)?)
}

/// Connects, enters programming mode and refuses anything but an ATtiny1616.
/// Every command that reads or writes memory starts here.
fn connect_for_programming(port: &str) -> Result<Programmer<Port>, String> {
    let mut p = connect(port)?;
    p.enter_progmode()?;
    p.check_signature()?;
    Ok(p)
}

fn ping(port: &str) -> Result<(), String> {
    let mut p = connect(port)?;
    let sib = p.sib()?;
    p.disconnect()?;
    println!("found {}", String::from_utf8_lossy(&sib).trim_end());
    Ok(())
}

fn backup(port: &str, dir: &Path) -> Result<(), String> {
    let flash_path = dir.join("flash.hex");
    if flash_path.exists() {
        return Err(format!(
            "{} already exists. Move it somewhere safe first; if it's the factory \
             firmware, it's the only copy",
            flash_path.display()
        ));
    }

    let mut p = connect_for_programming(port)?;
    eprintln!("reading flash");
    let flash = p.read(map::FLASH, map::FLASH_SIZE)?;
    eprintln!("reading EEPROM and fuses");
    let eeprom = p.read(map::EEPROM, map::EEPROM_SIZE)?;
    let fuses = p.read(map::FUSES, map::FUSES_SIZE)?;
    p.leave()?;

    fs::create_dir_all(dir).map_err(at(dir))?;
    write(&flash_path, &ihex::encode(&flash))?;
    write(&dir.join("eeprom.hex"), &ihex::encode(&eeprom))?;
    write(&dir.join("fuses.hex"), &ihex::encode(&fuses))?;
    eprintln!(
        "saved flash, EEPROM and fuses (for reference, never written back) to {}",
        dir.display()
    );
    Ok(())
}

fn flash(port: &str, image: &[u8]) -> Result<(), String> {
    program(port, image, None)
}

fn restore(port: &str, dir: &Path) -> Result<(), String> {
    let flash = read_hex(&dir.join("flash.hex"))?;
    let eeprom = read_hex(&dir.join("eeprom.hex"))?;
    program(port, &flash, Some(&eeprom))
}

/// Erase, write, verify, restart. The chip erase wipes EEPROM too unless the
/// EESAVE fuse is set; pass `Some` to put contents back.
fn program(port: &str, flash: &[u8], eeprom: Option<&[u8]>) -> Result<(), String> {
    if flash.len() > map::FLASH_SIZE {
        return Err(format!(
            "the image is {} bytes; the ATtiny1616 has {}",
            flash.len(),
            map::FLASH_SIZE
        ));
    }
    if eeprom.is_some_and(|e| e.len() > map::EEPROM_SIZE) {
        return Err(format!(
            "the EEPROM image is bigger than {} bytes",
            map::EEPROM_SIZE
        ));
    }

    let mut p = connect_for_programming(port)?;
    let written = write_and_verify(&mut p, flash, eeprom);
    // Leave programming mode even after a failure, so the chip isn't held in
    // it until the next power cycle. The first error is the one to report.
    let left = p.leave();
    written?;
    left?;
    eprintln!("verified and restarted");
    Ok(())
}

fn write_and_verify(
    p: &mut Programmer<Port>,
    flash: &[u8],
    eeprom: Option<&[u8]>,
) -> Result<(), String> {
    eprintln!("erasing and writing {} bytes of flash", flash.len());
    p.erase_and_write_flash(flash)?;
    p.verify(map::FLASH, flash)?;
    if let Some(eeprom) = eeprom {
        eprintln!("writing EEPROM");
        p.write(map::EEPROM, map::EEPROM_PAGE, eeprom)?;
        p.verify(map::EEPROM, eeprom)?;
    }
    Ok(())
}

fn read_hex(path: &Path) -> Result<Vec<u8>, String> {
    let text = fs::read_to_string(path).map_err(at(path))?;
    ihex::decode(&text).map_err(at(path))
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(at(path))
}

/// Prefixes an error with the file it came from.
fn at<E: Display>(path: &Path) -> impl Fn(E) -> String + '_ {
    move |e| format!("{}: {e}", path.display())
}

fn run(command: &mut Command) -> Result<(), String> {
    let program = command.get_program().to_string_lossy().into_owned();
    let status = command
        .status()
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} failed ({status})"))
    }
}

#[cfg(test)]
mod tests {
    use super::{commas, row};

    #[test]
    fn rows_line_up_with_and_without_a_total() {
        assert_eq!(
            row("flash", 3978, Some(16384), "bytes"),
            "flash  3,978 / 16,384  24%  bytes"
        );
        assert_eq!(
            row("stack", 107, None, "bytes"),
            "stack    107                bytes"
        );
    }

    #[test]
    fn commas_group_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(107), "107");
        assert_eq!(commas(3978), "3,978");
        assert_eq!(commas(39062), "39,062");
        assert_eq!(commas(1_234_567), "1,234,567");
    }
}
