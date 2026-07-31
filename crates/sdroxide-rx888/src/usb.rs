//! USB layer: which receivers exist, getting one into its programmed state, and
//! how to reach its vendor commands.
//!
//! # Invariant: control I/O happens on the streaming thread, and only there
//!
//! [`UsbDev`] is deliberately not `Clone`, and the stream thread owns the only
//! one. nusb's `Interface` *is* `Send + Sync`, so poking the device from another
//! thread would compile. It would also interleave with the firmware's own I2C
//! transactions to the Si5351 and the tuner — the same hazard the RTL-SDR
//! backend documents. All control from outside arrives as `Ctrl` messages.
//!
//! # The two identities
//!
//! A receiver is either `04b4:00f3` (bare boot ROM, no radio behaviour) or
//! `04b4:00f1` (our firmware running). [`open`] hides the difference: it uploads
//! the image when it finds the former and returns the latter either way.

use std::time::{Duration, Instant};

use nusb::transfer::{ControlIn, ControlOut, ControlType, Recipient};
use nusb::{MaybeFuture, Speed};

use sdroxide_types::Rx888Device;

use crate::error::{Error, Result};
use crate::firmware::{self, Image};
use crate::protocol::{Cmd, EP0_MAX, PID_FX3_BOOTLOADER, PID_RX888, VID_CYPRESS};

/// The interface the FX3 exposes in both states.
const INTERFACE: u8 = 0;

/// Vendor-command timeout. The firmware answers immediately; a wedged device
/// must not hang the DSP thread.
const CTRL_TIMEOUT: Duration = Duration::from_millis(500);

/// How long to wait for the device to come back after the firmware jump. It
/// re-enumerates in well under a second; the slack is for slow hubs.
const REENUMERATE_TIMEOUT: Duration = Duration::from_secs(5);

fn is_rx888(vid: u16, pid: u16) -> bool {
    vid == VID_CYPRESS && (pid == PID_FX3_BOOTLOADER || pid == PID_RX888)
}

/// Every RX-888 on the bus, in either state.
///
/// Non-invasive: nothing here opens a device, so it is safe to call while
/// another instance is streaming.
pub fn list() -> Vec<Rx888Device> {
    let Ok(devices) = nusb::list_devices().wait() else {
        return Vec::new();
    };
    devices
        .filter(|d| is_rx888(d.vendor_id(), d.product_id()))
        .map(|d| {
            let needs_firmware = d.product_id() == PID_FX3_BOOTLOADER;
            let product = d.product_string().unwrap_or_default().trim().to_string();
            // In the boot ROM the product string is Cypress's "WestBridge",
            // which would be a baffling thing to show someone who plugged in a
            // radio. Programmed, it is "RX888mk2" and worth showing.
            let label =
                if needs_firmware || product.is_empty() { "RX-888".to_string() } else { product };
            Rx888Device {
                serial: d.serial_number().filter(|s| !s.is_empty()).map(str::to_string),
                name: label,
                needs_firmware,
                superspeed: matches!(d.speed(), Some(Speed::Super | Speed::SuperPlus)),
            }
        })
        .collect()
}

/// Open a receiver, uploading firmware first if it needs it.
///
/// `serial` pins a particular receiver; `None` takes the first one found. Note
/// that the boot ROM and the programmed firmware report *different* serials (the
/// ROM reports a short die id, the firmware a 16-hex-digit one), so a pinned
/// serial is matched against whichever state the device is currently in.
pub fn open(serial: Option<&str>, firmware_override: Option<&[u8]>) -> Result<UsbDev> {
    let want = serial.map(str::trim).filter(|s| !s.is_empty());

    let info = find(want)?;

    if info.product_id() == PID_FX3_BOOTLOADER {
        // Remember what was already programmed, so that after the jump we can
        // tell *our* device from someone else's that happened to be there.
        let before: Vec<String> = programmed_ids();

        let bytes = firmware_override.unwrap_or(firmware::BUNDLED);
        let image = Image::parse(bytes)?;
        tracing::info!(
            "RX-888 is in its boot ROM; uploading {} bytes of firmware in {} sections",
            image.payload_len(),
            image.sections.len()
        );

        let label = "RX-888 (boot ROM)";
        let device = info.open().wait().map_err(|e| Error::from_open(e, &label))?;
        let iface = device
            .detach_and_claim_interface(INTERFACE)
            .wait()
            .map_err(|e| Error::from_open(e, &label))?;
        firmware::upload(&iface, &image)?;
        // Let go before the device leaves the bus, so the handle cannot keep a
        // stale usbfs node alive across re-enumeration.
        drop(iface);
        drop(device);

        let info = wait_for_programmed(&before)?;
        return claim(info);
    }

    claim(info)
}

