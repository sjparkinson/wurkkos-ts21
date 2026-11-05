//! Enough XML to read Microchip's ATDF, and the generator that turns the copy
//! in `vendor/` into the constants in `src/regs.rs`.
//!
//! This file does two jobs. The root package's `build.rs` loads it by path and
//! calls [`generate`]. xtask compiles it for its tests only (`main.rs` declares
//! it under `#[cfg(test)]`), which is where the parser and the generator get
//! tested, because a build script can't have tests of its own. The tests also
//! check what generating can't: what the firmware assumes about the ATDF's
//! layout, and that `link.x` agrees with it.
//!
//! The parser handles what ATDF files contain: elements, double- or
//! single-quoted attributes, comments and the XML declaration. Text content
//! is skipped, since the ATDF keeps everything in attributes.

use std::collections::BTreeMap;

/// One XML element: its name, attributes and child elements.
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Element>,
}

impl Element {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    fn get(&self, key: &str) -> &str {
        self.attr(key)
            .unwrap_or_else(|| panic!("<{}> has no {key} attribute", self.name))
    }

    fn all<'a>(&'a self, tag: &'static str) -> impl Iterator<Item = &'a Element> {
        self.children.iter().filter(move |c| c.name == tag)
    }

    fn child(&self, tag: &str) -> &Element {
        self.children
            .iter()
            .find(|c| c.name == tag)
            .unwrap_or_else(|| panic!("<{}> has no <{tag}>", self.name))
    }

    /// The child `<tag name="...">`.
    fn named(&self, tag: &str, name: &str) -> &Element {
        self.children
            .iter()
            .find(|c| c.name == tag && c.attr("name") == Some(name))
            .unwrap_or_else(|| panic!("<{}> has no <{tag} name=\"{name}\">", self.name))
    }

    fn num(&self, key: &str) -> u32 {
        num(self.get(key))
    }
}

fn num(s: &str) -> u32 {
    let parsed = match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => s.parse(),
    };
    parsed.unwrap_or_else(|_| panic!("{s:?} isn't a number"))
}

fn parse(xml: &str) -> Element {
    let mut stack = vec![Element {
        name: "#document".into(),
        attrs: Vec::new(),
        children: Vec::new(),
    }];
    let mut rest = xml;
    while let Some(i) = rest.find('<') {
        rest = &rest[i..];
        if let Some(r) = rest.strip_prefix("<!--") {
            let end = r.find("-->").expect("unterminated comment");
            rest = &r[end + 3..];
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            let end = rest.find('>').expect("unterminated declaration");
            rest = &rest[end + 1..];
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').expect("unterminated end tag");
            let element = stack.pop().expect("end tag with nothing open");
            assert_eq!(element.name, r[..end].trim(), "mismatched end tag");
            stack
                .last_mut()
                .expect("end tag closes the document")
                .children
                .push(element);
            rest = &r[end + 1..];
        } else {
            let mut quote = None;
            let end = rest
                .char_indices()
                .find(|&(_, c)| match quote {
                    Some(q) if c == q => {
                        quote = None;
                        false
                    }
                    Some(_) => false,
                    None if c == '"' || c == '\'' => {
                        quote = Some(c);
                        false
                    }
                    None => c == '>',
                })
                .map(|(j, _)| j)
                .expect("unterminated start tag");
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            let (tag, empty) = match tag.strip_suffix('/') {
                Some(t) => (t, true),
                None => (tag, false),
            };
            let element = parse_tag(tag);
            if empty {
                stack
                    .last_mut()
                    .expect("no open element")
                    .children
                    .push(element);
            } else {
                stack.push(element);
            }
        }
    }
    assert_eq!(
        stack.len(),
        1,
        "unclosed <{}>",
        stack.last().map_or("", |e| &e.name)
    );
    stack.pop().expect("the document is always on the stack")
}

