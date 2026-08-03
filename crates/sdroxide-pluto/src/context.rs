//! The IIOD context description: what `PRINT` hands back, and what it means.
//!
//! Everything this backend knows about the hardware it is talking to comes from
//! this document — which devices exist, which channels they have, which
//! attributes those channels carry, and (the part the sample decoders are built
//! from) how each scan element is laid out on the wire.
//!
//! Nothing here is hardcoded to a Pluto. That matters in two directions: a
//! stock AD9363 board and one unlocked to AD9364 differ only in the values
//! behind these attributes, and an IIO box that is *not* a Pluto is recognised
//! and named rather than driven into nonsense.
//!
//! # Ids, not names
//!
//! `iiod` accepts either a device's `id` (`iio:device0`) or its `name`
//! (`ad9361-phy`) in a command, but libiio itself always sends the id — so this
//! crate does too, resolving name → id here. Same for channels.

use std::collections::BTreeMap;

use crate::error::{Error, Result};

/// How one scan element is laid out on the wire, parsed from an IIO `format`
/// string such as `le:s12/16>>0`: little-endian, 12 significant **signed** bits
/// stored in a 16-bit container, no shift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanFormat {
    pub little_endian: bool,
    pub signed: bool,
    /// Significant bits — what sets full scale.
    pub bits: u32,
    /// Container size in bits; always a whole number of bytes in practice.
    pub storage_bits: u32,
    /// How far the significant bits sit above the bottom of the container.
    pub shift: u32,
}

impl ScanFormat {
    /// `[be|le]:[s|u]<bits>/<storage>[>><shift>]`
    pub fn parse(s: &str) -> Result<ScanFormat> {
        let bad = || Error::Xml(format!("cannot read the scan-element format {s:?}"));
        let (endian, rest) = s.split_once(':').ok_or_else(bad)?;
        let little_endian = match endian {
            "le" => true,
            "be" => false,
            _ => return Err(bad()),
        };
        let (signed, rest) = match rest.as_bytes().first() {
            Some(b's') | Some(b'S') => (true, &rest[1..]),
            Some(b'u') | Some(b'U') => (false, &rest[1..]),
            _ => return Err(bad()),
        };
        let (rest, shift) = match rest.split_once(">>") {
            Some((head, sh)) => (head, sh.trim().parse::<u32>().map_err(|_| bad())?),
            None => (rest, 0),
        };
        let (bits, storage) = rest.split_once('/').ok_or_else(bad)?;
        let bits: u32 = bits.trim().parse().map_err(|_| bad())?;
        let storage_bits: u32 = storage.trim().parse().map_err(|_| bad())?;
        if bits == 0 || bits > storage_bits || !storage_bits.is_multiple_of(8) || storage_bits > 32
        {
            return Err(Error::Xml(format!(
                "scan-element format {s:?} describes {bits} bits in a {storage_bits}-bit \
                 container, which this driver cannot decode"
            )));
        }
        if shift + bits > storage_bits {
            return Err(Error::Xml(format!(
                "scan-element format {s:?} shifts {bits} bits by {shift} out of a \
                 {storage_bits}-bit container"
            )));
        }
        Ok(ScanFormat { little_endian, signed, bits, storage_bits, shift })
    }

    /// Bytes one sample of this element occupies.
    pub fn bytes(&self) -> usize {
        (self.storage_bits / 8) as usize
    }

    /// The magnitude that maps to 1.0. Half the code range either way — an
    /// unsigned element is offset binary, so its midpoint is silence and the
    /// distance to a rail is the same number as for a signed one.
    pub fn full_scale(&self) -> f32 {
        (1u32 << (self.bits - 1)) as f32
    }

    /// Decode one sample to `-1.0..1.0`.
    pub fn decode(&self, raw: &[u8]) -> f32 {
        let mut word: u32 = 0;
        let n = self.bytes().min(raw.len());
        if self.little_endian {
            for (i, &b) in raw[..n].iter().enumerate() {
                word |= (b as u32) << (8 * i);
            }
        } else {
            for &b in &raw[..n] {
                word = (word << 8) | b as u32;
            }
        }
        word >>= self.shift;
        let mask = if self.bits >= 32 { u32::MAX } else { (1u32 << self.bits) - 1 };
        word &= mask;
        let value = if self.signed {
            // Sign-extend from the significant bit count: an AD9361 sample is
            // 12 bits inside a 16-bit slot, so `0x0fff` is -1, not +4095.
            let sign = 1u32 << (self.bits - 1);
            if word & sign != 0 { (word as i32) - (1i32 << self.bits) } else { word as i32 }
        } else {
            word as i32 - (1i32 << (self.bits - 1))
        };
        value as f32 / self.full_scale()
    }

