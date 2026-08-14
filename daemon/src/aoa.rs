//! Milestone 8: Android Open Accessory (AOA) transport -- talks to the
//! tablet directly over raw USB bulk transfers via `rusb`/libusb, bypassing
//! adb entirely (no adb-forward relay hop through the host's adb server and
//! the device's adbd).
//!
//! AOA handshake (standard protocol, not Samsung-specific): send a vendor
//! control request asking the device's protocol version, send six
//! identification strings, then a control request telling the device to
//! switch into accessory mode. The device then disconnects and
//! re-enumerates under Google's AOA vendor/product ID -- we have to find
//! and open *that* new device, not the original one.
//!
//! These six strings are the pairing identity: the Android app's
//! `accessory_filter.xml` manifest resource must match manufacturer/model
//! (version/uri/serial are informational) for Android to route the
//! USB_ACCESSORY_ATTACHED intent to our app at all.

use std::io::{self, Read, Write};
use std::sync::Arc;
use std::time::{Duration, Instant};

const GOOGLE_VID: u16 = 0x18D1;
const AOA_PID_ACCESSORY: u16 = 0x2D00;
const AOA_PID_ACCESSORY_ADB: u16 = 0x2D01;

const ACCESSORY_GET_PROTOCOL: u8 = 51;
const ACCESSORY_SEND_STRING: u8 = 52;
const ACCESSORY_START: u8 = 53;

const MANUFACTURER: &str = "Quill";
const MODEL: &str = "Quill Virtual Display";
const DESCRIPTION: &str = "Quill USB display + pen transport";
const VERSION: &str = "1.0";

// Separate read/write timeouts, not one shared value: reads (particularly
// the very first one, waiting for Android's handshake) need to tolerate
// real human-interaction delay -- Android cold-start plus the USB
// accessory permission dialog can easily take way longer than a
// reasonable single bulk-transfer timeout would otherwise be. Confirmed
// live: a single 5s timeout used for both caused the input thread to give
// up permanently after 5s with nothing to retry it, even though the
// higher-level clock-sync wait (see portal_capture.rs) was patient for
// much longer. Writes don't have this problem -- if nobody's reading,
// failing reasonably fast is fine (and is what actually happened safely).
const READ_TIMEOUT: Duration = Duration::from_secs(90);
const WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(2);
const REENUMERATE_TIMEOUT: Duration = Duration::from_secs(10);

/// libusb's synchronous transfer functions take `&self` (libusb itself is
/// thread-safe), so the handle is shared via `Arc` between a read half and
/// a write half that can each live on a different thread -- the same shape
/// as `TcpStream::try_clone()`, which the rest of the transport code
/// (`portal_capture.rs`, `input_receiver.rs`) was already written around.
struct Shared {
    handle: rusb::DeviceHandle<rusb::GlobalContext>,
    interface: u8,
}

pub struct AoaTransport {
    shared: Arc<Shared>,
    ep_in: u8,
    ep_out: u8,
}

pub struct AoaReader {
    shared: Arc<Shared>,
    ep_in: u8,
}

pub struct AoaWriter {
    shared: Arc<Shared>,
    ep_out: u8,
}

impl AoaTransport {
    /// Consumes `self`, splitting into an independent read half and write
    /// half sharing the same underlying USB handle via `Arc`, for use on
    /// separate threads -- each half keeps the handle alive on its own, so
    /// there's nothing left to leak or explicitly keep around afterward.
    pub fn split(self) -> (AoaReader, AoaWriter) {
        (
            AoaReader { shared: self.shared.clone(), ep_in: self.ep_in },
            AoaWriter { shared: self.shared, ep_out: self.ep_out },
        )
    }
}

fn send_string(
    handle: &rusb::DeviceHandle<rusb::GlobalContext>,
    index: u16,
    value: &str,
) -> rusb::Result<()> {
    let mut bytes = value.as_bytes().to_vec();
    bytes.push(0); // AOA strings are null-terminated
    handle.write_control(0x40, ACCESSORY_SEND_STRING, 0, index, &bytes, CONTROL_TIMEOUT)?;
    Ok(())
}

