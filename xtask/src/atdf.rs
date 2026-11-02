//! Enough XML to read Microchip's ATDF, and the tests that check every
//! hand-typed register constant against the copy in `vendor/`.
//!
//! Test-only: `main.rs` declares this module under `#[cfg(test)]`. The
//! firmware's constants live in `src/regs.rs`, which is pulled in below by
//! path because the firmware crate never builds for the host with them.
//! The UPDI constants are checked in `updi.rs`'s own tests.
//!
//! The parser handles what ATDF files contain: elements, double- or
//! single-quoted attributes, comments and the XML declaration. Text content
//! is skipped, since the ATDF keeps everything in attributes.

use std::fs;
use std::path::Path;

#[path = "../../src/regs.rs"]
mod regs;

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

/// The vendored ATtiny1617 ATDF.
pub struct Atdf {
    root: Element,
}

/// A memory segment from the device's address spaces.
pub struct Segment {
    pub start: u32,
    pub size: u32,
    pub page: Option<u32>,
}

/// A register on one peripheral instance, at its data-space address.
pub struct Register<'a> {
    pub address: u32,
    pub size: u32,
    element: &'a Element,
    module: &'a Element,
    label: String,
}

impl Atdf {
    pub fn load() -> Atdf {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/attiny1617.atdf");
        let xml = fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("can't read {}: {e}", path.display()));
        Atdf { root: parse(&xml) }
    }

    fn file(&self) -> &Element {
        self.root.child("avr-tools-device-file")
    }

    fn device(&self) -> &Element {
        self.file().child("devices").child("device")
    }

    pub fn segment(&self, name: &str) -> Segment {
        let segment = self
            .device()
            .child("address-spaces")
            .all("address-space")
            .flat_map(|space| space.all("memory-segment"))
            .find(|s| s.attr("name") == Some(name))
            .unwrap_or_else(|| panic!("no memory segment {name}"));
        Segment {
            start: segment.num("start"),
            size: segment.num("size"),
            page: segment.attr("pagesize").map(num),
        }
    }

    /// A `<property>` from any of the device's property groups.
    pub fn property(&self, name: &str) -> u32 {
        let property = self
            .device()
            .child("property-groups")
            .all("property-group")
            .flat_map(|group| group.all("property"))
            .find(|p| p.attr("name") == Some(name))
            .unwrap_or_else(|| panic!("no property {name}"));
        property.num("value")
    }

    /// Reset plus every interrupt: the highest vector index, plus one.
    pub fn vectors(&self) -> u32 {
        let interrupts = self.device().child("interrupts").all("interrupt");
        interrupts
            .map(|i| i.num("index"))
            .max()
            .expect("no interrupts")
            + 1
    }

    /// `register("TCA0", Some("SINGLE"), "CTRLA")` is TCA0.CTRLA in single
    /// mode. Pass a mode only for modules that have them.
    pub fn register(&self, instance: &str, mode: Option<&str>, name: &str) -> Register<'_> {
        let label = match mode {
            Some(m) => format!("{instance}.{m}.{name}"),
            None => format!("{instance}.{name}"),
        };
        let (module_name, group) = self
            .device()
            .child("peripherals")
            .all("module")
            .find_map(|m| {
                let i = m
                    .all("instance")
                    .find(|i| i.attr("name") == Some(instance))?;
                Some((m.get("name"), i.child("register-group")))
            })
            .unwrap_or_else(|| panic!("no peripheral instance {instance}"));
        let module = self.file().child("modules").named("module", module_name);
        let mut matches = module
            .named("register-group", group.get("name-in-module"))
            .all("register")
            .filter(|r| r.attr("name") == Some(name))
            .filter(|r| match (mode, r.attr("modes")) {
                (Some(m), Some(modes)) => modes.split_whitespace().any(|x| x == m),
                (None, None) => true,
                _ => false,
            });
        let element = matches
            .next()
            .unwrap_or_else(|| panic!("no register {label}"));
        assert!(matches.next().is_none(), "more than one {label}");
        Register {
            address: group.num("offset") + element.num("offset"),
            size: element.num("size"),
            element,
            module,
            label,
        }
    }
}