    /// Encode `v` (clamped to `-1.0..1.0`) into `out`, which must be
    /// [`Self::bytes`] long.
    ///
    /// Full scale is one code short of the rail so that +1.0 and -1.0 are
    /// symmetric. Clamping rather than wrapping is the whole point: a wrapped
    /// sample is a full-amplitude sign flip, and a transmitter that does that
    /// on peaks splatters across the band.
    pub fn encode(&self, v: f32, out: &mut [u8]) {
        let mut scaled = (v.clamp(-1.0, 1.0) * (self.full_scale() - 1.0)).round() as i32;
        if !self.signed {
            scaled += self.full_scale() as i32;
        }
        let mask = if self.bits >= 32 { u32::MAX } else { (1u32 << self.bits) - 1 };
        let word = ((scaled as u32) & mask) << self.shift;
        let n = self.bytes();
        if self.little_endian {
            for (i, slot) in out[..n].iter_mut().enumerate() {
                *slot = (word >> (8 * i)) as u8;
            }
        } else {
            for (i, slot) in out[..n].iter_mut().enumerate() {
                *slot = (word >> (8 * (n - 1 - i))) as u8;
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct Channel {
    /// What goes on the wire (`voltage0`, `altvoltage0`).
    pub id: String,
    /// The human label the driver attached, if any (`RX_LO`, `TX_LO`).
    pub name: Option<String>,
    /// IIO direction: the AD9361's receive chain is `input`, its transmit chain
    /// `output`, and the two have channels with the same id.
    pub output: bool,
    pub attrs: Vec<String>,
    /// Position in the sample set and wire layout, for buffer channels.
    pub scan: Option<(u32, ScanFormat)>,
}

impl Channel {
    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a == name)
    }
}

#[derive(Debug, Clone)]
pub struct Device {
    /// What goes on the wire (`iio:device0`).
    pub id: String,
    /// The driver's name (`ad9361-phy`).
    pub name: String,
    pub attrs: Vec<String>,
    pub channels: Vec<Channel>,
}

impl Device {
    pub fn channel(&self, id: &str, output: bool) -> Option<&Channel> {
        self.channels.iter().find(|c| c.id == id && c.output == output)
    }

    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a == name)
    }

    /// Buffer channels in scan-element order — the order samples arrive in.
    pub fn scan_channels(&self) -> Vec<&Channel> {
        let mut v: Vec<&Channel> = self.channels.iter().filter(|c| c.scan.is_some()).collect();
        v.sort_by_key(|c| c.scan.map(|(i, _)| i).unwrap_or(u32::MAX));
        v
    }
}

/// One parsed IIOD context.
#[derive(Debug, Clone)]
pub struct Context {
    pub attrs: BTreeMap<String, String>,
    pub devices: Vec<Device>,
    /// The `description` on the root element — the device's kernel version.
    pub description: String,
}

impl Context {
    pub fn parse(xml: &str) -> Result<Context> {
        let doc = roxmltree::Document::parse(xml)
            .map_err(|e| Error::Xml(format!("cannot parse the context description: {e}")))?;
        let root = doc.root_element();
        if root.tag_name().name() != "context" {
            return Err(Error::Xml(format!(
                "the context description's root element is <{}>, not <context>",
                root.tag_name().name()
            )));
        }
        let description = root.attribute("description").unwrap_or_default().to_string();
        let mut attrs = BTreeMap::new();
        let mut devices = Vec::new();
        for node in root.children().filter(|n| n.is_element()) {
            match node.tag_name().name() {
                "context-attribute" => {
                    if let (Some(k), Some(v)) = (node.attribute("name"), node.attribute("value")) {
                        attrs.insert(k.to_string(), v.to_string());
                    }
                }
                "device" => devices.push(parse_device(node)?),
                _ => {}
            }
        }
        Ok(Context { attrs, devices, description })
    }

