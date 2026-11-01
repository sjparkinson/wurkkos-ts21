//! `cargo xtask <command>`: the steps around `cargo fw` that cargo can't do.
//!
//! No dependencies. The ELF reading and Intel HEX writing are small enough
//! to do by hand, which saves needing avr-objcopy (or anything from LLVM
//! beyond what rustup ships).

mod elf;
mod ihex;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "\
usage: cargo xtask <command>

commands:
  hex                 build the firmware and write target/ts21.hex
  size                build the firmware and print flash and RAM use
  flash <port>        build, then write target/ts21.hex with avrdude
  backup <port>       read the torch's flash, EEPROM and fuses into backup/
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
        ["flash", port] => flash(port),
        ["backup", port] => backup(port),
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
    let elf = build()?;
    let image = elf.flash_image();
    let path = root().join(HEX);
    fs::write(&path, ihex::encode(&image)).map_err(|e| format!("{}: {e}", path.display()))?;
    eprintln!("wrote {} ({} bytes of flash)", HEX, image.len());
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

fn flash(port: &str) -> Result<(), String> {
    let hex = hex()?;
    run(Command::new("avrdude")
        .args(["-c", "serialupdi", "-p", "t1616", "-P", port])
        .arg("-e")
        .arg(format!("-Uflash:w:{}:i", hex.display())))
}

fn backup(port: &str) -> Result<(), String> {
    let dir = root().join("backup");
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    run(Command::new("avrdude")
        .current_dir(&dir)
        .args(["-c", "serialupdi", "-p", "t1616", "-P", port])
        .args(["-Uflash:r:flash.hex:i", "-Ueeprom:r:eeprom.hex:i"])
        .args(["-Ufuses:r:fuses.hex:i"]))?;
    eprintln!("saved flash, EEPROM and fuses to backup/");
    Ok(())
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
