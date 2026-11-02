//! `cargo xtask <command>`: the steps around `cargo fw` that cargo can't do.
//!
//! No dependencies beyond the firmware's own library. ELF reading, Intel
//! HEX, the UPDI programmer and the AVR simulator are all small enough to do
//! by hand, which saves needing avr-objcopy, avrdude, pymcuprog or simavr.

#[cfg(test)]
mod atdf;
mod avr;
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
  hex                     build the firmware and write target/ts21.hex
  size                    build the firmware and print flash and RAM use
  ping <port>             check the programmer can see the chip, change nothing
  backup <port>           save the torch's flash, EEPROM and fuses into backup/
  flash <port> [file.hex] build and flash the firmware, or flash file.hex
  restore <port>          put backup/flash.hex and backup/eeprom.hex back
  simulate [script...]    try src/ui.rs live in the terminal, or on a script
                          such as `1C 2H:1.5s wait:1s 3C`

<port> is the serial UPDI adapter, e.g. /dev/cu.usbserial-1410 on macOS
or /dev/ttyUSB0 on Linux.
";

const ELF: &str = "target/avr-none/release/ts21.elf";
/// src/bin/link_probe.rs, which `cargo fw` builds alongside the firmware.
const PROBE: &str = "target/avr-none/release/link-probe.elf";
const HEX: &str = "target/ts21.hex";

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["hex"] => hex(),
        ["size"] => size(),
        ["ping", port] => ping(port),
        ["backup", port] => backup(port),
        ["flash", port] => build().and_then(|(elf, _)| flash(port, &elf.flash_image())),
        ["flash", port, file] => read_hex(Path::new(file)).and_then(|image| flash(port, &image)),
        ["restore", port] => restore(port),
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

fn build() -> Result<(elf::Elf, sim::Report), String> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run(Command::new(cargo).current_dir(root()).arg("fw"))?;
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
    Ok((elf, report))
}

fn hex() -> Result<(), String> {
    let image = build()?.0.flash_image();
    write(&root().join(HEX), &ihex::encode(&image))?;
    eprintln!("wrote {HEX} ({} bytes of flash)", image.len());
    Ok(())
}

fn size() -> Result<(), String> {
    let (elf, sim) = build()?;
    let flash = elf.flash_image().len() as u32;
    let flash_size = map::FLASH_SIZE as u32;
    let ram = elf.ram_bytes();
    let ram_size = elf::RAM.end - elf::RAM.start;
    println!(
        "flash {flash:>5} / {flash_size} bytes ({}%)",
        flash * 100 / flash_size
    );
    println!(
        "ram   {ram:>5} / {ram_size} bytes ({}%), plus the stack",
        ram * 100 / ram_size
    );
    println!("stack {:>5} bytes at most, in the simulator", sim.stack);
    println!(
        "loop  {:>5} instructions at most per 64 Hz tick (budget {})",
        sim.loop_instructions, sim.loop_budget
    );
    Ok(())
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

fn backup(port: &str) -> Result<(), String> {
    let dir = root().join("backup");
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

    fs::create_dir_all(&dir).map_err(at(&dir))?;
    write(&flash_path, &ihex::encode(&flash))?;
    write(&dir.join("eeprom.hex"), &ihex::encode(&eeprom))?;
    write(&dir.join("fuses.hex"), &ihex::encode(&fuses))?;
    eprintln!("saved flash, EEPROM and fuses (for reference, never written back) to backup/");
    Ok(())
}

fn flash(port: &str, image: &[u8]) -> Result<(), String> {
    program(port, image, None)
}

fn restore(port: &str) -> Result<(), String> {
    let dir = root().join("backup");
    let flash = read_hex(&dir.join("flash.hex"))?;
    let eeprom = read_hex(&dir.join("eeprom.hex"))?;
    program(port, &flash, Some(&eeprom))
}

/// Erase, write, verify, restart. EEPROM goes too, because a chip erase
/// wipes it; pass `Some` to put contents back.
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
    eprintln!("erasing");
    p.chip_erase()?;
    eprintln!("writing {} bytes of flash", flash.len());
    p.write(map::FLASH, map::FLASH_PAGE, flash)?;
    p.verify(map::FLASH, flash)?;
    if let Some(eeprom) = eeprom {
        eprintln!("writing EEPROM");
        p.write(map::EEPROM, map::EEPROM_PAGE, eeprom)?;
        p.verify(map::EEPROM, eeprom)?;
    }
    p.leave()?;
    eprintln!("verified and restarted");
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
