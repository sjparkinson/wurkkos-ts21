//! Turns Microchip's ATDF into register constants, so no address or bit mask
//! in this firmware is ever typed by hand. std only: the ATDF is regular enough
//! that a tag scanner does the job of an XML crate.
//!
//! `addr::RTC_PITCTRLA` is an absolute address, `bits::RTC_PITCTRLA_PITEN` a
//! mask, and `bits::RTC_PITCTRLA_PERIOD_CYC512` a value already shifted into
//! its field. TCA's registers carry the mode: `addr::TCA0_SINGLE_CTRLA`.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::{env, fs, path::Path};

const ATDF: &str = "../vendor/ATtiny1616.atdf";

struct Field {
    name: String,
    mask: u32,
    values: Option<String>,
}

struct Register {
    module: String,
    group: String,
    prefix: String,
    offset: u32,
    fields: Vec<Field>,
}

/// One `<tag a="b">`: its name and attributes.
fn parse_tag(body: &str) -> (&str, HashMap<&str, &str>) {
    let name_end = body.find(char::is_whitespace).unwrap_or(body.len());
    let (name, mut rest) = body.split_at(name_end);
    let mut attrs = HashMap::new();
    while let Some(eq) = rest.find("=\"") {
        let key = rest[..eq].trim();
        let value = &rest[eq + 2..];
        let end = value.find('"').expect("unterminated attribute");
        attrs.insert(key, &value[..end]);
        rest = &value[end + 1..];
    }
    (name, attrs)
}

fn number(text: &str) -> Option<u32> {
    match text.strip_prefix("0x") {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => text.parse().ok(),
    }
}

fn ident(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

fn main() {
    println!("cargo::rerun-if-changed={ATDF}");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=link.x");

    let xml = fs::read_to_string(ATDF).expect("vendor ATDF");
    let mut in_modules = false;
    let mut module = String::new();
    let mut group = String::new();
    let mut instance = String::new();
    let mut value_group = String::new();
    // (module, group in module) -> [(instance, base address)]
    let mut instances: BTreeMap<(String, String), Vec<(String, u32)>> = BTreeMap::new();
    let mut registers: Vec<Register> = Vec::new();
    let mut value_groups: HashMap<(String, String), Vec<(String, u32)>> = HashMap::new();

    let mut rest = xml.as_str();
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest.find('>').expect("unterminated tag");
        let body = rest[..end].trim_end_matches('/');
        rest = &rest[end + 1..];
        if body.starts_with(['?', '!', '/']) {
            continue;
        }
        let (tag, attrs) = parse_tag(body);
        let attr = |key: &str| attrs.get(key).copied().unwrap_or_default().to_string();
        match tag {
            "modules" => in_modules = true,
            "module" => module = attr("name"),
            "instance" => instance = attr("name"),
            "register-group" if !in_modules => {
                if let (Some(name), Some(base)) = (attrs.get("name-in-module"), attrs.get("offset"))
                {
                    instances
                        .entry((module.clone(), (*name).to_string()))
                        .or_default()
                        .push((instance.clone(), number(base).expect("base address")));
                }
            }
            "register-group" => group = attr("name"),
            "register" if in_modules => {
                let mode = attr("modes");
                let prefix = match mode.split_whitespace().next() {
                    Some(first) => format!("{}_{}", ident(first), ident(&attr("name"))),
                    None => ident(&attr("name")),
                };
                registers.push(Register {
                    module: module.clone(),
                    group: group.clone(),
                    prefix,
                    offset: number(&attr("offset")).expect("register offset"),
                    fields: Vec::new(),
                });
            }
            "bitfield" if in_modules => {
                let register = registers.last_mut().expect("bitfield outside a register");
                register.fields.push(Field {
                    name: ident(&attr("name")),
                    mask: number(&attr("mask")).expect("bitfield mask"),
                    values: attrs.get("values").map(ToString::to_string),
                });
            }
            "value-group" if in_modules => value_group = attr("name"),
            "value" if in_modules => {
                if let Some(value) = number(&attr("value")) {
                    value_groups
                        .entry((module.clone(), value_group.clone()))
                        .or_default()
                        .push((ident(&attr("name")), value));
                }
            }
            _ => {}
        }
    }

    // BTreeMaps so a name two modes share comes out once, in a stable order.
    let mut addr = BTreeMap::new();
    let mut bits = BTreeMap::new();
    for register in &registers {
        let key = (register.module.clone(), register.group.clone());
        for (name, base) in instances.get(&key).into_iter().flatten() {
            addr.insert(
                format!("{}_{}", ident(name), register.prefix),
                base + register.offset,
            );
        }
        for field in &register.fields {
            let stem = format!(
                "{}_{}_{}",
                ident(&register.module),
                register.prefix,
                field.name
            );
            if field.mask <= 0xFF {
                bits.insert(stem.clone(), field.mask);
            }
            let values = field.values.clone().unwrap_or_default();
            let shift = field.mask.trailing_zeros();
            let group = (register.module.clone(), values);
            for (value_name, value) in value_groups.get(&group).into_iter().flatten() {
                let shifted = value << shift;
                if shifted <= 0xFF {
                    bits.insert(format!("{stem}_{value_name}"), shifted);
                }
            }
        }
    }

    let mut out = String::from("#[allow(dead_code)]\npub mod addr {\n");
    for (name, value) in &addr {
        writeln!(out, "    pub const {name}: u16 = {value:#06x};").unwrap();
    }
    out.push_str("}\n#[allow(dead_code)]\npub mod bits {\n");
    for (name, value) in &bits {
        writeln!(out, "    pub const {name}: u8 = {value:#04x};").unwrap();
    }
    out.push_str("}\n");
    let dest = Path::new(&env::var("OUT_DIR").unwrap()).join("regs.rs");
    fs::write(dest, out).unwrap();
}