fn parse_tag(tag: &str) -> Element {
    let tag = tag.trim();
    let name_end = tag.find(char::is_whitespace).unwrap_or(tag.len());
    let mut element = Element {
        name: tag[..name_end].into(),
        attrs: Vec::new(),
        children: Vec::new(),
    };
    let mut rest = tag[name_end..].trim_start();
    while !rest.is_empty() {
        let eq = rest
            .find('=')
            .unwrap_or_else(|| panic!("bad attribute in <{tag}>"));
        let key = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let q = after.chars().next().filter(|&c| c == '"' || c == '\'');
        let q = q.unwrap_or_else(|| panic!("unquoted attribute in <{tag}>"));
        let close = after[1..]
            .find(q)
            .unwrap_or_else(|| panic!("unterminated attribute in <{tag}>"));
        element
            .attrs
            .push((key.into(), unescape(&after[1..=close])));
        rest = after[close + 2..].trim_start();
    }
    element
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// A register on one peripheral instance, at its data-space address.
struct Register<'a> {
    instance: &'a str,
    name: &'a str,
    address: u32,
    size: u32,
    element: &'a Element,
}

/// Every register on every peripheral instance. TCA0's come twice where its
/// SINGLE and SPLIT modes share a name.
fn registers(document: &Element) -> Vec<Register<'_>> {
    let file = document.child("avr-tools-device-file");
    let device = file.child("devices").child("device");
    let mut registers = Vec::new();
    for module in device.child("peripherals").all("module") {
        let definition = file.child("modules").named("module", module.get("name"));
        for instance in module.all("instance") {
            for group in instance.all("register-group") {
                let name = group.get("name-in-module");
                for element in definition.named("register-group", name).all("register") {
                    registers.push(Register {
                        instance: instance.get("name"),
                        name: element.get("name"),
                        address: group.num("offset") + element.num("offset"),
                        size: element.num("size"),
                        element,
                    });
                }
            }
        }
    }
    registers
}

/// Generated constants on their way out, by name, so they come out sorted.
#[derive(Default)]
struct Consts(BTreeMap<String, (u32, String)>);

impl Consts {
    /// Adds a constant. A name can turn up more than once (RTC.PITINTFLAGS.PI
    /// and RTC.PITINTCTRL.PI, or a TCA register in both modes), which is fine
    /// while the value agrees. If it doesn't, the names in `src/regs.rs` have
    /// stopped being unique, and the build should say so. The ATDF's names
    /// are nearly all upper case; a few values, like `10mV`, aren't.
    fn add(&mut self, name: String, value: u32, doc: String) {
        let name = name.to_ascii_uppercase();
        if let Some((theirs, first)) = self.0.get(&name) {
            assert_eq!(
                *theirs, value,
                "{name} is both `{first}` and `{doc}`, with different values"
            );
        } else {
            self.0.insert(name, (value, doc));
        }
    }

    fn render(&self, kind: fn(u32) -> &'static str) -> String {
        self.0
            .iter()
            .map(|(name, (value, doc))| {
                let kind = kind(*value);
                format!("/// {doc}\npub const {name}: {kind} = {value:#04X};\n")
            })
            .collect()
    }
}

/// `path`, and the ATDF's caption for it if it has one.
fn doc(path: String, element: &Element) -> String {
    match element.attr("caption") {
        Some(caption) if !caption.is_empty() => format!("{path}: {caption}"),
        _ => path,
    }
}