    /// Look a device up by driver name or by id, the two spellings `iiod`
    /// itself accepts.
    pub fn device(&self, name_or_id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.name == name_or_id || d.id == name_or_id)
    }

    /// Like [`Self::device`], but the error names what is actually present —
    /// which is what tells an operator they have pointed sdroxide at an IIO
    /// device that is not a Pluto.
    pub fn require(&self, name: &'static str, addr: &str) -> Result<&Device> {
        self.device(name).ok_or_else(|| Error::NotAPluto {
            addr: addr.to_string(),
            missing: name,
            present: self.devices.iter().map(|d| d.name.as_str()).collect::<Vec<_>>().join(", "),
        })
    }

    pub fn hw_model(&self) -> &str {
        self.attrs.get("hw_model").map(String::as_str).unwrap_or("")
    }

    pub fn hw_serial(&self) -> &str {
        self.attrs.get("hw_serial").map(String::as_str).unwrap_or("")
    }

    pub fn fw_version(&self) -> &str {
        self.attrs.get("fw_version").map(String::as_str).unwrap_or("")
    }

    /// Whether the model string says AD9364 — the 70 MHz–6 GHz part. Only a
    /// hint for the log: the actual limits come from `frequency_available`.
    pub fn is_ad9364(&self) -> bool {
        self.hw_model().contains("AD9364")
    }

    /// A compact, paste-able description for the diagnostics trace.
    pub fn summary(&self) -> String {
        let mut s = String::new();
        s.push_str(&format!("model: {}\n", self.hw_model()));
        s.push_str(&format!("firmware: {}\n", self.fw_version()));
        s.push_str(&format!("serial: {}\n", self.hw_serial()));
        s.push_str(&format!("kernel: {}\n", self.description));
        for d in &self.devices {
            let scans: Vec<String> = d
                .scan_channels()
                .iter()
                .map(|c| {
                    let (idx, f) = c.scan.expect("filtered to scan channels");
                    format!(
                        "{}[{idx}] {}:{}{}/{}>>{}",
                        c.id,
                        if f.little_endian { "le" } else { "be" },
                        if f.signed { 's' } else { 'u' },
                        f.bits,
                        f.storage_bits,
                        f.shift
                    )
                })
                .collect();
            s.push_str(&format!("device {} ({}) — {} channels", d.name, d.id, d.channels.len()));
            if !scans.is_empty() {
                s.push_str(&format!("; scan: {}", scans.join(", ")));
            }
            s.push('\n');
        }
        s
    }
}

fn parse_device(node: roxmltree::Node) -> Result<Device> {
    let id = node.attribute("id").unwrap_or_default().to_string();
    let name = node.attribute("name").unwrap_or(&id).to_string();
    let mut attrs = Vec::new();
    let mut channels = Vec::new();
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            // `debug-attribute` and `buffer-attribute` are deliberately not
            // collected: they need different commands, and this driver uses
            // neither.
            "attribute" => {
                if let Some(n) = child.attribute("name") {
                    attrs.push(n.to_string());
                }
            }
            "channel" => channels.push(parse_channel(child)?),
            _ => {}
        }
    }
    Ok(Device { id, name, attrs, channels })
}