/// Scans every USB device for one that answers the AOA `GET_PROTOCOL`
/// vendor request, switches it into accessory mode, waits for it to
/// re-enumerate under Google's AOA VID/PID, and opens that new device --
/// generic device discovery, not hardcoded to this tablet's own VID/PID
/// (matches the project's no-hardcoding rule: any AOA-capable Android
/// device should work here unmodified).
pub fn connect() -> Result<AoaTransport, String> {
    // Bounded retry, not a single instant scan-and-fail: confirmed live,
    // this matters. A USB replug takes the tablet a few seconds to work
    // through disconnect -> re-enumerate-as-MTP before it's visible here at
    // all, and now that a failed connect() is fatal (see `setup_transport`
    // in portal_capture.rs -- exits so systemd can retry, rather than
    // silently running forever with a dead transport), an instant failure
    // right after a replug raced systemd's restart against the tablet still
    // mid-enumeration, burned through `StartLimitBurst` in a few hundred
    // milliseconds, and left the unit permanently `failed`, needing a
    // manual `systemctl --user reset-failed`. Retrying the scan here for a
    // while first absorbs that normal enumeration delay internally instead
    // of turning it into a crash loop.
    const DEVICE_SCAN_TIMEOUT: Duration = Duration::from_secs(20);
    let scan_deadline = Instant::now() + DEVICE_SCAN_TIMEOUT;

    loop {
        // Already switched from a previous run (device stays in accessory
        // mode across daemon restarts until physically replugged) -- use it
        // directly instead of trying the switch handshake again, which a
        // device already in accessory mode won't answer the same way.
        if let Some(t) = try_open_accessory() {
            eprintln!("[aoa] device already in accessory mode, reusing it");
            return Ok(t);
        }

        let devices = rusb::devices().map_err(|e| format!("rusb::devices: {e}"))?;

        let mut switched_any = false;
        for device in devices.iter() {
            let Ok(handle) = device.open() else { continue };

            let mut protocol_buf = [0u8; 2];
            let protocol = handle.read_control(0xC0, ACCESSORY_GET_PROTOCOL, 0, 0, &mut protocol_buf, CONTROL_TIMEOUT);
            let Ok(n) = protocol else { continue };
            if n != 2 {
                continue;
            }
            let version = u16::from_le_bytes(protocol_buf);
            if version == 0 {
                continue; // doesn't support AOA
            }
            eprintln!(
                "[aoa] found AOA-capable device (protocol v{version}) at bus {} addr {}, switching to accessory mode...",
                device.bus_number(),
                device.address()
            );

            send_string(&handle, 0, MANUFACTURER).map_err(|e| format!("send manufacturer: {e}"))?;
            send_string(&handle, 1, MODEL).map_err(|e| format!("send model: {e}"))?;
            send_string(&handle, 2, DESCRIPTION).map_err(|e| format!("send description: {e}"))?;
            send_string(&handle, 3, VERSION).map_err(|e| format!("send version: {e}"))?;
            send_string(&handle, 4, "").map_err(|e| format!("send uri: {e}"))?;
            send_string(&handle, 5, "").map_err(|e| format!("send serial: {e}"))?;

            handle
                .write_control(0x40, ACCESSORY_START, 0, 0, &[], CONTROL_TIMEOUT)
                .map_err(|e| format!("ACCESSORY_START: {e}"))?;

            switched_any = true;
            break;
        }

        if switched_any {
            break;
        }

        if Instant::now() > scan_deadline {
            return Err(format!(
                "no AOA-capable USB device found within {DEVICE_SCAN_TIMEOUT:?} (is the tablet connected and USB debugging/accessory mode available?)"
            ));
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    eprintln!("[aoa] waiting for device to re-enumerate as an accessory...");
    let deadline = Instant::now() + REENUMERATE_TIMEOUT;
    loop {
        if Instant::now() > deadline {
            return Err("timed out waiting for AOA re-enumeration".into());
        }
        if let Some(t) = try_open_accessory() {
            return Ok(t);
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn try_open_accessory() -> Option<AoaTransport> {
    let devices = rusb::devices().ok()?;
    for device in devices.iter() {
        // Each `continue` here skips to the next device on any failure --
        // one device that can't be inspected shouldn't abort the whole scan.
        let Ok(desc) = device.device_descriptor() else { continue };
        if desc.vendor_id() != GOOGLE_VID {
            continue;
        }
        if desc.product_id() != AOA_PID_ACCESSORY && desc.product_id() != AOA_PID_ACCESSORY_ADB {
            continue;
        }

        let Ok(handle) = device.open() else { continue };
        let Ok(config) = device.active_config_descriptor() else { continue };
        let Some(interface) = config.interfaces().next() else { continue };
        let Some(interface_desc) = interface.descriptors().next() else { continue };

        let mut ep_in = None;
        let mut ep_out = None;
        for ep in interface_desc.endpoint_descriptors() {
            if ep.transfer_type() != rusb::TransferType::Bulk {
                continue;
            }
            match ep.direction() {
                rusb::Direction::In => ep_in = Some(ep.address()),
                rusb::Direction::Out => ep_out = Some(ep.address()),
            }
        }
        let (Some(ep_in), Some(ep_out)) = (ep_in, ep_out) else {
            continue;
        };

        let interface_num = interface_desc.interface_number();
        if handle.claim_interface(interface_num).is_err() {
            continue;
        }

        eprintln!(
            "[aoa] connected: bus {} addr {}, interface {interface_num}, bulk in=0x{ep_in:02x} out=0x{ep_out:02x}",
            device.bus_number(),
            device.address()
        );

        return Some(AoaTransport {
            shared: Arc::new(Shared { handle, interface: interface_num }),
            ep_in,
            ep_out,
        });
    }
    None
}

impl Read for AoaReader {
    /// Loops on a USB bulk timeout instead of surfacing it as an error --
    /// confirmed live: `input_receiver.rs`'s steady-state loop reads one
    /// event at a time and treats any read error as fatal (stream ended,
    /// thread exits for good, no retry). A `rusb` bulk-read timeout just
    /// means "no data arrived within `READ_TIMEOUT`", which is completely
    /// normal during any idle stretch with no pen/finger input -- a real
    /// TCP socket blocks indefinitely in the same situation rather than
    /// erroring, so this makes AOA behave the same way rather than killing
    /// the input thread after 90 seconds of nobody touching the screen.
    /// Genuine failures (device unplugged, etc.) come back as a different
    /// `rusb::Error` variant and still propagate normally.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            match self.shared.handle.read_bulk(self.ep_in, buf, READ_TIMEOUT) {
                Ok(n) => return Ok(n),
                Err(rusb::Error::Timeout) => continue,
                Err(e) => return Err(io::Error::other(format!("AOA bulk read: {e}"))),
            }
        }
    }
}

impl Write for AoaWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.shared
            .handle
            .write_bulk(self.ep_out, buf, WRITE_TIMEOUT)
            .map_err(|e| io::Error::other(format!("AOA bulk write: {e}")))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(()) // bulk transfers are unbuffered, nothing to flush
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.interface);
    }
}
