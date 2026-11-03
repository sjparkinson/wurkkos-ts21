//! Not firmware: a program with one static of each kind, for `cargo xtask`
//! to check link.x and src/rt.rs against. The torch firmware has no statics
//! yet, so as linked its .rodata, .data and .bss are all empty and nothing
//! real reads through the mapped-flash window or relies on the startup copy.
//!
//! `main` copies a const table (.rodata, read through the mapped-flash
//! window) and a static with non-zero starting values (.data, which the
//! startup code copies from flash) into a static that starts at zero (.bss),
//! then stops in `link_probe_done`. xtask/src/sim.rs runs it and checks what
//! landed in `LINK_PROBE_COPY`, so keep the values here in step with
//! `PROBE_EXPECTED` there. `cargo xtask hex` builds this alongside the firmware;
//! it's never flashed.

#![cfg_attr(target_arch = "avr", no_std, no_main)]
#![cfg_attr(target_arch = "avr", feature(asm_experimental_arch))]
#![cfg_attr(target_arch = "avr", feature(abi_avr_interrupt))]

// rt.rs needs a few of regs.rs's constants, not all of them.
#[cfg(target_arch = "avr")]
#[allow(dead_code)]
#[path = "../regs.rs"]
mod regs;
#[cfg(target_arch = "avr")]
#[path = "../rt.rs"]
mod rt;

/// rt.rs's vector table jumps to these by name. The probe never enables an
/// interrupt, so they're empty.
#[cfg(target_arch = "avr")]
mod hw {
    /// The pins rt.rs holds low through its self-tests, as in src/hw.rs.
    pub const FET: u8 = 0;
    pub const LINEAR: u8 = 1;

    pub extern "avr-interrupt" fn porta_port() {}
    pub extern "avr-interrupt" fn rtc_pit() {}
}

#[cfg(target_arch = "avr")]
mod probe {
    use core::hint::black_box;
    use core::sync::atomic::{AtomicU8, Ordering::Relaxed};

    // The statics are `no_mangle` so sim.rs can find them by name. Odd
    // lengths, so .data and .bss need link.x's ALIGN(2) padding.

    /// Immutable, so .rodata.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    static LINK_PROBE_TABLE: [u8; 3] = [0x12, 0x34, 0x56];

    /// Interior mutability with non-zero starting values, so .data.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    static LINK_PROBE_DATA: [AtomicU8; 3] = [
        AtomicU8::new(0x9A),
        AtomicU8::new(0xBC),
        AtomicU8::new(0xDE),
    ];

    /// Starts at zero, so .bss.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    static LINK_PROBE_COPY: [AtomicU8; 7] = [const { AtomicU8::new(0) }; 7];

    // src/rt.rs calls `main` by name once memory is set up.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    extern "C" fn main() -> ! {
        // `black_box` stops the compiler folding the reads into constants,
        // so they really go through the addresses link.x gave the statics.
        let table = black_box(&LINK_PROBE_TABLE);
        let data = black_box(&LINK_PROBE_DATA);
        let copy = black_box(&LINK_PROBE_COPY);
        let (from_table, from_data) = copy.split_at(table.len());
        for (to, &b) in from_table.iter().zip(table) {
            to.store(b, Relaxed);
        }
        for (to, from) in from_data.iter().zip(data) {
            to.store(from.load(Relaxed), Relaxed);
        }
        link_probe_done()
    }

    /// Where sim.rs stops and looks. Nothing runs it on a real chip, so
    /// spinning costs nothing.
    #[allow(unsafe_code, clippy::empty_loop)]
    #[unsafe(no_mangle)]
    #[inline(never)]
    extern "C" fn link_probe_done() -> ! {
        loop {}
    }

    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }
}

#[cfg(not(target_arch = "avr"))]
fn main() {
    eprintln!("this is a linker check for the ATtiny1616; `cargo xtask hex` runs it");
    std::process::exit(1);
}