/// The Rust source of every constant the ATDF can give us. `src/regs.rs`
/// includes it, and describes the names.
///
/// Only addresses, masks, named values, segments, vectors and the signature.
/// The `initval`s stay out on purpose: they disagree with the datasheet on
/// ADC0.CALIB and the factory OSCCFG and SYSCFG0.
pub fn generate(xml: &str) -> String {
    let document = parse(xml);
    let file = document.child("avr-tools-device-file");
    let device = file.child("devices").child("device");

    let mut addr = Consts::default();
    for r in registers(&document) {
        let width = match r.size {
            1 => String::new(),
            size => format!(", {}-bit", size * 8),
        };
        addr.add(
            format!("{}_{}", r.instance, r.name),
            r.address,
            doc(format!("{}.{}{width}", r.instance, r.name), r.element),
        );
    }

    let mut mem = Consts::default();
    let spaces = device.child("address-spaces").all("address-space");
    for segment in spaces.flat_map(|space| space.all("memory-segment")) {
        let name = segment.get("name");
        let (start, size) = (segment.num("start"), segment.num("size"));
        mem.add(format!("{name}_START"), start, format!("{name}'s start."));
        mem.add(format!("{name}_SIZE"), size, format!("{name}'s size."));
        let end = "The first address after it.".to_string();
        mem.add(format!("{name}_END"), start + size, end);
        if let Some(page) = segment.attr("pagesize") {
            let doc = format!("{name}'s page size.");
            mem.add(format!("{name}_PAGE"), num(page), doc);
        }
    }

    let mut bits = Consts::default();
    for module in file.child("modules").all("module") {
        let prefix = module.get("name");
        let registers = module
            .all("register-group")
            .flat_map(|group| group.all("register"));
        for register in registers {
            for field in register.all("bitfield") {
                let mask = field.num("mask");
                let name = format!("{prefix}_{}", field.get("name"));
                let path = format!("{prefix}.{}.{}", register.get("name"), field.get("name"));
                let values = field.attr("values").map(|group| {
                    let group = module.named("value-group", group);
                    group.all("value")
                });
                for value in values.into_iter().flatten() {
                    let label = value.get("name");
                    let shifted = value.num("value") << mask.trailing_zeros();
                    assert_eq!(shifted & !mask, 0, "{path}={label} spills out of its mask");
                    bits.add(
                        format!("{name}_{label}"),
                        shifted,
                        doc(format!("{path} = {label}"), value),
                    );
                }
                bits.add(name, mask, doc(path, field));
            }
        }
    }

    let mut vectors = Consts::default();
    let mut count = 0;
    for interrupt in device.child("interrupts").all("interrupt") {
        let index = interrupt.num("index");
        let (instance, name) = (interrupt.get("module-instance"), interrupt.get("name"));
        count = count.max(index + 1);
        let path = format!("{instance}.{name}'s vector");
        vectors.add(
            format!("VECTOR_{instance}_{name}"),
            index,
            doc(path, interrupt),
        );
    }
    let doc = "Entries in the vector table, reset included.".to_string();
    vectors.add("VECTORS".into(), count, doc);

    let property = |name: &str| {
        let groups = device.child("property-groups").all("property-group");
        groups
            .flat_map(|group| group.all("property"))
            .find(|p| p.attr("name") == Some(name))
            .unwrap_or_else(|| panic!("no property {name}"))
            .num("value")
    };
    let signature = [
        property("SIGNATURE0"),
        property("SIGNATURE1"),
        property("SIGNATURE2"),
    ];

    format!(
        "/// Data-space addresses of every register.\n\
         pub mod addr {{\n{}}}\n\n\
         /// Every memory segment. All of them are in data space but PROGMEM.\n\
         pub mod mem {{\n{}}}\n\n\
         {}\n{}\n\
         /// The device signature.\n\
         pub const SIGNATURE: [u8; 3] = {signature:#04X?};\n",
        addr.render(|_| "usize"),
        mem.render(|_| "usize"),
        bits.render(|value| if value > 0xFF { "u16" } else { "u8" }),
        vectors.render(|_| "usize"),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use ts21::regs::{self, addr, mem};

    use super::*;

    fn atdf() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/ATtiny1616.atdf");
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()))
    }

    #[test]
    fn parses_attributes_comments_and_nesting() {
        let doc = parse(
            "<?xml version=\"1.0\"?>\n<!-- a <comment> -->\n\
             <a x=\"1\" y='a &gt; b'><b name=\"c\"/><b name=\"d\">text</b></a>",
        );
        let a = doc.child("a");
        assert_eq!(a.attr("x"), Some("1"));
        assert_eq!(a.attr("y"), Some("a > b"));
        assert_eq!(a.all("b").count(), 2);
        assert_eq!(a.named("b", "d").name, "b");
        assert_eq!(num("0x0A00"), 0x0A00);
        assert_eq!(num("16"), 16);
    }

    /// One constant per rule in `generate`, as the datasheet has them, so a
    /// generator that goes wrong can't agree with itself.
    #[test]
    fn generates_each_kind_of_constant() {
        assert_eq!(addr::CPU_CCP, 0x0034, "a plain address");
        assert_eq!(addr::ADC0_CTRLA, 0x0600, "base plus offset");
        assert_eq!(addr::TCA0_CMP1BUF, 0x0A3A, "a SINGLE mode register");
        assert_eq!(addr::FUSE_BOOTEND, 0x1288, "a fuse");
        assert_eq!(mem::INTERNAL_SRAM_START, 0x3800, "a segment's start");
        assert_eq!(mem::INTERNAL_SRAM_SIZE, 0x0800, "a segment's size");
        assert_eq!(mem::INTERNAL_SRAM_END, 0x4000, "a segment's end");
        assert_eq!(mem::EEPROM_PAGE, 32, "a segment's page size");
        assert_eq!(regs::PORT_PULLUPEN, 0x08, "a mask");
        assert_eq!(regs::TCA_CMP1EN, 0x20, "a mask on a mode's register");
        assert_eq!(regs::TCD_CMPASET, 0x0FFF, "a mask wider than a byte");
        assert_eq!(regs::ADC_REFSEL_VDDREF, 0x10, "a shifted value");
        assert_eq!(regs::RTC_PERIOD_CYC512, 0x40, "a shifted value");
        assert_eq!(regs::ADC_REFSEL_INTREF, 0x00, "a value of zero");
        assert_eq!(regs::VECTOR_RTC_PIT, 7, "a vector");
        assert_eq!(regs::VECTORS, 31, "the vector count");
        assert_eq!(regs::SIGNATURE, [0x1E, 0x94, 0x21], "the signature");
    }

    #[test]
    fn generated_constants_say_where_they_came_from() {
        let source = generate(&atdf());
        for line in [
            "pub const ADC0_RES: usize = 0x610;",
            "pub const TCD_CMPASET: u16 = 0xFFF;",
            "pub const RTC_PI: u8 = 0x01;",
        ] {
            assert!(source.contains(line), "{line}");
        }
        assert!(source.contains("/// ADC0.RES, 16-bit: "), "the width");
    }

    #[test]
    fn repeated_names_are_fine_when_they_agree() {
        let mut consts = Consts::default();
        consts.add("A".into(), 1, "first".into());
        consts.add("A".into(), 1, "second".into());
        assert_eq!(
            consts.render(|_| "u8"),
            "/// first\npub const A: u8 = 0x01;\n"
        );
    }

    #[test]
    #[should_panic(expected = "A is both `first` and `second`")]
    fn repeated_names_that_disagree_stop_the_build() {
        let mut consts = Consts::default();
        consts.add("A".into(), 1, "first".into());
        consts.add("A".into(), 2, "second".into());
    }

    /// `write16` and `read16` take these two bytes at a time.
    #[test]
    fn the_16_bit_registers_are_16_bits() {
        let xml = atdf();
        let document = parse(&xml);
        let registers = registers(&document);
        for wide in [
            addr::TCA0_CNT,
            addr::TCA0_CMP0,
            addr::TCA0_CMP1,
            addr::TCA0_PER,
            addr::TCA0_PERBUF,
            addr::TCA0_CMP0BUF,
            addr::TCA0_CMP1BUF,
            addr::TCB0_CNT,
            addr::TCB0_CCMP,
            addr::ADC0_RES,
        ] {
            let found = registers
                .iter()
                .any(|r| r.address as usize == wide && r.size == 2);
            assert!(found, "nothing 16 bits wide at {wide:#06x}");
        }
    }

    /// hw.rs reaches PINnCTRL as PIN0CTRL + n.
    #[test]
    fn pin_controls_are_consecutive() {
        let xml = atdf();
        let document = parse(&xml);
        let registers = registers(&document);
        for (base, port) in [
            (addr::PORTA_PIN0CTRL, "PORTA"),
            (addr::PORTB_PIN0CTRL, "PORTB"),
            (addr::PORTC_PIN0CTRL, "PORTC"),
        ] {
            for n in 0..8 {
                let name = format!("PIN{n}CTRL");
                let register = registers
                    .iter()
                    .find(|r| r.instance == port && r.name == name)
                    .unwrap_or_else(|| panic!("no {port}.{name}"));
                assert_eq!(base + n, register.address as usize, "{port}.{name}");
            }
        }
    }

    /// WINDOW takes the same values as PERIOD, which the simulator relies on
    /// to time both with one function, and hw.rs to check both with
    /// `wdt_clocks`.
    #[test]
    fn watchdog_window_values_are_the_period_values_shifted() {
        for (window, period) in [
            (regs::WDT_WINDOW_8CLK, regs::WDT_PERIOD_8CLK),
            (regs::WDT_WINDOW_32CLK, regs::WDT_PERIOD_32CLK),
            (regs::WDT_WINDOW_8KCLK, regs::WDT_PERIOD_8KCLK),
        ] {
            assert_eq!(window, period << 4);
        }
    }

    /// link.x's MEMORY regions, and every other address in it, against the
    /// ATDF. 0x800000 (`elf::DATA_SPACE`) is where AVR ELF files put data
    /// space, a toolchain convention the ATDF doesn't record.
    #[test]
    fn linker_script_matches_the_atdf() {
        let data = crate::elf::DATA_SPACE;
        let mapped = mem::MAPPED_PROGMEM_START as u32;

        let script = link_x();
        let (memory, sections) = linker_script(&script);
        assert_eq!(
            memory,
            [
                (
                    "FLASH".into(),
                    mem::PROGMEM_START as u32,
                    mem::PROGMEM_SIZE as u32
                ),
                (
                    "MAPPED".into(),
                    data + mapped,
                    mem::MAPPED_PROGMEM_SIZE as u32
                ),
                (
                    "RAM".into(),
                    data + mem::INTERNAL_SRAM_START as u32,
                    mem::INTERNAL_SRAM_SIZE as u32
                ),
            ],
            "link.x MEMORY"
        );
        // SECTIONS turns flash addresses into mapped ones, as ELF addresses
        // for .rodata and as data-space ones for __data_load.
        for n in sections {
            assert!(
                n == data + mapped || n == mapped,
                "link.x SECTIONS has {n:#x}, which isn't the mapped-flash window"
            );
        }
    }

    #[test]
    fn parses_a_linker_script() {
        let (memory, sections) = linker_script(
            "/* MEMORY { X : ORIGIN = 1 } 0x1 */\n\
             MEMORY {\n  A (rx) : ORIGIN = 0x10, LENGTH = 2K\n  B : ORIGIN = 0, LENGTH = 0x20\n}\n\
             SECTIONS { .a (0x800 + 1) : AT(0x9) { } }",
        );
        assert_eq!(memory, [("A".into(), 0x10, 2048), ("B".into(), 0, 0x20)]);
        assert_eq!(sections, [0x800, 0x9]);
    }

    fn link_x() -> String {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../link.x");
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    /// Just enough of a GNU ld script: the (name, origin, length) of each
    /// MEMORY region, and the hex numbers anywhere after MEMORY. Comments
    /// are dropped first.
    fn linker_script(script: &str) -> (Vec<(String, u32, u32)>, Vec<u32>) {
        let mut text = String::new();
        let mut rest = script;
        while let Some(at) = rest.find("/*") {
            text.push_str(&rest[..at]);
            let end = rest[at..].find("*/").expect("unterminated comment");
            rest = &rest[at + end + 2..];
        }
        text.push_str(rest);

        let start = text.find("MEMORY").expect("no MEMORY block");
        let open = start + text[start..].find('{').expect("no { after MEMORY");
        let close = open + text[open..].find('}').expect("no } after MEMORY");
        let memory = text[open + 1..close]
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(|line| {
                let (name, spec) = line.split_once(':').expect("region without :");
                let name = name.split_whitespace().next().expect("region name");
                let field = |key: &str| {
                    let value = spec
                        .split(',')
                        .find_map(|f| f.trim().strip_prefix(key))
                        .and_then(|f| f.trim().strip_prefix('='))
                        .unwrap_or_else(|| panic!("{name} has no {key}"))
                        .trim();
                    match value.strip_suffix('K') {
                        Some(k) => num(k) * 1024,
                        None => num(value),
                    }
                };
                (name.to_string(), field("ORIGIN"), field("LENGTH"))
            })
            .collect();

        let numbers = text[close + 1..]
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|w| w.starts_with("0x"))
            .map(num)
            .collect();
        (memory, numbers)
    }
}