impl Register<'_> {
    fn bitfield(&self, name: &str) -> &Element {
        self.element.named("bitfield", name)
    }

    /// A bitfield's mask.
    pub fn mask(&self, field: &str) -> u32 {
        self.bitfield(field).num("mask")
    }

    /// A named value of a bitfield, shifted into place.
    pub fn value(&self, field: &str, value: &str) -> u32 {
        let bitfield = self.bitfield(field);
        let mask = bitfield.num("mask");
        let group = self.module.named("value-group", bitfield.get("values"));
        let shifted = group.named("value", value).num("value") << mask.trailing_zeros();
        assert_eq!(
            shifted & !mask,
            0,
            "{}.{field}={value} spills out of its mask",
            self.label
        );
        shifted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Every address in `regs::addr`, and that the 16-bit ones really are.
    #[test]
    fn register_addresses_match_the_atdf() {
        let atdf = Atdf::load();
        let at = |instance: &str, mode: Option<&str>, name: &str| {
            let register = atdf.register(instance, mode, name);
            (register.address as usize, register.size)
        };
        let single = Some("SINGLE");

        for (ours, instance, mode, name, size) in [
            (regs::addr::CPU_CCP, "CPU", None, "CCP", 1),
            (regs::addr::CPU_SREG, "CPU", None, "SREG", 1),
            (regs::addr::VPORTA_IN, "VPORTA", None, "IN", 1),
            (regs::addr::VPORTB_DIR, "VPORTB", None, "DIR", 1),
            (
                regs::addr::CLKCTRL_MCLKCTRLB,
                "CLKCTRL",
                None,
                "MCLKCTRLB",
                1,
            ),
            (regs::addr::RTC_PITCTRLA, "RTC", None, "PITCTRLA", 1),
            (regs::addr::RTC_PITSTATUS, "RTC", None, "PITSTATUS", 1),
            (regs::addr::RTC_PITINTFLAGS, "RTC", None, "PITINTFLAGS", 1),
            (regs::addr::PORTA_PIN0CTRL, "PORTA", None, "PIN0CTRL", 1),
            (regs::addr::PORTB_PIN0CTRL, "PORTB", None, "PIN0CTRL", 1),
            (regs::addr::PORTC_PIN0CTRL, "PORTC", None, "PIN0CTRL", 1),
            (regs::addr::TCA0_CTRLA, "TCA0", single, "CTRLA", 1),
            (regs::addr::TCA0_CTRLB, "TCA0", single, "CTRLB", 1),
            (regs::addr::TCA0_CNT, "TCA0", single, "CNT", 2),
            (regs::addr::TCA0_PERBUF, "TCA0", single, "PERBUF", 2),
            (regs::addr::TCA0_CMP0BUF, "TCA0", single, "CMP0BUF", 2),
            (regs::addr::TCA0_CMP1BUF, "TCA0", single, "CMP1BUF", 2),
        ] {
            let (address, theirs) = at(instance, mode, name);
            assert!(
                ours == address,
                "{instance}.{name}: regs.rs says 0x{ours:04X}, the ATDF says 0x{address:04X}"
            );
            assert_eq!(size, theirs, "{instance}.{name} is {theirs} bytes wide");
        }

        // hw.rs reaches PINnCTRL as PIN0CTRL + n.
        for (base, port) in [
            (regs::addr::PORTA_PIN0CTRL, "PORTA"),
            (regs::addr::PORTB_PIN0CTRL, "PORTB"),
            (regs::addr::PORTC_PIN0CTRL, "PORTC"),
        ] {
            for n in 1..8 {
                let name = format!("PIN{n}CTRL");
                assert_eq!(base + n, at(port, None, &name).0, "{port}.{name}");
            }
        }
    }

    /// Every bitfield value in `regs`, which must already be shifted into
    /// place.
    #[test]
    fn bitfield_values_match_the_atdf() {
        let atdf = Atdf::load();
        let ccp = atdf.register("CPU", None, "CCP");
        let mclkctrlb = atdf.register("CLKCTRL", None, "MCLKCTRLB");
        let pinctrl = atdf.register("PORTA", None, "PIN0CTRL");
        let ctrla = atdf.register("TCA0", Some("SINGLE"), "CTRLA");
        let ctrlb = atdf.register("TCA0", Some("SINGLE"), "CTRLB");
        let pitctrla = atdf.register("RTC", None, "PITCTRLA");
        let pitstatus = atdf.register("RTC", None, "PITSTATUS");
        let pitintflags = atdf.register("RTC", None, "PITINTFLAGS");

        for (ours, theirs, label) in [
            (
                regs::CPU_CCP_IOREG,
                ccp.value("CCP", "IOREG"),
                "CPU.CCP=IOREG",
            ),
            (
                regs::CLKCTRL_PEN,
                mclkctrlb.mask("PEN"),
                "CLKCTRL.MCLKCTRLB.PEN",
            ),
            (
                regs::CLKCTRL_PDIV_2X,
                mclkctrlb.value("PDIV", "2X"),
                "CLKCTRL.MCLKCTRLB.PDIV=2X",
            ),
            (
                regs::PORT_PULLUPEN,
                pinctrl.mask("PULLUPEN"),
                "PORT.PINnCTRL.PULLUPEN",
            ),
            (
                regs::PORT_ISC_BOTHEDGES,
                pinctrl.value("ISC", "BOTHEDGES"),
                "PORT.PINnCTRL.ISC=BOTHEDGES",
            ),
            (regs::TCA_ENABLE, ctrla.mask("ENABLE"), "TCA0.CTRLA.ENABLE"),
            (
                regs::TCA_CLKSEL_DIV1,
                ctrla.value("CLKSEL", "DIV1"),
                "TCA0.CTRLA.CLKSEL=DIV1",
            ),
            (regs::TCA_CMP0EN, ctrlb.mask("CMP0EN"), "TCA0.CTRLB.CMP0EN"),
            (regs::TCA_CMP1EN, ctrlb.mask("CMP1EN"), "TCA0.CTRLB.CMP1EN"),
            (
                regs::TCA_WGMODE_DSBOTTOM,
                ctrlb.value("WGMODE", "DSBOTTOM"),
                "TCA0.CTRLB.WGMODE=DSBOTTOM",
            ),
            (
                regs::RTC_PITEN,
                pitctrla.mask("PITEN"),
                "RTC.PITCTRLA.PITEN",
            ),
            (
                regs::RTC_PERIOD_CYC512,
                pitctrla.value("PERIOD", "CYC512"),
                "RTC.PITCTRLA.PERIOD=CYC512",
            ),
            (
                regs::RTC_CTRLBUSY,
                pitstatus.mask("CTRLBUSY"),
                "RTC.PITSTATUS.CTRLBUSY",
            ),
            (regs::RTC_PI, pitintflags.mask("PI"), "RTC.PITINTFLAGS.PI"),
        ] {
            assert!(
                u32::from(ours) == theirs,
                "{label}: regs.rs says 0x{ours:02X}, the ATDF says 0x{theirs:02X}"
            );
        }
    }

    #[test]
    fn vector_count_matches_the_atdf() {
        assert_eq!(regs::VECTORS as u32, Atdf::load().vectors());
    }

    #[test]
    fn layout_checks_use_the_same_constants_as_the_firmware() {
        assert_eq!(crate::layout::VECTORS, regs::VECTORS, "vector count");
        assert_eq!(
            usize::from(crate::layout::CPU_CCP),
            regs::addr::CPU_CCP,
            "CPU.CCP"
        );
    }

    #[test]
    fn simulator_cpu_registers_match_the_atdf() {
        let atdf = Atdf::load();
        for (ours, name) in [
            (crate::avr::SPL, "SPL"),
            (crate::avr::SPH, "SPH"),
            (crate::avr::SREG, "SREG"),
        ] {
            let theirs = atdf.register("CPU", None, name).address;
            assert_eq!(u32::from(ours), theirs, "CPU.{name}");
        }
    }
}