fn parse_channel(node: roxmltree::Node) -> Result<Channel> {
    let id = node.attribute("id").unwrap_or_default().to_string();
    let name = node.attribute("name").map(str::to_string);
    let output = node.attribute("type") == Some("output");
    let mut attrs = Vec::new();
    let mut scan = None;
    for child in node.children().filter(|n| n.is_element()) {
        match child.tag_name().name() {
            "attribute" => {
                if let Some(n) = child.attribute("name") {
                    attrs.push(n.to_string());
                }
            }
            "scan-element" => {
                let index: u32 =
                    child.attribute("index").and_then(|s| s.parse().ok()).ok_or_else(|| {
                        Error::Xml(format!("channel {id} has a scan element with no index"))
                    })?;
                let format = child.attribute("format").ok_or_else(|| {
                    Error::Xml(format!("channel {id} has a scan element with no format"))
                })?;
                scan = Some((index, ScanFormat::parse(format)?));
            }
            _ => {}
        }
    }
    Ok(Channel { id, name, output, attrs, scan })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A trimmed but structurally faithful Pluto context, with the real
    /// attribute and format spellings.
    pub(crate) const PLUTO_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<context name="xml" description="Linux (none) 5.10.0 #1 SMP PREEMPT armv7l">
  <context-attribute name="hw_model" value="Analog Devices PlutoSDR Rev.B (Z7010-AD9364)" />
  <context-attribute name="hw_serial" value="10447354119600051f001b006bd6a2e4de" />
  <context-attribute name="fw_version" value="v0.39" />
  <device id="iio:device0" name="ad9361-phy">
    <channel id="voltage0" type="input">
      <attribute name="rf_port_select" filename="in_voltage0_rf_port_select" />
      <attribute name="rf_port_select_available" filename="in_voltage0_rf_port_select_available" />
      <attribute name="hardwaregain" filename="in_voltage0_hardwaregain" />
      <attribute name="hardwaregain_available" filename="in_voltage0_hardwaregain_available" />
      <attribute name="gain_control_mode" filename="in_voltage0_gain_control_mode" />
      <attribute name="rf_bandwidth" filename="in_voltage0_rf_bandwidth" />
      <attribute name="sampling_frequency" filename="in_voltage0_sampling_frequency" />
    </channel>
    <channel id="voltage0" type="output">
      <attribute name="rf_port_select" filename="out_voltage0_rf_port_select" />
      <attribute name="hardwaregain" filename="out_voltage0_hardwaregain" />
      <attribute name="rf_bandwidth" filename="out_voltage0_rf_bandwidth" />
      <attribute name="sampling_frequency" filename="out_voltage0_sampling_frequency" />
    </channel>
    <channel id="altvoltage0" name="RX_LO" type="output">
      <attribute name="frequency" filename="out_altvoltage0_RX_LO_frequency" />
      <attribute name="frequency_available" filename="out_altvoltage0_RX_LO_frequency_available" />
    </channel>
    <channel id="altvoltage1" name="TX_LO" type="output">
      <attribute name="frequency" filename="out_altvoltage1_TX_LO_frequency" />
    </channel>
    <attribute name="ensm_mode" filename="ensm_mode" />
    <attribute name="calib_mode" filename="calib_mode" />
    <debug-attribute name="adi,agc-attack-delay-us" />
  </device>
  <device id="iio:device3" name="cf-ad9361-lpc">
    <channel id="voltage0" type="input">
      <scan-element index="0" format="le:s12/16&gt;&gt;0" />
    </channel>
    <channel id="voltage1" type="input">
      <scan-element index="1" format="le:s12/16&gt;&gt;0" />
    </channel>
  </device>
  <device id="iio:device4" name="cf-ad9361-dds-core-lpc">
    <channel id="voltage0" type="output">
      <scan-element index="0" format="le:s16/16&gt;&gt;0" />
    </channel>
    <channel id="voltage1" type="output">
      <scan-element index="1" format="le:s16/16&gt;&gt;0" />
    </channel>
    <channel id="altvoltage0" name="TX1_I_F1" type="output">
      <attribute name="raw" filename="out_altvoltage0_TX1_I_F1_raw" />
    </channel>
  </device>
</context>"#;

    #[test]
    fn a_pluto_context_yields_its_three_devices() {
        let ctx = Context::parse(PLUTO_XML).expect("parse");
        assert_eq!(ctx.hw_model(), "Analog Devices PlutoSDR Rev.B (Z7010-AD9364)");
        assert_eq!(ctx.fw_version(), "v0.39");
        assert!(ctx.is_ad9364());
        // Lookup works by driver name and by id, as iiod itself accepts both.
        assert_eq!(ctx.device("ad9361-phy").unwrap().id, "iio:device0");
        assert_eq!(ctx.device("iio:device3").unwrap().name, "cf-ad9361-lpc");
        assert!(ctx.device("cf-ad9361-dds-core-lpc").is_some());
    }

    /// The AD9361's receive and transmit chains both have a channel called
    /// `voltage0`; they are told apart only by direction. Confusing them writes
    /// receive gain into the transmit attenuator.
    #[test]
    fn input_and_output_voltage0_are_different_channels() {
        let ctx = Context::parse(PLUTO_XML).expect("parse");
        let phy = ctx.device("ad9361-phy").unwrap();
        let rx = phy.channel("voltage0", false).expect("rx chain");
        let tx = phy.channel("voltage0", true).expect("tx chain");
        assert!(rx.has_attr("gain_control_mode"));
        assert!(!tx.has_attr("gain_control_mode"));
        assert!(rx.has_attr("hardwaregain_available"));
        assert!(!tx.has_attr("hardwaregain_available"));
        // The LO channels carry their human label as well as their wire id.
        let lo = phy.channel("altvoltage0", true).expect("rx lo");
        assert_eq!(lo.name.as_deref(), Some("RX_LO"));
    }

    #[test]
    fn scan_channels_come_back_in_sample_order() {
        let ctx = Context::parse(PLUTO_XML).expect("parse");
        let rx = ctx.device("cf-ad9361-lpc").unwrap();
        let scans = rx.scan_channels();
        assert_eq!(scans.len(), 2);
        assert_eq!(scans[0].id, "voltage0");
        assert_eq!(scans[1].id, "voltage1");
        assert_eq!(scans[0].scan.unwrap().1.bits, 12);
        // The transmit buffer is 16-bit, not 12 — the DAC uses the top nibble
        // of each word, so the container is full width.
        let tx = ctx.device("cf-ad9361-dds-core-lpc").unwrap();
        assert_eq!(tx.scan_channels()[0].scan.unwrap().1.bits, 16);
    }

    #[test]
    fn format_strings_parse_into_layouts() {
        let f = ScanFormat::parse("le:s12/16>>0").expect("pluto rx");
        assert_eq!(
            f,
            ScanFormat { little_endian: true, signed: true, bits: 12, storage_bits: 16, shift: 0 }
        );
        assert_eq!(f.bytes(), 2);
        assert_eq!(f.full_scale(), 2048.0);
        // Shifted and big-endian variants exist on other IIO parts.
        let g = ScanFormat::parse("be:u14/16>>2").expect("shifted");
        assert!(!g.little_endian && !g.signed && g.shift == 2);
        assert!(ScanFormat::parse("le:s12").is_err());
        assert!(ScanFormat::parse("xx:s12/16").is_err());
        assert!(ScanFormat::parse("le:s20/16").is_err());
        assert!(ScanFormat::parse("le:s12/16>>8").is_err());
    }

    /// The decoder is checked against bytes written out by hand from the wire
    /// format, not against this crate's own encoder — a self-consistent
    /// encode/decode pair passes its own test while both are wrong.
    #[test]
    fn twelve_bit_samples_decode_from_hand_written_bytes() {
        let f = ScanFormat::parse("le:s12/16>>0").expect("format");
        // 12-bit two's complement, little-endian in a 16-bit slot.
        assert_eq!(f.decode(&[0x00, 0x00]), 0.0); //     0
        assert_eq!(f.decode(&[0x01, 0x00]), 1.0 / 2048.0); //  +1
        assert_eq!(f.decode(&[0xFF, 0x0F]), -1.0 / 2048.0); //  -1 == 0x0FFF
        assert_eq!(f.decode(&[0x00, 0x08]), -1.0); // -2048 == 0x0800
        assert_eq!(f.decode(&[0xFF, 0x07]), 2047.0 / 2048.0); // +2047
        // Byte order is not a coin flip: the same bytes big-endian are not the
        // same number.
        let be = ScanFormat::parse("be:s12/16>>0").expect("format");
        assert_ne!(be.decode(&[0xFF, 0x07]), f.decode(&[0xFF, 0x07]));
    }

    #[test]
    fn sixteen_bit_transmit_samples_round_trip_through_the_wire_layout() {
        let f = ScanFormat::parse("le:s16/16>>0").expect("format");
        let mut buf = [0u8; 2];
        f.encode(1.0, &mut buf);
        assert_eq!(buf, [0xFF, 0x7F]); // +32767
        f.encode(-1.0, &mut buf);
        assert_eq!(buf, [0x01, 0x80]); // -32767, one short of the rail
        f.encode(0.0, &mut buf);
        assert_eq!(buf, [0x00, 0x00]);
        // Values past full scale are clamped, never wrapped: a wrapped sample
        // is a full-amplitude sign flip, which is a splatter of harmonics.
        f.encode(9.0, &mut buf);
        assert_eq!(buf, [0xFF, 0x7F]);
        f.encode(-9.0, &mut buf);
        assert_eq!(buf, [0x01, 0x80]);
    }

    /// An IIO device that is not a Pluto has to say so, and say what it is.
    #[test]
    fn a_non_pluto_context_names_what_it_actually_has() {
        let xml = r#"<context name="xml" description="x">
            <device id="iio:device0" name="adc081c" />
        </context>"#;
        let ctx = Context::parse(xml).expect("parse");
        let err = ctx.require("ad9361-phy", "10.0.0.9:30431").expect_err("not a pluto");
        let text = err.to_string();
        assert!(text.contains("not a PlutoSDR"), "{text}");
        assert!(text.contains("ad9361-phy"), "{text}");
        assert!(text.contains("adc081c"), "{text}");
    }

    #[test]
    fn the_summary_carries_the_scan_formats_a_reader_needs() {
        let ctx = Context::parse(PLUTO_XML).expect("parse");
        let s = ctx.summary();
        assert!(s.contains("v0.39"), "{s}");
        assert!(s.contains("voltage0[0] le:s12/16>>0"), "{s}");
        assert!(s.contains("cf-ad9361-dds-core-lpc"), "{s}");
    }
}
