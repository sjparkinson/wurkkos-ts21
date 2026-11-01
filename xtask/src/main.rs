//! `cargo xtask <command>`: the steps around `cargo fw` that cargo can't do.
//!
//! No dependencies. ELF reading, Intel HEX and the UPDI programmer are all
//! small enough to do by hand, which saves needing avr-objcopy, avrdude or
//! pymcuprog.

mod elf;
mod ihex;
mod serial;
mod updi;

use std::env;
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

<port> is the serial UPDI adapter, e.g. /dev/cu.usbserial-1410 on macOS
or /dev/ttyUSB0 on Linux.
";

const ELF: &str = "target/avr-none/release/ts21.elf";
const HEX: &str = "target/ts21.hex";

const FLASH_BYTES: u32 = 16 * 1024;
const RAM_BYTES: u32 = 2 * 1024;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();

    let result = match args.as_slice() {
        ["hex"] => hex().map(|_| ()),
        ["size"] => size(),
        ["ping", port] => ping(port),
        ["backup", port] => backup(port),
        ["flash", port] => build().and_then(|elf| flash(port, &elf.flash_image())),
        ["flash", port, file] => read_hex(Path::new(file)).and_then(|image| flash(port, &image)),
        ["restore", port] => restore(port),
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

fn build() -> Result<elf::Elf, String> {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    run(Command::new(cargo).current_dir(root()).arg("fw"))?;
    let path = root().join(ELF);
    let bytes = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    elf::Elf::parse(&bytes).map_err(|e| format!("{}: {e}", path.display()))
}

fn hex() -> Result<PathBuf, String> {
    let image = build()?.flash_image();
    let path = root().join(HEX);
    write(&path, &ihex::encode(&image))?;
    eprintln!("wrote {HEX} ({} bytes of flash)", image.len());
    Ok(path)
}

fn size() -> Result<(), String> {
    let elf = build()?;
    let flash = elf.flash_image().len() as u32;
    let ram = elf.ram_bytes();
    println!(
        "flash {flash:>5} / {FLASH_BYTES} bytes ({}%)",
        flash * 100 / FLASH_BYTES
    );
    println!(
        "ram   {ram:>5} / {RAM_BYTES} bytes ({}%), plus the stack",
        ram * 100 / RAM_BYTES
    );
    Ok(())
}

fn connect(port: &str) -> Result<Programmer<Port>, String> {
    eprintln!("connecting on {port}");
    Programmer::connect(Port::open(port)?)
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

    let mut p = connect(port)?;
    p.enter_progmode()?;
    p.check_signature()?;
    eprintln!("reading flash");
    let flash = p.read(map::FLASH, map::FLASH_SIZE)?;
    eprintln!("reading EEPROM and fuses");
    let eeprom = p.read(map::EEPROM, map::EEPROM_SIZE)?;
    let fuses = p.read(map::FUSES, map::FUSES_SIZE)?;
    p.leave()?;

    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
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
        return Err("the EEPROM image is bigger than 256 bytes".into());
    }

    let mut p = connect(port)?;
    p.enter_progmode()?;
    p.check_signature()?;
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
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    ihex::decode(&text).map_err(|e| format!("{}: {e}", path.display()))
}

fn write(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))
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