/// Serial numbers of every already-programmed receiver, used to spot the new
/// arrival after a firmware jump.
fn programmed_ids() -> Vec<String> {
    nusb::list_devices()
        .wait()
        .map(|ds| {
            ds.filter(|d| d.vendor_id() == VID_CYPRESS && d.product_id() == PID_RX888)
                .map(|d| d.serial_number().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn find(want: Option<&str>) -> Result<nusb::DeviceInfo> {
    let devices = nusb::list_devices().wait()?;
    let mut candidates = devices.filter(|d| is_rx888(d.vendor_id(), d.product_id()));

    match want {
        Some(s) => candidates.find(|d| d.serial_number() == Some(s)).ok_or_else(|| {
            Error::NotFound(format!(
                "no RX-888 with serial {s:?} is plugged in — pick another receiver \
                 in Settings → Radio, or clear the serial to use the first one found"
            ))
        }),
        None => {
            candidates.next().ok_or_else(|| Error::NotFound("no RX-888 found on USB".to_string()))
        }
    }
}

/// Poll for a programmed receiver that was not there before the jump.
pub fn wait_for_programmed(before: &[String]) -> Result<nusb::DeviceInfo> {
    let deadline = Instant::now() + REENUMERATE_TIMEOUT;
    // Count how many of each serial we saw before, so a second identical
    // receiver appearing is still recognised as new.
    loop {
        if let Ok(devices) = nusb::list_devices().wait() {
            let mut seen = before.to_vec();
            for d in devices {
                if d.vendor_id() != VID_CYPRESS || d.product_id() != PID_RX888 {
                    continue;
                }
                let serial = d.serial_number().unwrap_or_default().to_string();
                match seen.iter().position(|s| *s == serial) {
                    // Already accounted for before the upload; not ours.
                    Some(i) => {
                        seen.swap_remove(i);
                    }
                    None => return Ok(d),
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::FirmwareNoReenumerate);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn claim(info: nusb::DeviceInfo) -> Result<UsbDev> {
    let serial = info.serial_number().filter(|s| !s.is_empty()).map(str::to_string);
    let product = info.product_string().unwrap_or_default().trim().to_string();
    let label = if product.is_empty() { "RX-888".to_string() } else { product };
    let speed = info.speed();

    let device = info.open().wait().map_err(|e| Error::from_open(e, &label))?;
    let iface =
        device.claim_interface(INTERFACE).wait().map_err(|e| Error::from_open(e, &label))?;

    tracing::info!(
        "opened {label} (usb {:04x}:{:04x}, serial {}, link {})",
        info.vendor_id(),
        info.product_id(),
        serial.as_deref().unwrap_or("none"),
        speed_name(speed),
    );

    Ok(UsbDev { iface, label, serial, speed })
}

/// A human-readable link speed, for logs and the on-screen warning.
pub fn speed_name(speed: Option<Speed>) -> &'static str {
    match speed {
        Some(Speed::Low) => "USB 1.1 low speed",
        Some(Speed::Full) => "USB 1.1 full speed",
        Some(Speed::High) => "USB 2.0 high speed",
        Some(Speed::Super) => "USB 3.0 SuperSpeed",
        Some(Speed::SuperPlus) => "USB 3.1 SuperSpeed+",
        _ => "unknown speed",
    }
}

/// Sustained bulk throughput to plan for, in bytes per second.
///
/// Deliberately pessimistic. The theoretical ceilings are 53 MB/s and 500 MB/s;
/// what a real host sustains with other devices on the bus is a good deal less,
/// and over-promising here shows up as dropped samples rather than as an
/// honest "this link is too slow".
pub fn usable_bytes_per_sec(speed: Option<Speed>) -> f64 {
    match speed {
        Some(Speed::SuperPlus) => 800e6,
        Some(Speed::Super) => 380e6,
        Some(Speed::High) => 38e6,
        // Full/low speed cannot carry this receiver at any rate; report
        // something tiny and let the rate clamp produce the error.
        _ => 1e6,
    }
}

/// An opened, programmed receiver.
///
/// Not `Clone` on purpose — see the module invariant.
pub struct UsbDev {
    iface: nusb::Interface,
    label: String,
    serial: Option<String>,
    speed: Option<Speed>,
}

impl UsbDev {
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn serial(&self) -> Option<&str> {
        self.serial.as_deref()
    }

    pub fn speed(&self) -> Option<Speed> {
        self.speed
    }

    /// Borrow the interface so the streaming code can open the bulk endpoint.
    pub fn interface(&self) -> &nusb::Interface {
        &self.iface
    }

    /// A vendor command with no data stage, or with a small OUT payload.
    pub fn vendor_out(&self, cmd: Cmd, value: u16, index: u16, data: &[u8]) -> Result<()> {
        self.vendor_out_timeout(cmd, value, index, data, CTRL_TIMEOUT)
    }

    /// As [`UsbDev::vendor_out`], with an explicit timeout.
    ///
    /// Needed because a couple of commands restart the FX3's DMA and GPIF
    /// engines and take far longer than a register poke — see
    /// [`crate::device::Device::start`].
    pub fn vendor_out_timeout(
        &self,
        cmd: Cmd,
        value: u16,
        index: u16,
        data: &[u8],
        timeout: Duration,
    ) -> Result<()> {
        debug_assert!(data.len() <= EP0_MAX, "EP0 payloads over 64 bytes stall");
        tracing::trace!("vendor out {cmd:?} value={value:#06x} index={index:#06x} <- {data:02x?}");
        self.iface
            .control_out(
                ControlOut {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: cmd.code(),
                    value,
                    index,
                    data,
                },
                timeout,
            )
            .wait()
            .map_err(|source| Error::Transfer { op: "vendor write", source })
    }

    /// A vendor command with an IN data stage.
    pub fn vendor_in(&self, cmd: Cmd, value: u16, index: u16, len: usize) -> Result<Vec<u8>> {
        debug_assert!(len <= EP0_MAX, "EP0 payloads over 64 bytes stall");
        let data = self
            .iface
            .control_in(
                ControlIn {
                    control_type: ControlType::Vendor,
                    recipient: Recipient::Device,
                    request: cmd.code(),
                    value,
                    index,
                    length: len as u16,
                },
                CTRL_TIMEOUT,
            )
            .wait()
            .map_err(|source| Error::Transfer { op: "vendor read", source })?;
        tracing::trace!("vendor in  {cmd:?} value={value:#06x} index={index:#06x} -> {data:02x?}");
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_two_rx888_ids_are_recognised() {
        assert!(is_rx888(VID_CYPRESS, PID_FX3_BOOTLOADER));
        assert!(is_rx888(VID_CYPRESS, PID_RX888));
        // A different Cypress part, and an unrelated vendor.
        assert!(!is_rx888(VID_CYPRESS, 0x00f0));
        assert!(!is_rx888(0x0bda, PID_RX888));
    }

    #[test]
    fn a_usb2_link_cannot_carry_the_default_adc_rate() {
        // 64.8 Msps of 16-bit real samples is 129.6 MB/s. The point of the
        // clamp is that this comparison is false on High speed and true on
        // SuperSpeed — the failure the hardware on hand actually exhibits.
        let needed = 64.8e6 * 2.0;
        assert!(usable_bytes_per_sec(Some(Speed::High)) < needed);
        assert!(usable_bytes_per_sec(Some(Speed::Super)) > needed);
    }

    #[test]
    fn a_receiver_in_its_boot_rom_is_labelled_as_such() {
        let d = Rx888Device {
            serial: Some("0000000004BE".into()),
            name: "RX-888".into(),
            needs_firmware: true,
            superspeed: false,
        };
        let l = d.label();
        assert!(l.contains("0000000004BE"), "{l}");
        assert!(l.contains("firmware"), "{l}");
    }
}
