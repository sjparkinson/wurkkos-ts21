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

/// The vendored ATtiny1616 ATDF.
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
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../vendor/ATtiny1616.atdf");
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

    /// The vector number of `instance`'s interrupt `name`, such as
    /// `vector("RTC", "PIT")`.
    pub fn vector(&self, instance: &str, name: &str) -> u32 {
        self.device()
            .child("interrupts")
            .all("interrupt")
            .find(|i| i.attr("module-instance") == Some(instance) && i.attr("name") == Some(name))
            .unwrap_or_else(|| panic!("no interrupt {instance}.{name}"))
            .num("index")
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
            (regs::addr::CPU_SPL, "CPU", None, "SPL", 1),
            (regs::addr::CPU_SPH, "CPU", None, "SPH", 1),
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
            (regs::addr::TCA0_CMP0, "TCA0", single, "CMP0", 2),
            (regs::addr::TCA0_CMP1, "TCA0", single, "CMP1", 2),
            (regs::addr::TCA0_PERBUF, "TCA0", single, "PERBUF", 2),
            (regs::addr::TCA0_CMP0BUF, "TCA0", single, "CMP0BUF", 2),
            (regs::addr::TCA0_CMP1BUF, "TCA0", single, "CMP1BUF", 2),
            (regs::addr::VREF_CTRLA, "VREF", None, "CTRLA", 1),
            (regs::addr::VREF_CTRLB, "VREF", None, "CTRLB", 1),
            (regs::addr::WDT_CTRLA, "WDT", None, "CTRLA", 1),
            (regs::addr::WDT_STATUS, "WDT", None, "STATUS", 1),
            (regs::addr::ADC0_CALIB, "ADC0", None, "CALIB", 1),
            (regs::addr::ADC0_CTRLA, "ADC0", None, "CTRLA", 1),
            (regs::addr::ADC0_CTRLB, "ADC0", None, "CTRLB", 1),
            (regs::addr::ADC0_CTRLC, "ADC0", None, "CTRLC", 1),
            (regs::addr::ADC0_CTRLD, "ADC0", None, "CTRLD", 1),
            (regs::addr::ADC0_SAMPCTRL, "ADC0", None, "SAMPCTRL", 1),
            (regs::addr::ADC0_MUXPOS, "ADC0", None, "MUXPOS", 1),
            (regs::addr::ADC0_COMMAND, "ADC0", None, "COMMAND", 1),
            (regs::addr::ADC0_INTFLAGS, "ADC0", None, "INTFLAGS", 1),
            (regs::addr::ADC0_RES, "ADC0", None, "RES", 2),
            (
                regs::addr::SIGROW_TEMPSENSE0,
                "SIGROW",
                None,
                "TEMPSENSE0",
                1,
            ),
            (
                regs::addr::SIGROW_TEMPSENSE1,
                "SIGROW",
                None,
                "TEMPSENSE1",
                1,
            ),
            // Interrupts and sleep.
            (regs::addr::GPIO_GPIOR0, "GPIO", None, "GPIOR0", 1),
            (regs::addr::SLPCTRL_CTRLA, "SLPCTRL", None, "CTRLA", 1),
            (regs::addr::RTC_PITINTCTRL, "RTC", None, "PITINTCTRL", 1),
            (regs::addr::PORTA_INTFLAGS, "PORTA", None, "INTFLAGS", 1),
            (regs::addr::CRCSCAN_CTRLA, "CRCSCAN", None, "CTRLA", 1),
            (regs::addr::CRCSCAN_CTRLB, "CRCSCAN", None, "CTRLB", 1),
            (regs::addr::CRCSCAN_STATUS, "CRCSCAN", None, "STATUS", 1),
            (regs::addr::FUSE_WDTCFG, "FUSE", None, "WDTCFG", 1),
            (regs::addr::FUSE_OSCCFG, "FUSE", None, "OSCCFG", 1),
            (regs::addr::FUSE_SYSCFG0, "FUSE", None, "SYSCFG0", 1),
            (regs::addr::FUSE_APPEND, "FUSE", None, "APPEND", 1),
            (regs::addr::FUSE_BOOTEND, "FUSE", None, "BOOTEND", 1),
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
        let sreg = atdf.register("CPU", None, "SREG");
        let mclkctrlb = atdf.register("CLKCTRL", None, "MCLKCTRLB");
        let pinctrl = atdf.register("PORTA", None, "PIN0CTRL");
        let ctrla = atdf.register("TCA0", Some("SINGLE"), "CTRLA");
        let ctrlb = atdf.register("TCA0", Some("SINGLE"), "CTRLB");
        let pitctrla = atdf.register("RTC", None, "PITCTRLA");
        let pitstatus = atdf.register("RTC", None, "PITSTATUS");
        let pitintflags = atdf.register("RTC", None, "PITINTFLAGS");
        let vref = atdf.register("VREF", None, "CTRLA");
        let adc_ctrla = atdf.register("ADC0", None, "CTRLA");
        let adc_ctrlb = atdf.register("ADC0", None, "CTRLB");
        let adc_ctrlc = atdf.register("ADC0", None, "CTRLC");
        let adc_ctrld = atdf.register("ADC0", None, "CTRLD");
        let adc_muxpos = atdf.register("ADC0", None, "MUXPOS");
        let adc_command = atdf.register("ADC0", None, "COMMAND");
        let adc_intflags = atdf.register("ADC0", None, "INTFLAGS");
        let wdt_ctrla = atdf.register("WDT", None, "CTRLA");
        let wdt_status = atdf.register("WDT", None, "STATUS");
        let crc_ctrla = atdf.register("CRCSCAN", None, "CTRLA");
        let crc_ctrlb = atdf.register("CRCSCAN", None, "CTRLB");
        let crc_status = atdf.register("CRCSCAN", None, "STATUS");
        let osccfg = atdf.register("FUSE", None, "OSCCFG");
        let syscfg0 = atdf.register("FUSE", None, "SYSCFG0");

        // SAMPLEN is a count, not a named value: it only has to fit.
        let samplen = atdf.register("ADC0", None, "SAMPCTRL").mask("SAMPLEN");
        assert_eq!(
            u32::from(regs::ADC_SAMPLEN_32US) & !samplen,
            0,
            "ADC0.SAMPCTRL.SAMPLEN={} spills out of its mask {samplen:#04x}",
            regs::ADC_SAMPLEN_32US
        );

        for (ours, theirs, label) in [
            (
                regs::CPU_CCP_IOREG,
                ccp.value("CCP", "IOREG"),
                "CPU.CCP=IOREG",
            ),
            (regs::CPU_SREG_I, sreg.mask("I"), "CPU.SREG.I"),
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
                regs::WDT_PERIOD_32CLK,
                wdt_ctrla.value("PERIOD", "32CLK"),
                "WDT.CTRLA.PERIOD=32CLK",
            ),
            (
                regs::WDT_WINDOW_32CLK,
                wdt_ctrla.value("WINDOW", "32CLK"),
                "WDT.CTRLA.WINDOW=32CLK",
            ),
            (
                regs::WDT_SYNCBUSY,
                wdt_status.mask("SYNCBUSY"),
                "WDT.STATUS.SYNCBUSY",
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
            (
                regs::PORT_ISC_INPUT_DISABLE,
                pinctrl.value("ISC", "INPUT_DISABLE"),
                "PORT.PINnCTRL.ISC=INPUT_DISABLE",
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
            (
                regs::VREF_ADC0REFSEL_1V1,
                vref.value("ADC0REFSEL", "1V1"),
                "VREF.CTRLA.ADC0REFSEL=1V1",
            ),
            (
                regs::VREF_ADC0REFSEL_1V5,
                vref.value("ADC0REFSEL", "1V5"),
                "VREF.CTRLA.ADC0REFSEL=1V5",
            ),
            (
                regs::VREF_ADC0REFEN,
                atdf.register("VREF", None, "CTRLB").mask("ADC0REFEN"),
                "VREF.CTRLB.ADC0REFEN",
            ),
            (
                regs::ADC_DUTYCYC_DUTY25,
                atdf.register("ADC0", None, "CALIB")
                    .value("DUTYCYC", "DUTY25"),
                "ADC0.CALIB.DUTYCYC=DUTY25",
            ),
            (
                regs::ADC_ENABLE,
                adc_ctrla.mask("ENABLE"),
                "ADC0.CTRLA.ENABLE",
            ),
            (
                regs::ADC_SAMPNUM_ACC1,
                adc_ctrlb.value("SAMPNUM", "ACC1"),
                "ADC0.CTRLB.SAMPNUM=ACC1",
            ),
            (
                regs::ADC_SAMPNUM_ACC4,
                adc_ctrlb.value("SAMPNUM", "ACC4"),
                "ADC0.CTRLB.SAMPNUM=ACC4",
            ),
            (
                regs::ADC_PRESC_DIV16,
                adc_ctrlc.value("PRESC", "DIV16"),
                "ADC0.CTRLC.PRESC=DIV16",
            ),
            (
                regs::ADC_REFSEL_INTREF,
                adc_ctrlc.value("REFSEL", "INTREF"),
                "ADC0.CTRLC.REFSEL=INTREF",
            ),
            (
                regs::ADC_REFSEL_VDDREF,
                adc_ctrlc.value("REFSEL", "VDDREF"),
                "ADC0.CTRLC.REFSEL=VDDREF",
            ),
            (
                regs::ADC_SAMPCAP,
                adc_ctrlc.mask("SAMPCAP"),
                "ADC0.CTRLC.SAMPCAP",
            ),
            (
                regs::ADC_INITDLY_DLY32,
                adc_ctrld.value("INITDLY", "DLY32"),
                "ADC0.CTRLD.INITDLY=DLY32",
            ),
            (
                regs::ADC_MUXPOS_INTREF,
                adc_muxpos.value("MUXPOS", "INTREF"),
                "ADC0.MUXPOS=INTREF",
            ),
            (
                regs::ADC_MUXPOS_TEMPSENSE,
                adc_muxpos.value("MUXPOS", "TEMPSENSE"),
                "ADC0.MUXPOS=TEMPSENSE",
            ),
            (
                regs::ADC_STCONV,
                adc_command.mask("STCONV"),
                "ADC0.COMMAND.STCONV",
            ),
            (
                regs::ADC_RESRDY,
                adc_intflags.mask("RESRDY"),
                "ADC0.INTFLAGS.RESRDY",
            ),
            // Interrupts and sleep.
            (
                regs::RTC_PI,
                atdf.register("RTC", None, "PITINTCTRL").mask("PI"),
                "RTC.PITINTCTRL.PI",
            ),
            (
                regs::PORT_INT_ALL,
                atdf.register("PORTA", None, "INTFLAGS").mask("INT"),
                "PORT.INTFLAGS.INT",
            ),
            (
                regs::SLPCTRL_SEN,
                atdf.register("SLPCTRL", None, "CTRLA").mask("SEN"),
                "SLPCTRL.CTRLA.SEN",
            ),
            (
                regs::SLPCTRL_SMODE_IDLE,
                atdf.register("SLPCTRL", None, "CTRLA")
                    .value("SMODE", "IDLE"),
                "SLPCTRL.CTRLA.SMODE=IDLE",
            ),
            (
                regs::SLPCTRL_SMODE_PDOWN,
                atdf.register("SLPCTRL", None, "CTRLA")
                    .value("SMODE", "PDOWN"),
                "SLPCTRL.CTRLA.SMODE=PDOWN",
            ),
            (
                regs::CRCSCAN_ENABLE,
                crc_ctrla.mask("ENABLE"),
                "CRCSCAN.CTRLA.ENABLE",
            ),
            (
                regs::CRCSCAN_SRC_FLASH,
                crc_ctrlb.value("SRC", "FLASH"),
                "CRCSCAN.CTRLB.SRC=FLASH",
            ),
            (
                regs::CRCSCAN_MODE_PRIORITY,
                crc_ctrlb.value("MODE", "PRIORITY"),
                "CRCSCAN.CTRLB.MODE=PRIORITY",
            ),
            (
                regs::CRCSCAN_BUSY,
                crc_status.mask("BUSY"),
                "CRCSCAN.STATUS.BUSY",
            ),
            (regs::CRCSCAN_OK, crc_status.mask("OK"), "CRCSCAN.STATUS.OK"),
            (
                regs::FUSE_FREQSEL,
                osccfg.mask("FREQSEL"),
                "FUSE.OSCCFG.FREQSEL",
            ),
            (
                regs::FUSE_FREQSEL_20MHZ,
                osccfg.value("FREQSEL", "20MHZ"),
                "FUSE.OSCCFG.FREQSEL=20MHZ",
            ),
            (
                regs::FUSE_OSCLOCK,
                osccfg.mask("OSCLOCK"),
                "FUSE.OSCCFG.OSCLOCK",
            ),
            (
                regs::FUSE_RSTPINCFG,
                syscfg0.mask("RSTPINCFG"),
                "FUSE.SYSCFG0.RSTPINCFG",
            ),
            (
                regs::FUSE_RSTPINCFG_UPDI,
                syscfg0.value("RSTPINCFG", "UPDI"),
                "FUSE.SYSCFG0.RSTPINCFG=UPDI",
            ),
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
    fn vector_numbers_match_the_atdf() {
        let atdf = Atdf::load();
        for (ours, instance, name) in [
            (regs::VECTOR_PORTA_PORT, "PORTA", "PORT"),
            (regs::VECTOR_RTC_PIT, "RTC", "PIT"),
            (crate::sim::VECTOR_PORTB_PORT, "PORTB", "PORT"),
            (crate::sim::VECTOR_PORTC_PORT, "PORTC", "PORT"),
        ] {
            assert_eq!(
                ours as u32,
                atdf.vector(instance, name),
                "{instance}.{name}"
            );
        }
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
    fn sram_matches_the_atdf() {
        let sram = Atdf::load().segment("INTERNAL_SRAM");
        let start = crate::elf::DATA_SPACE + sram.start;
        assert_eq!(crate::elf::RAM, start..start + sram.size, "elf::RAM");
        assert_eq!(
            (regs::addr::SRAM_START, regs::addr::SRAM_END),
            (sram.start as usize, (sram.start + sram.size) as usize),
            "regs::addr::SRAM_START and SRAM_END"
        );
    }

    /// link.x's MEMORY regions, and every other address in it, against the
    /// ATDF. 0x800000 (`elf::DATA_SPACE`) is where AVR ELF files put data
    /// space, a toolchain convention the ATDF doesn't record.
    #[test]
    fn linker_script_matches_the_atdf() {
        let atdf = Atdf::load();
        let data = crate::elf::DATA_SPACE;
        let flash = atdf.segment("PROGMEM");
        let mapped = atdf.segment("MAPPED_PROGMEM");
        let sram = atdf.segment("INTERNAL_SRAM");

        let script = link_x();
        let (memory, sections) = linker_script(&script);
        assert_eq!(
            memory,
            [
                ("FLASH".into(), flash.start, flash.size),
                ("MAPPED".into(), data + mapped.start, mapped.size),
                ("RAM".into(), data + sram.start, sram.size),
            ],
            "link.x MEMORY"
        );
        // SECTIONS turns flash addresses into mapped ones, as ELF addresses
        // for .rodata and as data-space ones for __data_load.
        for n in sections {
            assert!(
                n == data + mapped.start || n == mapped.start,
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

    #[test]
    fn simulator_watchdog_constants_match_the_atdf() {
        let atdf = Atdf::load();
        let status = atdf.register("WDT", None, "STATUS");
        assert_eq!(
            u32::from(crate::sim::WDT_LOCK),
            status.mask("LOCK"),
            "WDT.STATUS.LOCK"
        );
        let ctrla = atdf.register("WDT", None, "CTRLA");
        assert_eq!(
            u32::from(crate::sim::WDT_PERIOD),
            ctrla.mask("PERIOD"),
            "WDT.CTRLA.PERIOD"
        );
        assert_eq!(
            u32::from(crate::sim::WDT_PERIOD_MAX),
            ctrla.value("PERIOD", "8KCLK"),
            "WDT.CTRLA.PERIOD=8KCLK"
        );
        assert_eq!(
            u32::from(crate::sim::WDT_WINDOW),
            ctrla.mask("WINDOW"),
            "WDT.CTRLA.WINDOW"
        );
        // WINDOW takes the same values as PERIOD, which the simulator relies
        // on to time both with one function.
        for value in ["8CLK", "32CLK", "8KCLK"] {
            assert_eq!(
                ctrla.value("WINDOW", value),
                ctrla.value("PERIOD", value) << 4,
                "WDT.CTRLA.WINDOW={value}"
            );
        }
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

    #[test]
    fn simulator_pin_sense_values_match_the_atdf() {
        use crate::sim;
        let atdf = Atdf::load();
        let pinctrl = atdf.register("PORTA", None, "PIN0CTRL");
        assert_eq!(u32::from(sim::ISC), pinctrl.mask("ISC"), "ISC mask");
        for (ours, name) in [
            (sim::ISC_INTDISABLE, "INTDISABLE"),
            (sim::ISC_BOTHEDGES, "BOTHEDGES"),
            (sim::ISC_RISING, "RISING"),
            (sim::ISC_FALLING, "FALLING"),
            (sim::ISC_INPUT_DISABLE, "INPUT_DISABLE"),
            (sim::ISC_LEVEL, "LEVEL"),
        ] {
            assert_eq!(u32::from(ours), pinctrl.value("ISC", name), "ISC={name}");
        }
        assert_eq!(u32::from(sim::PORT_INVEN), pinctrl.mask("INVEN"), "INVEN");
    }
}
